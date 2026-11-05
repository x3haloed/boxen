#!/usr/bin/env python3
"""Test real daemons/transport/storage against a deterministic backend fixture."""
from smoke_helpers import fake_codex, PipeLines
import json
import threading
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
fixture = fake_codex()


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

    class Gate:
        """An actual TCP partition: close existing sockets and refuse new ones."""
        def __init__(self):
            self.listener = socket.socket()
            self.listener.bind(("127.0.0.1", 0))
            self.listener.listen()
            self.address = f"127.0.0.1:{self.listener.getsockname()[1]}"
            self.enabled = True
            self.sockets = []
            self.lock = threading.Lock()
            threading.Thread(target=self.accept, daemon=True).start()

        def accept(self):
            while True:
                try:
                    downstream, _ = self.listener.accept()
                    with self.lock:
                        if not self.enabled:
                            downstream.close()
                            continue
                        try:
                            upstream = socket.create_connection(("127.0.0.1", peers[1]), timeout=3)
                        except OSError:
                            downstream.close()
                            continue
                        upstream.settimeout(None)
                        self.sockets.extend([downstream, upstream])
                    def relay(source, destination):
                        try:
                            while data := source.recv(65536):
                                destination.sendall(data)
                        except OSError:
                            pass
                        finally:
                            for sock in [source, destination]:
                                try: sock.shutdown(socket.SHUT_RDWR)
                                except OSError: pass
                                sock.close()
                                with self.lock:
                                    if sock in self.sockets: self.sockets.remove(sock)
                    for pair in [(downstream, upstream), (upstream, downstream)]:
                        threading.Thread(target=relay, args=pair, daemon=True).start()
                except OSError:
                    return

        def partition(self):
            with self.lock:
                self.enabled = False
                for sock in self.sockets[:]:
                    try: sock.shutdown(socket.SHUT_RDWR)
                    except OSError: pass
                    sock.close()

    readers = {}
    def read_line(process, timeout=10):
        if process not in readers:
            readers[process] = PipeLines(process.stdout)
        return json.loads(readers[process].get(timeout))

    watchers = []
    gate = Gate()
    target["address"] = gate.address
    try:
        run = spec("SLOW")
        request({"op":"start", "spec":run})
        watch = subprocess.Popen(command + ["--data-dir", directories[0], "watch", gate.address, keys[1], run["id"]],
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        watchers.append(watch)
        events = []
        while True:
            event = read_line(watch)
            events.append(event)
            if event["data"].get("method") == "item/commandExecution/outputDelta": break
        gate.partition()
        time.sleep(.8)
        if watch.poll() is not None:
            raise AssertionError(f"watcher exited instead of reconnecting: status={watch.returncode}, stderr={watch.stderr.read().decode()}")
        gate.enabled = True
        while True:
            event = read_line(watch)
            events.append(event)
            if event["kind"] == "status" and event["data"]["status"] == "completed": break
        watch.wait(timeout=10)
        assert watch.returncode == 0
        assert b"outcome unknown" in watch.stderr.read()
        snapshot = request({"op":"get", "id":run["id"]})
        assert [e["seq"] for e in events] == list(range(1, snapshot["last_seq"]+1))
        print("PASS actual TCP partition, automatic shell reconnect, gap-free exactly-once event projection")
        closed = subprocess.Popen(command + ["--data-dir", directories[0], "watch", gate.address, keys[1], run["id"], "--once"],
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        watchers.append(closed)
        closed.stdout.close()
        assert closed.wait(timeout=10) == 0, closed.stderr.read().decode()
        print("PASS closed stdout exits cleanly; socket errors remain reconnectable")

        gate.partition()
        reconnect = subprocess.Popen(command + ["--data-dir", directories[0], "watch", gate.address, keys[1], run["id"], "--after", str(snapshot["last_seq"]-3)],
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        watchers.append(reconnect)
        time.sleep(.7)
        assert reconnect.poll() is None
        gate.enabled = True
        output, errors = reconnect.communicate(timeout=10)
        assert reconnect.returncode == 0, errors
        assert [json.loads(line)["seq"] for line in output.splitlines()] == list(range(snapshot["last_seq"]-2,snapshot["last_seq"]+1))
        print("PASS initially unreachable machine reconnects using explicit replay cursor")

        # The requesting daemon can restart while execution on the other box continues.
        survives = spec("SLOW")
        # Send a complete submission but discard the HTTP response. The client
        # cannot infer acceptance from that lost acknowledgement.
        payload = json.dumps({**target,"request":{"op":"start","spec":survives}}).encode()
        with socket.create_connection(("127.0.0.1",uis[0]),timeout=3) as lost:
            lost.sendall((f"POST /api/operation HTTP/1.1\r\nHost: 127.0.0.1:{uis[0]}\r\nAuthorization: Bearer {tokens[0]}\r\nContent-Type: application/json\r\nContent-Length: {len(payload)}\r\n\r\n").encode()+payload)
        deadline = time.monotonic()+5
        while not any(r["record"]["spec"]["id"]==survives["id"] for r in request({"op":"list"})):
            assert time.monotonic()<deadline,"submission not accepted before disconnect"
            time.sleep(.05)
        old_token = tokens[0]
        processes[0].kill(); processes[0].wait(timeout=5)
        tokens[0] = start_daemon(0)
        assert tokens[0] == old_token, "local authorization did not survive restart"
        assert collect(survives["id"])[1]["status"] == "completed"
        assert request({"op":"start", "spec":survives})["record"]["spec"] == survives
        assert len(request({"op":"list"})) == 2
        print("PASS requesting-daemon restart preserves authorization and remote work; uncertain submission retry does not duplicate")

        # After executing-daemon death the shell reconnects and reports uncertainty.
        crashed = spec("SLOW")
        request({"op":"start", "spec":crashed})
        watch = subprocess.Popen(command + ["--data-dir", directories[0], "watch", gate.address, keys[1], crashed["id"]],
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE, bufsize=0)
        watchers.append(watch)
        events = []
        while True:
            event = read_line(watch); events.append(event)
            if event["kind"] == "status" and event["data"]["status"] == "running": break
        processes[1].kill(); processes[1].wait(timeout=5)  # original remote process
        time.sleep(.3)
        old_token = tokens[1]
        tokens[1] = start_daemon(1)
        assert old_token == tokens[1]
        while True:
            event = read_line(watch); events.append(event)
            if event["kind"] == "status" and event["data"]["status"] == "interrupted": break
        watch.wait(timeout=10)
        assert watch.returncode == 0
        recovered = request({"op":"get", "id":crashed["id"]})
        assert [e["seq"] for e in events] == list(range(1,recovered["last_seq"]+1))
        assert request({"op":"start", "spec":crashed})["status"] == "interrupted"
        continuation=spec("continue after incident",crashed["thread_id"])
        request({"op":"start","spec":continuation})
        continued_events,continued_snapshot=collect(continuation["id"])
        assert continued_snapshot["status"]=="completed"
        assert any(e["kind"]=="recovery_notice" for e in continued_events)
        assert len(request({"op":"list"})) == 4
        assert collect(run["id"])[1]["status"] == "completed"
        print("PASS executing-daemon crash retains history, shell reconnects to uncertain outcome, no automatic execution retry")

        shared=request({"op":"get","id":run["id"]},i=1,destination={"peer":keys[1],"address":""})
        assert shared["record"]["owner"]==keys[0]
        print("PASS shared operator snapshot lookup retains original ownership")
    finally:
        gate.partition();gate.listener.close()
        for process in watchers + processes:
            if process.poll() is None: process.terminate()
            process.wait(timeout=5)
