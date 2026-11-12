#!/usr/bin/env python3
"""Original-file delivery through two real daemons, with controlled Codex and inference."""
import argparse
import base64
import hashlib
import http.server
import json
import pathlib
import secrets
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request

from smoke_helpers import fake_codex

ROOT = pathlib.Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("binary", nargs="?", default=str(ROOT / "target/debug/boxen"))
binary = pathlib.Path(parser.parse_args().binary).resolve()
command = (["/bin/bash"] if binary.suffix == ".com" else []) + [str(binary)]
marker = secrets.token_hex(16)
original = marker.encode() + b"\n" + b"fixture line\n" * 20000
reference = {"name": "review.txt", "size": len(original), "digest": hashlib.sha256(original).hexdigest()}
wire = []


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        wire.append(request)
        messages = request["messages"]
        response = {"content": "fixture reply"}
        finish = "stop"
        if not any(m["role"] == "tool" for m in messages):
            prompt = next(m["content"] for m in reversed(messages) if m["role"] == "user")
            assert marker not in prompt
            files = json.loads(prompt.rsplit("are not included in this input. ", 1)[1])
            assert files[0]["name"] == reference["name"]
            path = pathlib.Path(files[0]["path"])
            assert path.is_absolute() and path.read_bytes() == original
            response = {"tool_calls": [{"index": 0, "id": "read-1", "type": "function",
                "function": {"name": "boxen_read", "arguments": json.dumps({"path": str(path), "limit": 64})}}]}
            finish = "tool_calls"
        else:
            assert marker in next(m["content"] for m in messages if m["role"] == "tool")
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        frame = {"choices": [{"index": 0, "delta": response, "finish_reason": finish}]}
        self.wfile.write(("data: " + json.dumps(frame) + "\n\ndata: [DONE]\n\n").encode())


def port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def until(fn, seconds=45):
    end = time.monotonic() + seconds
    while time.monotonic() < end:
        result = fn()
        if result:
            return result
        time.sleep(.08)
    raise AssertionError("Attachment workflow did not reach the expected state")


with tempfile.TemporaryDirectory(prefix="boxen-attachments-") as tmp:
    home = [pathlib.Path(tmp) / name for name in ("source", "destination")]
    peer, ui = [port(), port()], [port(), port()]
    daemons = {}
    provider = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    threading.Thread(target=provider.serve_forever, daemon=True).start()

    def cli(i, *args):
        return subprocess.check_output(command + ["--data-dir", str(home[i]), *args], text=True, timeout=15).strip()

    keys = [cli(i, "identity") for i in range(2)]
    for i in range(2):
        cli(i, "trust", keys[1-i], "Attachment peer")

    def launch(i):
        process = subprocess.Popen(command + ["--data-dir", str(home[i]), "serve", "--listen",
            f"127.0.0.1:{peer[i]}", "--advertise", f"127.0.0.1:{peer[i]}", "--ui", f"127.0.0.1:{ui[i]}",
            "--codex", str(fake_codex()), "--openrouter-endpoint", f"http://127.0.0.1:{provider.server_port}/chat"],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        daemons[i] = process
        lines = [process.stdout.readline() for _ in range(3)]
        assert "Local controls:" in lines[-1], lines

    def stop(i):
        process = daemons.pop(i)
        process.terminate()
        process.wait(timeout=10)
        process.stdout.close()
        process.stderr.close()

    def api(i, j, request, watch=False):
        operation = "watch" if watch else "operation"
        token = (home[i] / "control-token").read_text().strip()
        payload = {"peer": keys[j], "address": f"127.0.0.1:{peer[j]}", "request": request}
        req = urllib.request.Request(f"http://127.0.0.1:{ui[i]}/api/{operation}",
            headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"},
            data=json.dumps(payload).encode())
        response = urllib.request.urlopen(req, timeout=20)
        if watch:
            with response:
                return [event for line in response for event in json.loads(line)["events"]]
        with response:
            return json.load(response)

    def transfer(i, j, raw, ref):
        offset = 0
        while offset < len(raw) or len(raw) == 0:
            data = raw[offset:offset+192*1024]
            result = api(i, j, {"op": "attachment", "action": "write", "file": ref,
                "offset": offset, "data": base64.b64encode(data).decode()})
            offset += len(data)
            assert result["next"] == offset
            if result["complete"]:
                break
        assert result["complete"]

    def rejected(call, phrase):
        try:
            call()
        except urllib.error.HTTPError as error:
            assert error.code == 422 and phrase in error.read().decode(), phrase
        else:
            raise AssertionError("An incomplete or damaged original was accepted")

    def start(backend, prompt, attachments):
        ident = secrets.token_hex(32)
        return {"op": "start", "spec": {"id": ident, "thread_id": ident, "backend": backend,
            "cwd": tmp, "model": "fixture", "prompt": prompt, "attachments": attachments}}

    def watch(i, j, ident):
        return api(i, j, {"id": ident, "after": 0}, True)

    try:
        launch(0)
        launch(1)
        cli(0, "ping", f"127.0.0.1:{peer[1]}", keys[1], "fixture")
        resident = secrets.token_hex(32)
        api(0, 1, {"op": "resident_create", "spec": {"id": resident, "name": "Fixture",
            "backend": "openrouter", "cwd": tmp, "model": "fixture", "auto_activate": False}})
        until(lambda: any(resident in p.read_text() for p in (home[0] / "network-directory").glob("*.json")))
        rejected(lambda: api(0, 1, start("codex", "", [reference])), "unavailable")
        bad = {"name": "bad.bin", "digest": "1"*64, "size": 1}
        rejected(lambda: transfer(0, 0, b"x", bad), "integrity")
        transfer(0, 0, original, reference)
        stop(1)
        mail = {"op": "mailbox_send", "client_id": keys[0], "message": {
            "id": secrets.token_hex(32), "to": {"type": "resident", "id": resident},
            "subject": "Conversation", "body": "", "attachments": [reference]}}
        assert api(0, 0, mail)["delivery"]["status"] == "queued"
        assert api(0, 0, mail)["id"] == mail["message"]["id"]
        rejected(lambda: api(0, 0, mail | {"message": mail["message"] | {
            "attachments": [reference | {"name": "changed.txt"}]}}), "different")
        launch(1)
        until(lambda: api(0, 0, {"op": "messaging", "args": {"action": "status",
            "id": mail["message"]["id"]}})["delivery"]["status"] == "accepted")
        stop(0)
        stop(1)
        launch(1)
        inbox = api(1, 1, {"op": "resident_inbox", "id": resident})
        assert len(inbox["messages"]) == 1 and inbox["messages"][0]["attachments"] == [reference]
        activation = secrets.token_hex(32)
        api(1, 1, {"op": "resident_activate", "id": resident, "activation_id": activation})
        events = watch(1, 1, activation)
        assert events[-1]["data"]["status"] == "completed", events[-1]
        assert len(wire) == 2
        assert any(e["kind"] == "tool_result" and e["data"]["tool"] == "boxen_read" for e in events)

        launch(0)
        # Stage a distinct remote original through the operator link before a
        # managed turn. The fixture opens its bytes from the projected path.
        managed = b"fixture Codex attachment " + secrets.token_hex(8).encode()
        codex_file = {"name": "managed.txt", "digest": hashlib.sha256(managed).hexdigest(), "size": len(managed)}
        transfer(0, 1, managed, codex_file)
        request = start("codex", "SLOW ATTACHMENT_FIXTURE", [codex_file])
        ident = request["spec"]["id"]
        api(0, 1, request)
        assert api(0, 1, request)["record"]["spec"]["id"] == ident
        until(lambda: api(0, 1, {"op": "get", "id": ident})["status"] == "running")
        update = secrets.token_hex(32)
        api(0, 1, {"op": "control", "id": ident, "action": {
            "type": "steer", "text": "", "attachments": [codex_file], "update_id": update}})
        events = watch(0, 1, ident)
        sent = [e["data"] for e in events if e["kind"] == "user_message"]
        assert len(sent) == 2 and sent[0]["attachments"] == [codex_file] and sent[1]["update_id"] == update
        assert events[-1]["data"]["status"] == "completed", events[-1]
        print("Attachment smoke passed: cross-peer offline mail, restart, explicit file reads, managed start/steer, exact retry, missing/corrupt rejection")
    finally:
        for i in list(daemons):
            stop(i)
        provider.shutdown()
        provider.server_close()
