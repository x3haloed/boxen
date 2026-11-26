#!/usr/bin/env python3
"""Measure a Linux Boxen daemon against an isolated synthetic run archive."""

import argparse
from contextlib import contextmanager
import hashlib
import http.server
import json
import os
from pathlib import Path
import platform
import secrets
import shutil
import socket
import subprocess
import tempfile
import threading
import time
import urllib.request


def now_ms():
    return time.time_ns() // 1_000_000


def port(exclude=None):
    while True:
        with socket.socket() as sock:
            sock.bind(("127.0.0.1", 0))
            selected = sock.getsockname()[1]
        if selected != exclude:
            return selected


def wait_until(probe, process, timeout):
    deadline = time.monotonic() + timeout
    while process.poll() is None and time.monotonic() < deadline:
        value = probe()
        if value is not None:
            return value
        time.sleep(0.02)
    raise RuntimeError(f"Daemon did not reach the expected state (exit={process.poll()})")


@contextmanager
def daemon(command, home, owner, provider_port, scratch, timeout):
    peer = port()
    ui = port(peer)
    started = time.monotonic()
    with tempfile.TemporaryFile(dir=scratch) as output:
        process = subprocess.Popen(
            command + [
                "--data-dir", str(home), "serve", "--listen", f"127.0.0.1:{peer}",
                "--ui", f"127.0.0.1:{ui}", "--openrouter-endpoint",
                f"http://127.0.0.1:{provider_port}/chat",
            ],
            stdout=output, stderr=subprocess.STDOUT,
        )
        try:
            def readiness():
                for line in os.pread(output.fileno(), 16384, 0).splitlines():
                    _, separator, tail = line.partition(b"Local controls:")
                    if separator and b"#" in tail:
                        return tail.rsplit(b"#", 1)[1].decode()
                return None

            token = wait_until(readiness, process, timeout)
            base = f"http://127.0.0.1:{ui}"

            def request(route, body=None):
                data = None if body is None else json.dumps(body).encode()
                req = urllib.request.Request(
                    base + route, data=data,
                    headers={"Authorization": "Bearer " + token,
                             "Content-Type": "application/json"},
                )
                with urllib.request.urlopen(req, timeout=timeout) as response:
                    return json.load(response)

            def operation(body):
                return request("/api/operation", {"peer": owner, "address": "", "request": body})

            request("/api/status")
            yield process, operation, time.monotonic() - started
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait()


def process_stats(pid):
    fields = {}
    for line in Path(f"/proc/{pid}/status").read_text().splitlines():
        name, _, value = line.partition(":")
        if name in ("VmRSS", "VmHWM", "VmSwap", "Threads"):
            fields[name] = int(value.split()[0])
    stat = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    ticks = int(stat[11]) + int(stat[12])
    return {
        "rss_kib": fields["VmRSS"],
        "peak_rss_kib": fields["VmHWM"],
        "swap_kib": fields["VmSwap"],
        "threads": fields["Threads"],
        "cpu_seconds": ticks / os.sysconf("SC_CLK_TCK"),
    }


def gauge(process, action):
    begin = time.monotonic()
    before = process_stats(process.pid)
    action()
    after = process_stats(process.pid)
    elapsed = time.monotonic() - begin
    return {
        "seconds": round(elapsed, 3),
        "core_fraction": round((after["cpu_seconds"] - before["cpu_seconds"]) / elapsed, 4),
        "rss_delta_kib": after["rss_kib"] - before["rss_kib"],
        "final": after,
    }


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_args):
        pass

    def do_POST(self):
        self.rfile.read(int(self.headers["Content-Length"]))
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        frame = {"choices": [{"index": 0, "delta": {"content": "Profile complete"},
                              "finish_reason": "stop"}]}
        self.wfile.write(b"data: " + json.dumps(frame).encode() + b"\n\ndata: [DONE]\n\n")


def event(seq, at, kind, data):
    return json.dumps({"seq": seq, "at": at, "kind": kind, "data": data},
                      separators=(",", ":")).encode() + b"\n"


def archive(home, owner, residents, runs, rows, payload_bytes, snapshot_bytes):
    at = now_ms() - 86_400_000
    resident_frame = {
        "step": 0, "purpose": "workload",
        "event": {"type": "provider_frame", "frame": {
            "choices": [{"index": 0, "delta": {"content": "x" * payload_bytes}}]}},
    }
    # Each run receives its own physical journal, with the same controlled payload mix.
    # Prepare one run's wire bytes at a time so archive creation stays bounded.
    journal = b"".join([
        event(1, at, "status", {"status": "running", "detail": "Synthetic workload"}),
        *[event(seq, at, "inference_frame", resident_frame) for seq in range(2, rows)],
        event(rows, at, "status", {"status": "completed", "detail": "Synthetic workload"}),
    ])
    # Leave space for metadata and fresh activations.
    if (len(journal) + snapshot_bytes + 8192) * runs + 64 * 1024 * 1024 > shutil.disk_usage(home).free:
        raise RuntimeError("Scratch volume does not have space for the requested archive")
    snapshots = [
        dict(resident, memory="m" * (snapshot_bytes // 2),
             identity="i" * (snapshot_bytes - snapshot_bytes // 2))
        for resident in residents
    ] if snapshot_bytes else residents
    record_bytes = 0
    for count in range(runs):
        ident = hashlib.sha256(f"profile-run-{count}".encode()).hexdigest()
        resident = snapshots[count % len(snapshots)]
        thread = resident["id"]
        run = home / "runs" / ident
        run.mkdir(mode=0o700)
        record = {
            "spec": {"id": ident, "thread_id": thread, "resident_id": thread,
                     "backend": "openrouter", "model": "fixture", "cwd": str(home),
                     "prompt": "Synthetic archive input."},
            "created_at": at + count, "owner": owner,
            "mailbox": {"message_ids": [], "reason": "manual", "directions": ""},
        }
        if snapshot_bytes:
            record["resident"] = resident
        encoded = json.dumps(record).encode() + b"\n"
        (run / "record.json").write_bytes(encoded)
        (run / "events.jsonl").write_bytes(journal)
        record_bytes += len(encoded)
    return len(journal) * runs, record_bytes


def publish_samples(operation, home, count):
    source_id = secrets.token_hex(32)
    source = operation({
        "op": "events", "args": {"action": "create_source", "spec": {
            "id": source_id, "name": "Profile notifications", "cwd": str(home),
            "command": [], "interval_ms": 1000, "mode": "latest", "timeout_ms": 1000,
            "tools": [],
        }},
    })
    for seq in range(1, count + 1):
        operation({
            "op": "events", "args": {
                "action": "publish", "id": source_id, "generation": source["generation"],
                "sequence": seq, "response": {
                    "protocol": "boxen-adapter-v1",
                    "observations": [{"id": str(seq), "observed_at_ms": now_ms(),
                                      "payload": {"sample": seq}}],
                },
            },
        })
        time.sleep(1)


def activate(process, operation, resident, timeout):
    ident = secrets.token_hex(32)
    start = time.monotonic()
    operation({"op": "resident_activate", "id": resident, "activation_id": ident,
               "directions": "Run the controlled profile."})
    accepted = time.monotonic()

    def terminal():
        snapshot = operation({"op": "get", "id": ident})
        status = snapshot["status"]
        if status == "completed":
            return status
        if status in ("failed", "interrupted", "cancelled"):
            raise RuntimeError(f"Controlled activation ended with status {status}")
        return None

    wait_until(terminal, process, timeout)
    return {"accepted_ms": round((accepted - start) * 1000, 2),
            "completed_ms": round((time.monotonic() - start) * 1000, 2),
            "daemon": process_stats(process.pid)}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("--scratch", type=Path, help="Parent for the disposable data directory")
    parser.add_argument("--runs", type=int, default=200)
    parser.add_argument("--events", type=int, default=200, help="Events per synthetic run")
    parser.add_argument("--payload-bytes", type=int, default=500)
    parser.add_argument("--residents", type=int, default=12)
    parser.add_argument("--resident-snapshot-bytes", type=int, default=0,
                        help="Bytes of identity and memory retained in each historical record")
    parser.add_argument("--wakes", type=int, default=0,
                        help="Publish this many observations, one second apart, without a subscription")
    parser.add_argument("--idle-seconds", type=float, default=10)
    parser.add_argument("--activations", type=int, default=3)
    parser.add_argument("--timeout", type=float, default=120)
    parser.add_argument("--max-idle-core-fraction", type=float)
    parser.add_argument("--max-peak-rss-mib", type=float)
    args = parser.parse_args()
    if platform.system() != "Linux":
        parser.error("This benchmark uses Linux /proc accounting")
    if (min(args.runs, args.residents, args.activations) < 1
            or args.events < 2 or min(args.payload_bytes, args.wakes) < 0
            or not 0 <= args.resident_snapshot_bytes <= 1024 * 1024
            or min(args.idle_seconds, args.timeout) <= 0
            or args.events * (args.payload_bytes + 256) > 128 * 1024 * 1024):
        parser.error("Invalid archive size or measurement interval")
    binary = args.binary.resolve(strict=True)
    command = (["/bin/bash"] if binary.suffix == ".com" else []) + [str(binary)]
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()

    with tempfile.TemporaryDirectory(prefix="boxen-journal-profile-", dir=args.scratch) as tmp:
        home = Path(tmp) / "data"
        owner = subprocess.check_output(command + ["--data-dir", str(home), "identity"],
                                        text=True, timeout=args.timeout).strip()
        with http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider) as provider:
            worker = threading.Thread(target=provider.serve_forever, daemon=True)
            worker.start()
            try:
                settings = (command, home, owner, provider.server_port, tmp, args.timeout)
                with daemon(*settings) as (process, operation, seconds):
                    residents = []
                    for _ in range(args.residents):
                        resident = operation({
                            "op": "resident_create",
                            "spec": {"id": secrets.token_hex(32), "name": "Profile resident",
                                     "backend": "openrouter", "model": "fixture",
                                     "cwd": str(home), "auto_activate": True},
                        })
                        residents.append(resident)
                    baseline = {"ready_seconds": round(seconds, 3),
                                "idle": gauge(process, lambda: time.sleep(args.idle_seconds))}
                size, record_bytes = archive(home, owner, residents, args.runs, args.events,
                                             args.payload_bytes, args.resident_snapshot_bytes)
                with daemon(*settings) as (process, operation, seconds):
                    ready = process_stats(process.pid)
                    idle = gauge(process, lambda: time.sleep(args.idle_seconds))
                    wakes = gauge(process, lambda: publish_samples(operation, home, args.wakes)) \
                        if args.wakes else None
                    latencies = [activate(process, operation, residents[0]["id"], args.timeout)
                                 for _ in range(args.activations)]
                    post = process_stats(process.pid)
            finally:
                provider.shutdown()
                worker.join()
    print(json.dumps({
        "artifact": "ape" if binary.suffix == ".com" else "native",
        "sha256": digest, "architecture": platform.machine(), "kernel": platform.release(),
        "runs": args.runs, "events": args.runs * args.events, "journal_bytes": size,
        "record_bytes": record_bytes, "residents": args.residents,
        "wake_samples": args.wakes, "notification_load": wakes,
        "baseline": baseline, "ready_seconds": round(seconds, 3), "ready": ready,
        "idle": idle, "activations": latencies, "post_activation": post,
    }, indent=2))
    if (args.max_idle_core_fraction is not None
            and idle["core_fraction"] > args.max_idle_core_fraction):
        raise SystemExit("Idle CPU exceeds the requested budget")
    if args.max_peak_rss_mib is not None and post["peak_rss_kib"] > args.max_peak_rss_mib * 1024:
        raise SystemExit("Peak RSS exceeds the requested budget")


if __name__ == "__main__":
    main()
