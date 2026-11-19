#!/usr/bin/env python3
"""Verify retained execution identity through real daemon and subprocess paths.

--account exercises a root macOS service selecting that logged-in user's session.
All state and effects are confined to a disposable daemon directory.
"""
import argparse
import base64
import hashlib
import http.server
import json
import os
import pathlib
import secrets
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request

from smoke_helpers import fake_codex, python_shell

ROOT = pathlib.Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("binary", nargs="?", default=str(ROOT / "target/release/boxen"))
parser.add_argument("--account")
parser.add_argument("--unavailable-account")
parser.add_argument("--root-owned-data", action="store_true")
args = parser.parse_args()
binary = pathlib.Path(args.binary).resolve()
command = (["/bin/bash"] if binary.suffix == ".com" else []) + [str(binary)]
proof = """import json,os,pathlib
p=pathlib.Path('execution-shell.json')
p.write_text(json.dumps(dict(uid=os.geteuid() if hasattr(os,'geteuid') else None,home=str(pathlib.Path.home()),cwd=str(pathlib.Path.cwd()))))
print(p.read_text())
"""


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        assert "workload subprocess execution context" in request["messages"][-1]["content"]
        delta = {"content": "execution checked"}
        finish = "stop"
        if not any(message["role"] == "tool" for message in request["messages"]):
            prompt = next(message["content"] for message in reversed(request["messages"]) if message["role"] == "user")
            shell = {"command": python_shell(proof)}
            if prompt.startswith(("EXECUTION_TIMEOUT", "EXECUTION_CANCEL")):
                slow = "import pathlib,time;pathlib.Path('shell-started').write_text('started');time.sleep(3);pathlib.Path('late-effect').write_text('escaped')"
                shell = {"command": python_shell(slow), "timeout_ms": 100 if prompt.startswith("EXECUTION_TIMEOUT") else 30000}
            delta = {"tool_calls": [{"index": 0, "id": "execution-shell", "type": "function",
                "function": {"name": "boxen_shell", "arguments": json.dumps(shell)}}]}
            finish = "tool_calls"
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        self.wfile.write(("data: " + json.dumps({"choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}) + "\n\ndata: [DONE]\n\n").encode())


def port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


with tempfile.TemporaryDirectory(prefix="boxen-execution-") as temporary:
    home = pathlib.Path(temporary)
    uid = os.geteuid() if hasattr(os, "geteuid") else None
    if args.account:
        assert sys.platform == "darwin" and uid == 0
        import pwd
        account = pwd.getpwnam(args.account)
        uid = account.pw_uid
        if args.root_owned_data:
            (home / "execution.json").write_text(json.dumps({"mode": "user_session", "account": args.account}))
        else:
            os.chown(home, uid, account.pw_gid)
    elif sys.platform == "darwin" and uid == 0:
        (home / "execution.json").write_text(json.dumps({"mode": "inherit"}))
    identity = subprocess.check_output(command + ["--data-dir", str(home), "identity"], text=True).strip()
    (home / "credentials").mkdir()
    (home / "credentials/openrouter.key").write_text("fixture-only")
    provider = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Provider)
    threading.Thread(target=provider.serve_forever, daemon=True).start()
    tcp, ui = port(), port()
    process = None

    def launch():
        global process
        process = subprocess.Popen(command + ["--data-dir", str(home), "serve", "--listen", f"127.0.0.1:{tcp}",
            "--ui", f"127.0.0.1:{ui}", "--codex", str(fake_codex()),
            "--openrouter-endpoint", f"http://127.0.0.1:{provider.server_port}/chat"],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        lines = [process.stdout.readline() for _ in range(3)]
        assert "Local controls:" in lines[-1], lines

    def stop():
        process.terminate()
        process.wait(timeout=10)
        process.stdout.close()
        process.stderr.close()

    def api(request):
        token = (home / "control-token").read_text().strip()
        body = {"peer": identity, "address": "", "request": request}
        with urllib.request.urlopen(urllib.request.Request(f"http://127.0.0.1:{ui}/api/operation",
                headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"},
                data=json.dumps(body).encode()), timeout=10) as response:
            return json.load(response)

    def until(ident, statuses):
        deadline = time.monotonic() + 30
        while time.monotonic() < deadline:
            run = api({"op": "get", "id": ident})
            if run["status"] in statuses:
                return run
            if run["status"] in {"failed", "interrupted", "cancelled"}:
                events = [json.loads(line) for line in (home / "runs" / ident / "events.jsonl").read_text().splitlines()]
                raise AssertionError([event for event in events if event["kind"] == "status"])
            time.sleep(.08)
        raise AssertionError("execution did not reach its expected state")

    try:
        launch()
        api({"op": "log_file", "path": str(home / "execution-diagnostics.jsonl"), "filter": {"targets": ["runtime"], "events": ["execution."]}})
        info = api({"op": "runtime_info"})["execution"]
        assert info["workload"]["uid"] == uid
        if args.account:
            assert info["daemon"]["uid"] == 0
            assert info["workload"]["mode"] == "user_session"
            assert info["workload"]["desktop_available"] is True
        retained = b"fixture Codex attachment execution-proof"
        attachment = {"name": "managed.txt", "digest": hashlib.sha256(retained).hexdigest(), "size": len(retained)}
        api({"op": "attachment", "action": "write", "file": attachment, "offset": 0, "data": base64.b64encode(retained).decode()})
        accepted = []
        for backend, filename in [("codex", "execution-proof.json"), ("openrouter", "execution-shell.json")]:
            ident = secrets.token_hex(32)
            spec = {"id": ident, "thread_id": ident, "backend": backend, "model": "fixture",
                    "prompt": "EXECUTION_PROOF", "attachments": [attachment]}
            initial = api({"op": "start", "spec": spec})
            completed = until(ident, {"completed"})
            assert completed["record"]["execution"] == initial["record"]["execution"]
            directory = pathlib.Path(completed["record"]["spec"]["cwd"])
            result = json.loads((directory / filename).read_text())
            assert result["uid"] == uid, result
            if uid is not None:
                assert (directory / filename).stat().st_uid == uid
            if args.account:
                assert result["home"] == account.pw_dir
                if backend == "codex":
                    assert result["session"] == "Aqua"
            events = [json.loads(line) for line in (home / "runs" / ident / "events.jsonl").read_text().splitlines()]
            observed = [event["data"] for event in events if event["kind"] == "execution_context"]
            assert len(observed) == 1 and observed[0]["uid"] == uid
            accepted.append((spec, completed["record"], events))
        if args.account:
            original_path = home / "attachments" / attachment["digest"]
            os.chown(original_path, 0, -1)
            previous = accepted[0][0]
            ident = secrets.token_hex(32)
            api({"op": "start", "spec": {"id": ident, "thread_id": previous["thread_id"], "backend": "codex", "model": "fixture", "prompt": "EXECUTION_HISTORY_PROOF:" + attachment["digest"]}})
            until(ident, {"completed"})
            assert original_path.stat().st_uid == uid
            assert original_path.read_bytes() == retained
            ident = secrets.token_hex(32)
            api({"op": "start", "spec": {"id": ident, "thread_id": ident, "backend": "codex", "model": "fixture", "prompt": "SLOW"}})
            until(ident, {"running"})
            os.chown(original_path, 0, -1)
            api({"op": "control", "id": ident, "action": {"type": "steer", "text": "", "attachments": [attachment]}})
            until(ident, {"completed"})
            assert original_path.stat().st_uid == uid
        slow_directories = []
        for prompt in ["EXECUTION_TIMEOUT", "EXECUTION_CANCEL"]:
            ident = secrets.token_hex(32)
            accepted_slow = api({"op": "start", "spec": {"id": ident, "thread_id": ident, "backend": "openrouter", "model": "fixture", "prompt": prompt}})
            directory = pathlib.Path(accepted_slow["record"]["spec"]["cwd"])
            slow_directories.append(directory)
            if prompt == "EXECUTION_CANCEL":
                deadline = time.monotonic() + 20
                while not (directory / "shell-started").exists():
                    assert time.monotonic() < deadline
                    time.sleep(.05)
                api({"op": "control", "id": ident, "action": {"type": "cancel"}})
                until(ident, {"cancelled"})
            else:
                until(ident, {"completed"})
        time.sleep(3.2)
        assert all(not (directory / "late-effect").exists() for directory in slow_directories), "a stopped shell escaped its process group"
        # Changed host policy cannot alter an accepted exact-ID retry.
        (home / "execution.json").write_text('{"mode":"invalid"}')
        spec, record, events = accepted[0]
        assert api({"op": "start", "spec": spec})["record"] == record
        invalid = dict(spec, id=secrets.token_hex(32), thread_id=secrets.token_hex(32))
        try:
            api({"op": "start", "spec": invalid})
        except urllib.error.HTTPError as error:
            assert error.code == 422
        else:
            raise AssertionError("invalid execution policy started a workload")
        (home / "execution.json").unlink()
        stop()
        launch()
        assert api({"op": "get", "id": spec["id"]})["record"] == record
        assert [json.loads(line) for line in (home / "runs" / spec["id"] / "events.jsonl").read_text().splitlines()] == events
        if args.unavailable_account:
            import pwd
            unavailable = pwd.getpwnam(args.unavailable_account)
            (home / "execution.json").write_text(json.dumps({"mode": "user_session", "account": args.unavailable_account}))
            try:
                ident = secrets.token_hex(32)
                api({"op": "start", "spec": {"id": ident, "thread_id": ident, "backend": "codex", "model": "fixture", "prompt": "EXECUTION_PROOF"}})
            except urllib.error.HTTPError as error:
                assert error.code == 422
            else:
                raise AssertionError("another account replaced the enrolled owner")
            os.chown(home, unavailable.pw_uid, unavailable.pw_gid)
            (home / "execution.json").write_text(json.dumps({"mode": "user_session", "account": args.unavailable_account}))
            ident = secrets.token_hex(32)
            waiting = api({"op": "start", "spec": {"id": ident, "thread_id": ident, "backend": "codex", "model": "fixture", "prompt": "EXECUTION_PROOF"}})
            until(ident, {"waiting"})
            api({"op": "control", "id": ident, "action": {"type": "cancel"}})
            until(ident, {"cancelled"})
            assert not (pathlib.Path(waiting["record"]["spec"]["cwd"]) / "execution-proof.json").exists()
        print("Execution identity, account-owned effects, attachments, both runtime launch paths, pinned retries, rejection and restart passed")
    finally:
        if process is not None and process.poll() is None:
            stop()
        provider.shutdown()
