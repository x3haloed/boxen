#!/usr/bin/env python3
"""Authenticated HTTP watcher for one local visual sensor."""
import argparse
from collections import OrderedDict, deque
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import hmac
import json
from pathlib import Path
import signal
import ssl
import subprocess
import threading
import time
from urllib.parse import unquote, urlsplit
from uuid import uuid4

TTL = 120
IMAGE_BYTES = 64 * 1024 * 1024
SAMPLE_IMAGE_BYTES = 16 * 1024 * 1024
STALE_AFTER = 20
STARTUP_TIMEOUT = 90
HISTORY = 64
MAX_CONNECTIONS = 12


class Feed:
    def __init__(self, command):
        self.command = command
        self.changed = threading.Condition()
        self.epoch = uuid4().hex
        self.sequence = 0
        self.events = deque(maxlen=HISTORY)
        self.images = OrderedDict()
        self.image_bytes = 0
        self.snapshot = None
        self.last_sample = None
        self.process = None
        self.capture_failure = None
        self.fresh = None
        self.started = 0
        self.next_fresh = 0
        self.stopping = False

    def health(self):
        if self.process is None:
            state = self.capture_failure or "capture_unavailable"
        elif self.last_sample is None:
            state = "starting"
        elif time.monotonic() - self.last_sample >= STALE_AFTER:
            state = "capture_stalled"
        else:
            state = "sampling"
        return {
            "state": state,
            "lastCaptureMs": self.snapshot["capturedAtUnixMs"] if self.snapshot else None,
        }

    def reap(self):
        while self.images:
            _, (deadline, _, size, _) = next(iter(self.images.items()))
            if deadline > time.monotonic() and self.image_bytes <= IMAGE_BYTES:
                break
            self.images.popitem(last=False)
            self.image_bytes -= size

    def read_samples(self, process):
        stream = process.stdout
        while line := stream.readline(128 * 1024 + 1):
            if len(line) > 128 * 1024:
                raise ValueError("sample header too large")
            sample = json.loads(line)
            observation = sample["observation"]
            key = (observation["session"], observation["frame"])
            images = {}
            total = 0
            for item in sample["images"]:
                size = item["size"]
                total += size
                if size <= 0 or total > SAMPLE_IMAGE_BYTES:
                    raise ValueError("sample images exceed limit")
                body = stream.read(size)
                if len(body) != size:
                    return
                images[item.get("field")] = body

            with self.changed:
                self.snapshot = observation
                self.last_sample = time.monotonic()
                captured = observation["capturedAtUnixMs"]
                if images:
                    deadline = self.last_sample + TTL
                    self.images[key] = (deadline, captured, total, images)
                    self.image_bytes += total
                    self.reap()
                if sample["changes"]:
                    self.sequence += 1
                    event = {
                        "session": key[0], "frame": key[1], "capturedAtUnixMs": captured,
                        "changes": sample["changes"], "crops": [field for field in images if field is not None],
                        "wholeFrame": None in images,
                        "evidenceTtlSeconds": TTL,
                    }
                    self.events.append((self.sequence, "change", event))
                if sample["fresh"] and self.fresh is not None:
                    self.fresh["result"] = (key, captured, images)
                    self.fresh = None
                self.changed.notify_all()

    def supervise(self):
        while True:
            with self.changed:
                if self.stopping:
                    return
            try:
                process = subprocess.Popen(self.command, stdin=subprocess.PIPE, stdout=subprocess.PIPE)
            except OSError:
                with self.changed:
                    self.capture_failure = "capture_unavailable"
                    self.changed.notify_all()
                time.sleep(3)
                continue
            with self.changed:
                if self.stopping:
                    process.terminate()
                self.process = process
                self.last_sample = None
                self.started = time.monotonic()
                self.capture_failure = None
                self.changed.notify_all()
            try:
                self.read_samples(process)
            except (OSError, ValueError, KeyError, TypeError):
                print("visual watcher: capture stream failed", flush=True)
            finally:
                if process.poll() is None:
                    process.kill()
                process.wait()
                process.stdin.close()
                process.stdout.close()
                with self.changed:
                    self.process = None
                    self.fresh = None
                    if self.last_sample is not None:
                        self.sequence += 1
                        self.events.append((self.sequence, "gap", {"reason": "capture_interrupted"}))
                    self.changed.notify_all()
            time.sleep(3)

    def maintain(self):
        while True:
            with self.changed:
                if self.stopping:
                    return
                self.reap()
                if self.process is not None:
                    baseline = self.last_sample if self.last_sample is not None else self.started
                    timeout = STALE_AFTER if self.last_sample is not None else STARTUP_TIMEOUT
                    if time.monotonic() - baseline >= timeout:
                        self.capture_failure = "capture_stalled"
                        self.process.kill()
                self.changed.wait(timeout=1)

    def start(self):
        threading.Thread(target=self.supervise, daemon=True).start()
        threading.Thread(target=self.maintain, daemon=True).start()

    def stop(self):
        with self.changed:
            self.stopping = True
            if self.process is not None:
                self.process.kill()
            self.changed.notify_all()

    def capture(self, field):
        with self.changed:
            if field is not None and self.snapshot is not None:
                if all(entry["id"] != field for entry in self.snapshot["fields"]):
                    return 404, None
            if self.fresh is not None or time.monotonic() < self.next_fresh:
                return 429, None
            if self.health()["state"] != "sampling":
                return 503, None
            request = {}
            self.fresh = request
            self.next_fresh = time.monotonic() + 2
            try:
                self.process.stdin.write(b"F")
                self.process.stdin.flush()
            except OSError:
                self.fresh = None
                return 503, None
            self.changed.wait_for(lambda: self.fresh is not request, timeout=8)
            result = request.get("result")
            if result is None:
                # The producer may still fulfill it. A later request cannot claim that image.
                return 503, None
            key, captured, images = result
            body = images.get(field)
            return (200, (key, captured, body)) if body else (410, None)

class WatchServer(ThreadingHTTPServer):
    request_queue_size = MAX_CONNECTIONS

    def __init__(self, address, feed, credentials, tls):
        self.feed = feed
        self.credentials = credentials
        self.tls = tls
        self.slots = threading.BoundedSemaphore(MAX_CONNECTIONS)
        self.streams = threading.BoundedSemaphore(MAX_CONNECTIONS - 4)
        super().__init__(address, WatchRequest)

    def process_request(self, request, address):
        if not self.slots.acquire(blocking=False):
            request.close()
            return
        try:
            super().process_request(request, address)
        except BaseException:
            self.slots.release()
            raise

    def process_request_thread(self, request, address):
        try:
            request.settimeout(8)
            if self.tls:
                request = self.tls.wrap_socket(request, server_side=True)
            super().process_request_thread(request, address)
        except OSError:
            request.close()
        finally:
            self.slots.release()


class WatchRequest(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_):
        pass

    def authorize(self):
        # Reading the small file per request permits rotation without stopping capture.
        try:
            with self.server.credentials.open("rb") as stream:
                raw = stream.read(4097)
            credentials = json.loads(raw) if len(raw) <= 4096 else {}
            header = self.headers.get("Authorization", "").encode("utf-8")
            for scope in ("whole", "crops"):
                token = credentials.get(scope)
                if token and hmac.compare_digest(header, ("Bearer " + token).encode()):
                    return scope
        except (OSError, ValueError, TypeError, AttributeError):
            pass
        return None

    def reply(self, status, body, content_type, headers=None):
        self.close_connection = True
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        for name, value in (headers or {}).items():
            self.send_header(name, str(value))
        self.end_headers()
        self.wfile.write(body)

    def json_reply(self, status, data):
        self.reply(status, json.dumps(data).encode(), "application/json")

    def image_reply(self, result):
        key, captured, image = result
        self.reply(200, image, "image/png", {
            "X-Visual-Session": key[0], "X-Visual-Frame": key[1],
            "X-Visual-Captured-At-Ms": captured,
        })

    def do_GET(self):
        self.route("GET")

    def do_POST(self):
        self.route("POST")

    def route(self, method):
        scope = self.authorize()
        if scope is None:
            self.json_reply(401, {"error": "sensor_credentials_required"})
            return
        path = urlsplit(self.path).path.split("/")[2:]
        if not self.path.startswith("/v1/"):
            self.json_reply(404, {"error": "route_unavailable"})
            return
        feed = self.server.feed
        if method == "GET" and path == ["events"]:
            if not self.server.streams.acquire(blocking=False):
                self.json_reply(503, {"error": "watcher_limit"})
                return
            try:
                self.stream(scope)
            finally:
                self.server.streams.release()
            return
        if method == "GET" and path == ["snapshot"]:
            with feed.changed:
                data = {"snapshot": feed.snapshot, "health": feed.health()}
            self.json_reply(200, data)
            return
        if (method == "GET" and len(path) in (3, 4) and path[0] == "frames"
                or method == "POST" and len(path) in (1, 2) and path[0] == "fresh"):
            field = unquote(path[-1]) if len(path) in (2, 4) else None
            if field is None and scope != "whole":
                self.json_reply(403, {"error": "whole_frame_permission_required"})
                return
            if method == "POST":
                status, result = feed.capture(field)
                if status == 200:
                    self.image_reply(result)
                else:
                    error = {404: "field_unavailable", 429: "fresh_capture_busy",
                             503: "capture_unavailable", 410: "image_unavailable"}
                    self.json_reply(status, {"error": error[status]})
                return
            try:
                key = (path[1], int(path[2]))
            except ValueError:
                self.json_reply(404, {"error": "invalid_frame"})
                return
            with feed.changed:
                feed.reap()
                stored = feed.images.get(key)
                image = stored[3].get(field) if stored else None
            if image:
                self.image_reply((key, stored[1], image))
            else:
                self.json_reply(410, {"error": "evidence_expired_or_unavailable"})
            return
        self.json_reply(404, {"error": "route_unavailable"})

    def stream(self, scope):
        feed = self.server.feed
        self.close_connection = True
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-store")
        self.send_header("Connection", "close")
        self.end_headers()
        header = self.headers.get("Last-Event-ID", "")
        cursor = None
        if header:
            epoch, _, number = header.partition(":")
            if epoch == feed.epoch and number.isascii() and number.isdigit() and len(number) <= 20:
                cursor = int(number)
        last_health = None
        previous_snapshot = None
        heartbeat_at = 0
        connection_ends = time.monotonic() + 3600

        def send(kind, data, seq=None):
            event_id = f"id: {feed.epoch}:{seq}\n" if seq is not None else ""
            self.wfile.write(f"{event_id}event: {kind}\ndata: {json.dumps(data, separators=(',', ':'))}\n\n".encode())

        def snapshot():
            return {"snapshot": feed.snapshot, "health": feed.health()}

        try:
            with feed.changed:
                initial = snapshot()
                latest = feed.sequence
                oldest = feed.events[0][0] if feed.events else latest + 1
            if header and (cursor is None or cursor > latest):
                send("gap", {"reason": "cursor_unavailable"})
                cursor = oldest - 1
            if cursor is None:
                cursor = latest
            # Only a newly attached client can advance its cursor from a snapshot.
            send("snapshot", initial, latest if not header else None)

            while time.monotonic() < connection_ends and self.authorize() is not None:
                with feed.changed:
                    feed.changed.wait_for(lambda: feed.sequence > cursor or feed.stopping, timeout=2)
                    if feed.stopping:
                        return
                    current = snapshot()
                    oldest = feed.events[0][0] if feed.events else feed.sequence + 1
                    pending = [(seq, kind, event) for seq, kind, event in feed.events if seq > cursor]
                if self.authorize() != scope:
                    return
                if cursor < oldest - 1:
                    send("gap", {"reason": "retention", "from": cursor + 1, "through": oldest - 1})
                    send("snapshot", current)
                    cursor = oldest - 1
                for seq, kind, event in pending:
                    if kind == "change" and scope != "whole":
                        event = dict(event, wholeFrame=False)
                    send(kind, event, seq)
                    cursor = seq
                health = current["health"]["state"]
                if health != last_health or time.monotonic() >= heartbeat_at:
                    send("health", current["health"])
                    if current["snapshot"] is not initial["snapshot"] and current["snapshot"] is not previous_snapshot:
                        send("snapshot", current)
                        previous_snapshot = current["snapshot"]
                    last_health = health
                    heartbeat_at = time.monotonic() + 10
        except OSError:
            return


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--bind", default="127.0.0.1")
    parser.add_argument("--port", type=int, default=42428)
    parser.add_argument("--credentials", type=Path, required=True)
    parser.add_argument("--cert")
    parser.add_argument("--key")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if not command or bool(args.cert) != bool(args.key):
        parser.error("A capture command is required. TLS requires a certificate and key.")
    if args.bind != "127.0.0.1" and not args.cert:
        parser.error("Non-loopback listeners require TLS.")
    tls = None
    if args.cert:
        tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        tls.minimum_version = ssl.TLSVersion.TLSv1_2
        tls.load_cert_chain(args.cert, args.key)
    feed = Feed(command)
    server = WatchServer((args.bind, args.port), feed, args.credentials, tls)

    def terminate(_signal, _frame):
        raise KeyboardInterrupt

    signal.signal(signal.SIGTERM, terminate)
    feed.start()
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        feed.stop()
        server.server_close()


if __name__ == "__main__":
    main()
