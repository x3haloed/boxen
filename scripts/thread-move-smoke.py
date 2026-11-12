#!/usr/bin/env python3
"""Move an anonymous conversation between real paired daemons and recover an unobserved reply."""
import argparse
import base64
import hashlib
import http.client
import json
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import time
import urllib.error
import urllib.request

from smoke_helpers import PipeLines, fake_codex

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("binary", nargs="?", type=Path, default=Path("target/debug/boxen"))
args = parser.parse_args()
binary = args.binary.resolve()
command = (["/bin/bash"] if binary.suffix == ".com" else []) + [str(binary)]

def port():
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        return listener.getsockname()[1]

def eventually(check):
    deadline = time.monotonic() + 20
    while not (result := check()):
        assert time.monotonic() < deadline, "condition did not become true"
        time.sleep(.05)
    return result

with tempfile.TemporaryDirectory(prefix="boxen-thread-move-") as temp:
    root = Path(temp).resolve()
    homes = [root / "source", root / "destination"]
    ports = [port(), port()]
    addresses = [f"127.0.0.1:{p}" for p in ports]
    controls = [port(), port()]
    processes = [None, None]
    tokens = [None, None]

    def cli(i, *args):
        return subprocess.check_output(command + ["--data-dir", str(homes[i]), *args], text=True).strip()

    keys = [cli(i, "identity") for i in range(2)]
    for i in range(2):
        cli(i, "trust", keys[1-i], "Other fixture", "--address", addresses[1-i])

    def launch(i):
        process = subprocess.Popen(command + ["--data-dir", str(homes[i]), "serve", "--listen",
            addresses[i], "--ui", f"127.0.0.1:{controls[i]}", "--codex", str(fake_codex())],
            stdout=subprocess.PIPE, text=True)
        processes[i] = process
        pipe = PipeLines(process.stdout)
        for _ in range(3):
            line = pipe.get()
            if "Local controls:" in line:
                tokens[i] = line.strip().split("#")[-1]
                return
        raise AssertionError("daemon did not open local control")

    def stop(i):
        process = processes[i]
        if process and process.poll() is None:
            process.terminate()
            process.wait(timeout=8)

    def envelope(i, operation):
        return json.dumps({"peer": keys[i], "address": "", "request": operation}).encode()

    def headers(i):
        return {"Authorization": "Bearer " + tokens[i], "Content-Type": "application/json"}

    def op(i, **operation):
        request = urllib.request.Request(f"http://127.0.0.1:{controls[i]}/api/operation",
            headers=headers(i), data=envelope(i, operation))
        with urllib.request.urlopen(request, timeout=20) as response:
            return json.load(response)

    def rejected(i, detail, **operation):
        try:
            op(i, **operation)
        except urllib.error.HTTPError as error:
            message = error.read().decode()
            assert detail.lower() in message.lower(), message
        else:
            raise AssertionError("operation should have been rejected")

    def completed(i, run):
        def poll():
            result = op(i, op="get", id=run)
            status = result["status"]
            assert status in {"starting", "running", "waiting", "completed"}, result
            return result if status == "completed" else None
        return eventually(poll)

    try:
        launch(0)
        launch(1)
        thread = secrets.token_hex(32)
        contents = secrets.token_bytes(1700)
        file = {"name":"fixture.bin", "digest":hashlib.sha256(contents).hexdigest(), "size":len(contents)}
        op(0, op="attachment", action="write", file=file, offset=0, data=base64.b64encode(contents).decode())
        spec = {"id":thread,"thread_id":thread,"backend":"codex","prompt":"First thought", "attachments":[file]}
        op(0, op="start", spec=spec)
        first = completed(0, thread)
        source_cwd = Path(first["record"]["spec"]["cwd"])
        assert source_cwd == homes[0]/"threads"/thread
        (source_cwd / "earlier.bin").write_bytes(contents)
        move = {"op":"thread_move","id":secrets.token_hex(32),"thread_id":thread,"after":thread,
            "target":keys[1],"address":addresses[1]}
        rejected(0, "could not be prepared", **dict(move, cwd=str(root/"missing")))
        assert op(0, op="thread_placement", thread_id=thread) is None
        # Send a real request and close the client connection before its response.
        connection = http.client.HTTPConnection("127.0.0.1", controls[0], timeout=5)
        connection.request("POST", "/api/operation", envelope(0, move), headers(0))
        connection.close()
        eventually(lambda: op(1, op="thread_placement", thread_id=thread))
        retry = op(0, **move)
        assert retry["complete"] and retry["machine"] == keys[1] and retry["generation"] == 1
        assert op(1, op="get", id=thread) == first
        assert (homes[1]/"threads"/thread).is_dir()
        assert not (homes[1]/"threads"/thread/"earlier.bin").exists()
        assert base64.b64decode(op(1, op="attachment", action="read", file=file, offset=0)["data"]) == contents
        stop(0)
        assert op(1, op="get", id=thread) == first
        rejected(1, "unreachable", op="thread_workspace", action="pull", thread_id=thread, run_id=thread, path="earlier.bin")
        for i in range(2):
            stop(i)
            launch(i)
        rejected(0, "execution moved", op="start", spec=dict(spec, id=secrets.token_hex(32)))
        assert op(0, op="start", spec=spec) == first
        assert op(0, **move) == retry
        earlier = op(1, op="thread_workspace", action="list", thread_id=thread)
        assert earlier["directories"][0]["machine"] == keys[0]
        pulled = op(1, op="thread_workspace", action="pull", thread_id=thread, run_id=thread, path="earlier.bin")
        assert Path(pulled["path"]).read_bytes() == contents
        rejected(1, "must be relative", op="thread_workspace", action="pull", thread_id=thread, run_id=thread, path="../control-token")
        changed = b"source changed after its original snapshot"
        (source_cwd / "earlier.bin").write_bytes(changed)
        second = secrets.token_hex(32)
        op(1, op="start", after=thread, spec=dict(spec, id=second, prompt="WORKSPACE_MOVE_FIXTURE", attachments=[]))
        result = completed(1, second)
        change = result["record"]["execution_change"]
        assert change["previous"]["machine"] == keys[0] and change["current"]["machine"] == keys[1]
        assert change["current"]["cwd"] == str(homes[1]/"threads"/thread)
        events = (homes[1]/"runs"/second/"events.jsonl").read_text().splitlines()
        assert any(json.loads(line)["kind"] == "backend_session_rebuilt" for line in events)
        assert hashlib.sha256(changed).hexdigest() in op(1, op="thread_result", id=second)["response"]
        assert {s["record"]["spec"]["id"] for s in op(1, op="list")} == {thread,second}
        back = op(1, **dict(move, id=secrets.token_hex(32), target=keys[0], address=addresses[0], after=second))
        assert back["generation"] == 2
        assert op(0, **move) == retry
        assert op(0, op="get", id=second) == result
        rejected(1, "execution moved", op="start", spec=dict(spec, id=secrets.token_hex(32), attachments=[]))
        print("Paired daemons validated destination path, retained history and attachments, single execution authority, restart, unobserved reply retry, and a move back.")
    finally:
        for i in range(2):
            stop(i)
