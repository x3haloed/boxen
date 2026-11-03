#!/usr/bin/env python3
"""Test real daemons/transport/storage against a deterministic backend fixture."""
import json
import concurrent.futures
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

    def rejected(operation, text=None, **kwargs):
        try:
            request(operation, **kwargs)
            raise AssertionError("operation unexpectedly accepted")
        except urllib.error.HTTPError as error:
            detail = error.read().decode()
            assert error.code == 422, detail
            if text: assert text in detail, detail

    def message(resident, prompt):
        return {"op":"resident_activate","id":resident["id"],"activation_id":secrets.token_hex(32),"directions":prompt}

    def get_resident(ident):
        return request({"op":"resident_get","id":ident})

    try:
        ident = secrets.token_hex(32)
        definition = {"id":ident,"name":"Pip","identity":"A resident, with no assigned purpose.","memory":"initial memory","cwd":temporary,"model":"fixture","quota_bytes":256}
        resident = request({"op":"resident_create","spec":definition})
        assert resident["thread_id"] == ident and resident["revision"] == 1
        assert resident["machine_id"] == keys[1]
        assert request({"op":"list"}) == [], "creation unexpectedly started inference"
        assert request({"op":"resident_create","spec":definition}) == resident
        rejected({"op":"resident_create","spec":{**definition,"name":"different"}},"different input")
        foreign={"i":1,"destination":{"peer":keys[1],"address":""}}
        rejected({"op":"resident_get","id":ident},"different peer",**foreign)
        assert request({"op":"resident_list"},**foreign)==[]
        print("PASS idle resident creation, retry-safe identity, canonical thread, placement and owner isolation")

        def race(memory):
            try:
                return request({"op":"resident_update","id":ident,"patch":{"expected_revision":1,"memory":memory}})
            except urllib.error.HTTPError as error:
                assert error.code == 422 and b"revision conflict" in error.read()
                return None
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            outcomes=list(pool.map(race,["first memory","second memory"]))
        assert sum(r is not None for r in outcomes)==1
        resident=get_resident(ident)
        assert resident["revision"]==2
        rejected({"op":"resident_update","id":ident,"patch":{"expected_revision":2,"model":"other"}},"managed sessions retain their model")
        rejected({"op":"resident_update","id":ident,"patch":{"expected_revision":2,"memory":"x"*300}},"quota exceeded")
        assert get_resident(ident)==resident
        rejected({"op":"resident_update","id":ident,"patch":{"expected_revision":2,"memory":"stolen"}},"different peer",**foreign)
        print("PASS concurrent memory writes use compare-and-set; stale/oversized/foreign writes leave durable state unchanged")

        run=message(resident,"RESIDENT_WRITE")
        accepted=request(run)
        assert accepted["record"]["resident"]["revision"]==2
        events,snapshot=collect(run["activation_id"])
        assert snapshot["status"]=="completed", snapshot
        assert any(e["kind"]=="resident_tool_result" and e["data"]["success"] for e in events)
        assert get_resident(ident)["memory"]=="fixture durable memory"
        assert request(run)["record"]["resident"]["revision"]==2
        assert request({"op":"resident_create","spec":definition})["revision"]==3
        print("PASS resident runtime receives pinned continuity and host context; dynamic tool commits durable memory; retries retain original context")

        processes[1].kill();processes[1].wait(timeout=5)
        tokens[1]=start_daemon(1)
        resident=get_resident(ident)
        assert resident["revision"]==3 and resident["memory"]=="fixture durable memory"
        follow=message(resident,"RESIDENT_READ")
        request(follow)
        events,next_snapshot=collect(follow["activation_id"])
        assert next_snapshot["backend_thread"]==snapshot["backend_thread"]
        assert next_snapshot["status"]=="completed"
        result=[e["data"]["result"] for e in events if e["kind"]=="resident_tool_result" and e["data"].get("tool")=="boxen_continuity"][-1]
        assert result["memory"]=="fixture durable memory" and result["revision"]==3
        print("PASS executing-daemon restart preserves identity, memory, canonical Boxen thread and backend conversation continuity")

        for prompt in ["RESIDENT_QUOTA","RESIDENT_STALE"]:
            attempted=message(resident,prompt);request(attempted)
            assert collect(attempted["activation_id"])[1]["status"]=="completed"
            assert get_resident(ident)==resident
        print("PASS model-facing tool reports quota/conflict errors without mutating memory or terminating the conversation")

        invalid={**spec("direct resident turn",ident),"resident_id":ident}
        rejected({"op":"start","spec":invalid},"mailbox")
        rejected({"op":"start","spec":{**invalid,"resident_id":None}},"requires its resident_id")
        active=message(resident,"SLOW RESIDENT_READ");request(active)
        rejected(message(resident,"competing"),"already active")
        with request({"id":active["activation_id"],"after":0},watch=True) as stream:
            for line in stream:
                if json.loads(line)["snapshot"]["status"]=="running":break
        processes[-1].kill();processes[-1].wait(timeout=5)
        tokens[1]=start_daemon(1)
        interrupted=collect(active["activation_id"])[1]
        assert interrupted["status"]=="interrupted"
        resumed=message(resident,"RESIDENT_READ");request(resumed)
        resumed_events,resumed_snapshot=collect(resumed["activation_id"])
        assert resumed_snapshot["status"]=="completed"
        assert any(e["kind"]=="recovery_notice" and e["data"]["prior_run_id"]==active["activation_id"] for e in resumed_events)
        old_events,old_snapshot=collect(active["activation_id"])
        assert sum(e["kind"]=="user_message" for e in old_events)<=1
        assert old_snapshot["status"]=="interrupted"
        assert get_resident(ident)==resident
        print("PASS canonical execution guard and crash recovery keep resident available without receipts or automatic replay; uncertainty remains visible")

        pathlib.Path(temporary,"fixture-forget-session").touch()
        rebuilt=message(resident,"RESIDENT_READ");request(rebuilt)
        rebuilt_events,rebuilt_snapshot=collect(rebuilt["activation_id"])
        assert rebuilt_snapshot["status"]=="completed"
        assert any(e["kind"]=="backend_session_rebuilt" for e in rebuilt_events)
        assert any(e["kind"]=="resident_tool_result" and e["data"]["result"].get("memory")==resident["memory"] for e in rebuilt_events)
        assert rebuilt_snapshot["record"]["spec"]["thread_id"]==ident
        print("PASS missing backend session rebuilds with retained continuity/history while preserving the canonical Boxen conversation")

        broken_id=secrets.token_hex(32)
        broken=pathlib.Path(directories[1],"residents",broken_id+".json")
        broken.write_text("{ interrupted snapshot")
        processes[-1].kill();processes[-1].wait(timeout=5)
        tokens[1]=start_daemon(1)
        issues=request({"op":"resident_issues"})
        assert len(issues)==1 and issues[0]["id"]==broken_id
        assert broken.read_text()=="{ interrupted snapshot"
        incident_log=broken.parent/"incidents.jsonl"
        assert "resident_unavailable" in incident_log.read_text()
        assert get_resident(ident)==resident
        rejected({"op":"start","spec":spec("cannot silently replace a damaged resident",broken_id)},"unavailable")
        continued=message(resident,"RESIDENT_READ");request(continued)
        assert collect(continued["activation_id"])[1]["status"]=="completed"
        print("PASS malformed resident state is isolated with a durable incident; original evidence and healthy residents remain available")
        print("PASS resident catalog and history remain independently discoverable through the daemon API")
    finally:
        for process in processes:
            if process.poll() is None:process.terminate()
            process.wait(timeout=5)
