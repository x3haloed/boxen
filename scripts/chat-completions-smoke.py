#!/usr/bin/env python3
"""Exercise a named endpoint through paired Boxen daemons, with optional live vision."""

import argparse
import base64
from contextlib import ExitStack
import http.server
import json
from pathlib import Path
import re
import secrets
import socket
import struct
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import zlib


BACKEND = "chat-completions:vision"
IMAGE = b"\x89PNG\r\n\x1a\ncontrolled-test-image"


def free_port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


def id_hex():
    return secrets.token_hex(32)


def png_card(color, shape, count):
    """Generate synthetic image pixels without requiring image libraries."""
    width, height = 560, 224
    centers = [width * (i + 1) / (count + 1) for i in range(count)]
    rgb = {"red": (216, 10, 35), "blue": (10, 70, 225), "green": (20, 137, 41)}
    rows = bytearray()
    for y in range(height):
        rows.append(0)
        dy = y - height // 2
        for x in range(width):
            dx = min(abs(x - center) for center in centers)
            inside = dx * dx + dy * dy <= 42**2 if shape == "circle" else max(dx, abs(dy)) <= 39
            rows.extend(rgb[color] if inside else (255, 255, 255))

    def chunk(tag, data):
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", zlib.crc32(tag + data))

    return (b"\x89PNG\r\n\x1a\n"
            + chunk(b"IHDR", struct.pack(">IIBBBBB", width, height, 8, 2, 0, 0, 0))
            + chunk(b"IDAT", zlib.compress(rows)) + chunk(b"IEND", b""))


class Fixture(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(
            self.rfile.read(int(self.headers["Content-Length"])))
        self.server.observed.append((self.path, dict(self.headers), body))
        messages = body["messages"]
        prompt_index = max(i for i, m in enumerate(messages)
                           if m["role"] == "user" and isinstance(m["content"], str))
        prompt = messages[prompt_index]["content"]
        tools = [m for m in messages[prompt_index + 1:] if m["role"] == "tool"]
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()

        def send(delta=None, finish=None):
            frame = {"id": "fixture", "choices": [
                {"index": 0, "delta": delta or {}, "finish_reason": finish}
            ]}
            self.wfile.write(("data: " + json.dumps(frame) + "\n\n").encode())
            self.wfile.flush()

        try:
            if prompt == "Wait for cancellation":
                send({"content": "started "})
                for _ in range(150):
                    time.sleep(.02)
                    send({"content": "waiting "})
                send(finish="stop")
            elif prompt.startswith("Read: ") and not tools:
                for index, path in enumerate(prompt[len("Read: "):].split(",")):
                    send({"tool_calls": [{"index": index, "id": f"read-{index}", "type": "function",
                                          "function": {"name": "boxen_read",
                                                       "arguments": json.dumps({"path": path})}}]})
                send(finish="tool_calls")
            elif prompt == "Interrupt":
                send({"content": "unaccepted partial "})
                return
            elif prompt == "Steer before finish":
                send({"content": "initial "})
                time.sleep(.5)
                send({"content": "response"}, "stop")
            elif prompt == "Steering arrived":
                send({"content": "steered response"}, "stop")
            else:
                send({"reasoning_content": "fixture reasoning"})
                send({"content": "fixture "})
                time.sleep(.2)
                send({"content": "response"}, "stop")
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            self.server.disconnected.set()


class Pair:
    def __init__(self, stack, command, directory, endpoint, model, fixture_endpoint):
        self.stack = stack
        self.command = command
        self.directory = directory
        self.data = [directory / "observer", directory / "executor"]
        self.peer = [free_port(), free_port()]
        self.ui = [free_port(), free_port()]
        self.provider_file = directory / "endpoints.json"
        config = {
            "vision": {"endpoint": endpoint, "default_model": model, "images": True},
        }
        if fixture_endpoint:
            config["text"] = {"endpoint": endpoint + "-text", "default_model": "text-fixture"}
        self.provider_file.write_text(json.dumps(config))
        self.fixture_endpoint = fixture_endpoint
        self.keys = [self.cli(i, "identity") for i in range(2)]
        for i in range(2):
            self.cli(i, "trust", self.keys[1 - i], "Other")
        self.tokens = [None, None]
        self.processes = [None, None]
        self.daemon(0)
        self.daemon(1)

    def cli(self, index, *args):
        return subprocess.check_output(
            self.command + ["--data-dir", str(self.data[index]), *args],
            text=True, timeout=15).strip()

    @staticmethod
    def stop(process):
        if process.poll() is None:
            process.kill()
        process.wait(timeout=10)
        process.stdout.close()

    def daemon(self, index):
        if self.processes[index] is not None:
            self.stop(self.processes[index])
        command = self.command + [
            "--data-dir", str(self.data[index]), "serve",
            "--listen", f"127.0.0.1:{self.peer[index]}", "--ui", f"127.0.0.1:{self.ui[index]}",
        ]
        if index == 1:
            command += ["--chat-endpoints", str(self.provider_file)]
            if self.fixture_endpoint:
                command += ["--openrouter-endpoint", self.fixture_endpoint]
        log = self.stack.enter_context((self.directory / f"daemon-{index}.log").open("ab"))
        process = subprocess.Popen(command, stdout=subprocess.PIPE, stderr=log, text=True)
        self.stack.callback(self.stop, process)
        self.processes[index] = process
        lines = [process.stdout.readline() for _ in range(3)]
        assert "Local controls:" in lines[-1], lines
        self.tokens[index] = lines[-1].strip().rsplit("#", 1)[1]

    def api(self, operation, watch=False):
        url = f"http://127.0.0.1:{self.ui[0]}/api/" + ("watch" if watch else "operation")
        request = urllib.request.Request(url, headers={
            "Authorization": "Bearer " + self.tokens[0], "Content-Type": "application/json",
        }, data=json.dumps({"peer": self.keys[1], "address": f"127.0.0.1:{self.peer[1]}",
                           "request": operation}).encode())
        result = urllib.request.urlopen(request, timeout=90)
        if watch:
            return result
        with result:
            return json.load(result)

    def submit(self, prompt, backend=BACKEND, thread=None, **fields):
        ident = id_hex()
        spec = {"id": ident, "thread_id": thread or ident, "backend": backend, "model": "",
                "cwd": str(self.directory), "prompt": prompt, **fields}
        operation = "start_checked" if spec.get("required_capabilities") else "start"
        result = self.api({"op": operation, "spec": spec})
        assert result["record"]["spec"]["id"] == ident, result
        return ident

    def collect(self, ident, after=0, until=None):
        events = []
        with self.api({"id": ident, "after": after}, watch=True) as stream:
            for line in stream:
                batch = json.loads(line)
                assert "error" not in batch, batch
                events.extend(batch["events"])
                if batch["done"] or (until and any(event["kind"] == until for event in events)):
                    return events, batch["snapshot"]
        raise AssertionError("Watch ended before its completion condition")

    def cancel(self, ident):
        return self.api({"op": "control", "id": ident, "action": {"type": "cancel"}})

    def result(self, ident):
        return self.api({"op": "thread_result", "id": ident})["response"]


def expect_rejection(request, contains):
    try:
        reply = request()
    except urllib.error.HTTPError as error:
        with error:
            assert contains in error.read().decode()
        return
    raise AssertionError(f"Request should have been rejected: {reply}")


def controlled(pair, provider):
    catalog = {entry["id"]: entry for entry in pair.api({"op": "runtime_info"})["backends"]}
    assert catalog[BACKEND]["configured"] and catalog[BACKEND]["input"] == ["text", "image"]
    assert catalog["chat-completions:text"]["input"] == ["text"]
    expect_rejection(lambda: pair.submit("unknown", backend="chat-completions:unknown"), "unavailable")
    expect_rejection(lambda: pair.submit("unsupported", reasoning_effort="high"), "reasoning")
    policy = pair.data[1] / "runtime-policy.json"
    policy.write_text(json.dumps({"providers": ["fixture-provider"], "max_output_tokens": None}))

    (pair.directory / "picture").write_bytes(IMAGE)
    (pair.directory / "document").write_bytes(b"%PDF-1.4\nfixture")
    ident = pair.submit("Read: picture,document", required_capabilities=["queued_steering"])
    events, snapshot = pair.collect(ident)
    assert snapshot["status"] == "completed", events[-1]
    results = [e["data"]["result"] for e in events if e["kind"] == "tool_result"]
    assert results[0]["ok"] and "native file" in results[1]["error"], results
    assert snapshot["record"]["resolved_inference_model"] == "vision-fixture"
    runtime = pair.api({"op": "runtime_info", "run_id": ident})
    assert runtime["declared"]["id"] == BACKEND
    assert runtime["observed"]["session"]["model"] == "vision-fixture"
    local = provider.observed[-1][2]
    assert local["model"] == "vision-fixture"
    assert not ({"session_id", "provider", "reasoning", "plugins", "max_tokens"} & local.keys())
    assert local["messages"][-1]["role"] == "system"
    native = [part for message in local["messages"] for part in
              (message["content"] if isinstance(message["content"], list) else [])]
    encoded = next(part["image_url"]["url"] for part in native if part["type"] == "image_url")
    assert base64.b64decode(encoded.split(",", 1)[1]) == IMAGE
    assert any("tool output" in part.get("text", "") for part in native)

    (pair.directory / "picture").unlink()
    config = json.loads(pair.provider_file.read_text())
    config["vision"]["default_model"] = "replacement"
    pair.provider_file.write_text(json.dumps(config))
    pair.daemon(1)
    assert pair.collect(ident)[0] == events
    follow = pair.submit("Follow up", thread=ident)
    follow_events, follow_status = pair.collect(follow)
    assert follow_status["status"] == "completed"
    follow_body = provider.observed[-1][2]
    assert follow_body["model"] == "vision-fixture"
    assert local["messages"][:-1] == follow_body["messages"][:len(local["messages"]) - 1]
    assert any(e["kind"] == "assistant_delta" for e in follow_events)
    assert pair.result(follow) == "fixture response"
    assert pair.collect(follow, after=follow_events[1]["seq"])[0] == follow_events[2:]

    (pair.directory / "text-profile-picture").write_bytes(IMAGE)
    other = pair.submit("Read: text-profile-picture", backend="chat-completions:text")
    other_events, other_status = pair.collect(other)
    assert other_status["status"] == "completed"
    assert any(e["kind"] == "tool_result" and not e["data"]["result"]["ok"] for e in other_events)
    assert provider.observed[-1][0] == "/local-text"
    assert provider.observed[-1][2]["model"] == "text-fixture"

    legacy = pair.submit("Legacy request", backend="openrouter", model="legacy-fixture",
                         reasoning_effort="high")
    assert pair.collect(legacy)[1]["status"] == "completed"
    legacy_path, _, upstream = provider.observed[-1]
    assert legacy_path == "/router"
    assert upstream["provider"]["only"] == ["fixture-provider"]
    assert upstream["session_id"] == f"boxen:{legacy}:workload"
    assert upstream["reasoning"] == {"effort": "high"}
    assert all("Authorization" not in headers for _, headers, _ in provider.observed)

    interrupted = pair.submit("Interrupt")
    assert pair.collect(interrupted)[1]["status"] == "interrupted"
    health = pair.api({"op": "inference_health"})["incidents"]
    assert any(row["backend"] == BACKEND and row["last_run_id"] == interrupted for row in health)
    resume_id = id_hex()
    request = {"op": "inference_resume", "failed_run_id": interrupted, "id": resume_id}
    for _ in range(2):
        assert pair.api(request)["record"]["spec"]["id"] == resume_id
    assert pair.collect(resume_id)[1]["status"] == "completed"
    assert "unaccepted partial" not in json.dumps(provider.observed[-1][2])

    steer = pair.submit("Steer before finish")
    assert pair.collect(steer, until="assistant_delta")[1]["status"] == "running"
    pair.api({"op": "control", "id": steer, "action": {"type": "steer", "text": "Steering arrived"}})
    assert pair.collect(steer)[1]["status"] == "completed"
    assert pair.result(steer) == "steered response"

    resident = pair.api({"op": "resident_create", "spec": {
        "id": id_hex(), "name": "Example local resident", "backend": BACKEND, "model": "",
        "cwd": str(pair.directory), "auto_activate": False,
    }})
    activation = id_hex()
    pair.api({"op": "resident_activate", "id": resident["id"],
              "activation_id": activation, "directions": "Complete privately."})
    activation_events, activation_state = pair.collect(activation)
    assert activation_state["status"] == "completed"
    assert any(e["kind"] == "assistant_message" and e["data"]["private"] for e in activation_events)
    assert not any(e["kind"] == "tool_started" for e in activation_events)

    cancel = pair.submit("Wait for cancellation")
    partial, state = pair.collect(cancel, until="assistant_delta")
    assert partial and state["status"] == "running"
    pair.cancel(cancel)
    assert pair.collect(cancel)[1]["status"] == "cancelled"
    assert provider.disconnected.wait(4), "Provider did not see socket close"
    print("PASS routing, policy isolation, streaming, replay, media, recovery, steering, resident activation and cancellation")


def live(pair):
    # The prompt and opaque names contain none of the visual answers.
    colors = secrets.SystemRandom().sample(["red", "green", "blue"], 2)
    first = None
    for color, shape, count in zip(colors, ("circle", "square"), (3, 2)):
        pathname = pair.directory / (id_hex()[:12] + ".png")
        pathname.write_bytes(png_card(color, shape, count))
        prompt = (f"Call boxen_read with path {pathname.name}. Describe the image returned by the tool. "
                  "How many colored shapes are shown, what color are they, and what shape are they? "
                  "Use one short sentence.")
        ident = pair.submit(prompt)
        events, snapshot = pair.collect(ident)
        reads = [e["data"]["result"] for e in events if e["kind"] == "tool_result"
                 and e["data"]["tool"] == "boxen_read"]
        assert snapshot["status"] == "completed" and reads and reads[-1]["ok"], events[-5:]
        reply = pair.result(ident)
        terms = re.findall(r"[a-z0-9]+", reply.lower())
        assert color in terms and shape + "s" in terms and (
            str(count) in terms or {2: "two", 3: "three"}[count] in terms
        ), reply
        assert pair.collect(ident)[0] == events
        assert any(e["kind"] == "assistant_delta" for e in events)
        pathname.unlink()
        print(f"PASS live native image and tool result: {reply}", flush=True)
        if first is None:
            first = (ident, color)
    pair.daemon(1)
    follow = pair.submit("What color were the shapes in the image from my first request? Answer with only the color.",
                         thread=first[0])
    assert pair.collect(follow)[1]["status"] == "completed"
    assert first[1] in pair.result(follow).lower()
    print("PASS live same-thread continuity after daemon restart and source removal", flush=True)

    cancel = pair.submit("Write a numbered list of 500 unrelated English words, exactly one word per line.")
    partial, state = pair.collect(cancel, until="assistant_delta")
    assert partial and state["status"] == "running"
    pair.cancel(cancel)
    assert pair.collect(cancel)[1]["status"] == "cancelled"
    recovery = pair.submit("Reply with the word READY and nothing else.")
    assert pair.collect(recovery)[1]["status"] == "completed" and "READY" in pair.result(recovery).upper()
    print("PASS live streamed cancellation and subsequent generation", flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", nargs="?", type=Path, default=Path("target/debug/boxen"))
    parser.add_argument("--live-endpoint", help="Loopback Chat Completions URL for an actual model")
    parser.add_argument("--model", default="bonsai-2.5-27b", help="Live endpoint wire model")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    command = (["/bin/bash"] if binary.suffix == ".com" else []) + [str(binary)]
    with ExitStack() as stack:
        directory = Path(stack.enter_context(tempfile.TemporaryDirectory(prefix="boxen-chat-")))
        if args.live_endpoint:
            live(Pair(stack, command, directory, args.live_endpoint, args.model, None))
        else:
            provider = stack.enter_context(http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture))
            provider.observed = []
            provider.disconnected = threading.Event()
            threading.Thread(target=provider.serve_forever, daemon=True).start()
            stack.callback(provider.shutdown)
            base = f"http://127.0.0.1:{provider.server_port}"
            controlled(Pair(stack, command, directory, base + "/local", "vision-fixture",
                            base + "/router"), provider)


if __name__ == "__main__":
    main()
