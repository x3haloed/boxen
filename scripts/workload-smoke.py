#!/usr/bin/env python3
"""Test real daemons/transport/storage against a deterministic backend fixture."""
import json
import os
import pathlib
import secrets
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

root = pathlib.Path(__file__).resolve().parent.parent
binary = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else root / "target/release/boxen").resolve()
command = (["/bin/bash"] if binary.suffix == ".com" else []) + [str(binary)]
fixture = root / "tests/fake-codex.py"
fixture.chmod(0o755)


def port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


with tempfile.TemporaryDirectory(prefix="boxen-workloads-") as temporary:
    directories = [str(pathlib.Path(temporary) / letter) for letter in "ab"]
    processes, tokens = [], []
    peers, uis = [port(), port()], [port(), port()]

    def cli(i, *args):
        return subprocess.check_output(command + ["--data-dir", directories[i], *args], text=True, timeout=10).strip()

    def start_daemon(i):
        process = subprocess.Popen(command + ["--data-dir", directories[i], "serve", "--listen", f"127.0.0.1:{peers[i]}",
                                   "--ui", f"127.0.0.1:{uis[i]}", "--codex", str(fixture)], text=True,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        processes.append(process)
        lines = [process.stdout.readline().strip() for _ in range(3)]
        assert "Local controls:" in lines[-1], lines
        return lines[-1].split("#")[1]

    keys = [cli(i, "identity") for i in range(2)]
    cli(0, "trust", keys[1], "B")
    cli(1, "trust", keys[0], "A")
    tokens = [start_daemon(i) for i in range(2)]
    target = {"address": f"127.0.0.1:{peers[1]}", "peer": keys[1]}

    def request(operation, watch=False, i=0, destination=None):
        req = urllib.request.Request(f"http://127.0.0.1:{uis[i]}/api/" + ("watch" if watch else "operation"),
            headers={"Authorization": "Bearer " + tokens[i], "Content-Type": "application/json"},
            data=json.dumps({**(destination or target), "request": operation}).encode())
        response = urllib.request.urlopen(req, timeout=10)
        return response if watch else json.load(response)

    def spec(prompt, thread=None):
        ident = secrets.token_hex(32)
        return {"id": ident, "thread_id": thread or ident, "backend": "codex", "cwd": temporary, "model": "fixture", "prompt": prompt}

    def collect(ident, after=0):
        events = []
        with request({"id": ident, "after": after}, watch=True) as stream:
            for line in stream:
                batch = json.loads(line)
                assert "error" not in batch, batch
                events.extend(batch["events"])
                if batch["done"]:
                    return events, batch["snapshot"]
        raise AssertionError("stream ended without a terminal state")

    try:
        run = spec("SLOW LARGE")
        first = request({"op": "start", "spec": run})
        assert request({"op": "start", "spec": run})["record"] == first["record"]
        for invalid in [{**run, "prompt": "different input"}, spec("competing turn", run["thread_id"]),
                        {**spec("invalid directory"), "cwd": temporary + "/does-not-exist"}]:
            try:
                request({"op": "start", "spec": invalid})
                raise AssertionError("invalid or competing submission accepted")
            except urllib.error.HTTPError as error:
                assert error.code == 422
        stream = request({"id": run["id"], "after": 0}, watch=True)
        early, after = [], 0
        for line in stream:
            batch = json.loads(line)
            early.extend(batch["events"])
            if early:
                after = early[-1]["seq"]
            if any(e["data"].get("method") == "item/commandExecution/outputDelta" for e in early):
                assert not batch["done"] and not batch["snapshot"]["status"] == "completed"
                break
        stream.close()
        assert any(e["data"].get("method") == "item/reasoning/summaryTextDelta" for e in early)
        request({"op": "control", "id": run["id"], "action": {"type": "steer", "text": "fixture steering"}})
        later, snapshot = collect(run["id"], after)
        assert snapshot["status"] == "completed"
        events = early + later
        assert [e["seq"] for e in events] == list(range(1, snapshot["last_seq"] + 1))
        assert any(len(e["data"].get("params", {}).get("delta", "")) == 50000 for e in events)
        assert any(e["kind"] == "rpc_response" for e in events)
        print("PASS live reasoning/tool output before completion, detach/replay without gaps, large Unicode frames, steering")

        followup = spec("follow-up", run["thread_id"])
        request({"op": "start", "spec": followup})
        _, next_snapshot = collect(followup["id"])
        assert next_snapshot["backend_thread"] == snapshot["backend_thread"]
        print("PASS follow-up resumes backend conversation with a new run ID")

        approval = spec("APPROVAL")
        request({"op": "start", "spec": approval})
        with request({"id": approval["id"], "after": 0}, watch=True) as stream:
            for line in stream:
                batch = json.loads(line)
                if any(e["data"].get("id") == "approval-1" for e in batch["events"]):
                    break
        request({"op": "control", "id": approval["id"], "action": {"type": "respond", "request_id": "approval-1", "result": {"decision": "accept"}}})
        assert collect(approval["id"])[1]["status"] == "completed"
        print("PASS backend request streams to client and explicit response reaches runtime")

        cancellation = spec("SLOW")
        request({"op": "start", "spec": cancellation})
        with request({"id": cancellation["id"], "after": 0}, watch=True) as stream:
            for line in stream:
                if json.loads(line)["snapshot"]["status"] == "running":
                    break
        request({"op": "control", "id": cancellation["id"], "action": {"type": "cancel"}})
        assert collect(cancellation["id"])[1]["status"] == "cancelled"
        print("PASS cancellation is confirmed by backend interrupted state")

        try:
            request({"op": "control", "id": run["id"], "action": {"type": "cancel"}}, i=1,
                    destination={"peer": keys[1], "address": ""})
            raise AssertionError("different owner accessed run")
        except urllib.error.HTTPError as error:
            assert b"different peer" in error.read()
        print("PASS run ownership enforced independently of machine trust")

        interrupted = spec("SLOW")
        request({"op": "start", "spec": interrupted})
        with request({"id": interrupted["id"], "after": 0}, watch=True) as stream:
            for line in stream:
                if json.loads(line)["snapshot"]["status"] == "running":
                    break
        processes[1].terminate()
        processes[1].wait(timeout=5)
        tokens[1] = start_daemon(1)
        _, recovered = collect(interrupted["id"])
        assert recovered["status"] == "interrupted"
        assert collect(run["id"])[1]["status"] == "completed"
        assert len(request({"op": "list"})) == 5
        print("PASS daemon restart retains history and marks active outcome uncertain without retry")

        local = spec("local execution")
        request({"op": "start", "spec": local}, destination={"peer": keys[0], "address": ""})
        with request({"id": local["id"], "after": 0}, watch=True, destination={"peer": keys[0], "address": ""}) as stream:
            for line in stream:
                batch = json.loads(line)
                if batch["done"]:
                    assert batch["snapshot"]["status"] == "completed"
                    break
        print("PASS same operation/event contract for local execution")
    finally:
        for process in processes:
            if process.poll() is None:
                process.terminate()
            process.wait(timeout=5)
