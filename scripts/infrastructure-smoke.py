#!/usr/bin/env python3
"""Exercise durable infrastructure evidence through two real paired daemons."""
import argparse
import json
from pathlib import Path
import secrets
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request

from smoke_helpers import PipeLines, fake_codex

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("binary", nargs="?", type=Path, default=Path("target/debug/boxen"))
binary = parser.parse_args().binary.resolve()
command = (["/bin/bash"] if binary.suffix == ".com" else []) + [str(binary)]

def listener():
    sock = socket.socket()
    sock.bind(("127.0.0.1", 0))
    return sock

def port():
    with listener() as sock:
        return sock.getsockname()[1]

def frame(source):
    def read_exactly(length):
        data = bytearray()
        while len(data) < length:
            chunk = source.recv(length - len(data))
            assert chunk
            data.extend(chunk)
        return data

    header = read_exactly(2)
    return header + read_exactly(int.from_bytes(header, "big"))

def drop_reply(proxy, destination, errors):
    try:
        with proxy:
            proxy.settimeout(8)
            client, _ = proxy.accept()
            with client, socket.create_connection(destination, timeout=8) as server:
                client.settimeout(8)
                server.sendall(frame(client))
                client.sendall(frame(server))
                server.sendall(frame(client))
                server.sendall(frame(client))
                frame(server)
                # The authenticated reply exists. Closing here leaves the caller uncertain.
    except Exception as error:
        errors.append(error)

with tempfile.TemporaryDirectory(prefix="boxen-infrastructure-") as temporary:
    root = Path(temporary).resolve()
    homes = [root / "sender", root / "receiver"]
    ports, controls = [port(), port()], [port(), port()]
    processes, tokens = [None, None], [None, None]
    marker = "private fixture " + secrets.token_hex(12)

    def cli(i, *args):
        return subprocess.check_output(command + ["--data-dir", str(homes[i]), *args], text=True).strip()

    keys = [cli(i, "identity") for i in range(2)]
    for i in range(2):
        cli(i, "trust", keys[1-i], "Paired fixture", "--address", f"127.0.0.1:{ports[1-i]}")

    def launch(i):
        process = subprocess.Popen(command + ["--data-dir", str(homes[i]), "serve",
            "--listen", f"127.0.0.1:{ports[i]}", "--ui", f"127.0.0.1:{controls[i]}",
            "--codex", str(root / "missing-codex" if i == 0 else fake_codex())],
            stdout=subprocess.PIPE, text=True)
        processes[i] = process
        output = PipeLines(process.stdout)
        for _ in range(3):
            line = output.get()
            if "Local controls:" in line:
                tokens[i] = line.strip().split("#")[-1]
                return
        raise AssertionError("daemon did not open local control")

    def stop(i):
        process = processes[i]
        if process and process.poll() is None:
            process.terminate()
            process.wait(timeout=8)

    def request(i, operation, peer=None, address="", code=200):
        payload = {"peer": peer or keys[i], "address": address, "request": operation}
        req = urllib.request.Request(f"http://127.0.0.1:{controls[i]}/api/operation",
            headers={"Authorization": "Bearer " + tokens[i], "Content-Type": "application/json"},
            data=json.dumps(payload).encode())
        try:
            with urllib.request.urlopen(req, timeout=15) as response:
                assert response.status == code
                return json.load(response)
        except urllib.error.HTTPError as error:
            assert error.code == code, error.read().decode()
            return None

    def start(cwd=root):
        run = secrets.token_hex(32)
        return {"op": "start", "spec": {"id": run, "thread_id": run, "backend": "codex",
                "cwd": str(cwd), "prompt": marker}}

    def history(i):
        records, after = [], 0
        while True:
            page = request(i, {"op": "infrastructure_history", "after": after})
            assert page["machine_id"] == keys[i] and page["write_failures_since_start"] == 0
            records.extend(page["records"])
            if page["next_after"] is None:
                return records
            after = page["next_after"]

    def outcome(rows, operation, boundary, stage, status):
        return next(row for row in rows if row["run_id"] == operation["spec"]["id"]
            and row["boundary"] == boundary and row["stage"] == stage and row["outcome"] == status)

    def fill_segment(i):
        page = request(i, {"op": "infrastructure_history"})
        stop(i)
        current = homes[i] / "infrastructure" / "current.jsonl"
        remaining = 4 * 1024 * 1024 - current.stat().st_size
        seq = page["newest_seq"]
        with current.open("ab") as journal:
            while remaining:
                seq += 1
                size = min(remaining, 1024)
                if remaining - size < 32:
                    size = remaining
                line = json.dumps({"seq": seq}).encode()
                journal.write(line.ljust(size - 1) + b"\n")
                remaining -= size
        launch(i)
        return seq

    try:
        launch(0)
        launch(1)
        for i in range(2):
            assert not request(i, {"op": "log_status"})["enabled"]

        rejected = start(root / "missing-workspace")
        request(0, rejected, keys[1], f"127.0.0.1:{ports[1]}", code=422)
        refused = start()
        request(0, refused, keys[1], f"127.0.0.1:{port()}", code=502)
        acknowledged = start()
        request(0, acknowledged, keys[1], f"127.0.0.1:{ports[1]}")

        accepted = start()
        proxy = listener()
        proxy.listen(1)
        errors = []
        relay = threading.Thread(target=drop_reply, args=(proxy, ("127.0.0.1", ports[1]), errors))
        relay.start()
        request(0, accepted, keys[1], f"127.0.0.1:{proxy.getsockname()[1]}", code=502)
        relay.join(10)
        assert not relay.is_alive() and not errors, errors

        launch_failure = start()
        request(0, launch_failure)
        for i, operation in [(0, launch_failure), (1, accepted)]:
            deadline = time.monotonic() + 10
            while True:
                state = request(i, {"op": "get", "id": operation["spec"]["id"]})
                if state["status"] in {"failed", "completed"}:
                    assert state["status"] == ("failed" if i == 0 else "completed")
                    break
                assert time.monotonic() < deadline, state["status"]
                time.sleep(.03)
            stop(i)
            if i == 0:
                with (homes[i] / "infrastructure" / "current.jsonl").open("ab") as journal:
                    journal.write(b'{"unfinished":')
            launch(i)

        origin, destination = history(0), history(1)
        assert all(marker not in json.dumps(row) for row in origin + destination)
        assert all(token not in json.dumps(row) for token in tokens for row in origin + destination)
        assert outcome(origin, refused, "peer_out", "connect", "failed")["reason"] == "peer_unreachable"
        assert not any(row["run_id"] == refused["spec"]["id"] for row in destination)
        rejection = outcome(destination, rejected, "peer_in", "dispatch", "rejected")
        assert rejection["reason"] == "invalid_working_directory"
        assert rejection["request_id"] == outcome(origin, rejected, "peer_out", "reply", "rejected")["request_id"]
        acknowledged_reply = outcome(origin, acknowledged, "peer_out", "reply", "completed")
        assert acknowledged_reply["request_id"] == outcome(destination, acknowledged, "peer_in", "dispatch", "completed")["request_id"]
        lost_reply = outcome(origin, accepted, "peer_out", "reply", "unknown")
        retained_acceptance = outcome(destination, accepted, "peer_in", "dispatch", "completed")
        assert lost_reply["request_id"] == retained_acceptance["request_id"]
        assert outcome(origin, launch_failure, "runtime", "launch", "failed")["reason"] == "not_found"
        launched = outcome(destination, accepted, "runtime", "launch", "completed")
        assert len(launched["runtime_policy_revision"]) == 64 and launched["daemon_version"]
        assert all(len({row["session_id"] for row in records}) >= 2 for records in (origin, destination))
        recovered = request(0, {"op": "infrastructure_history"})
        assert recovered["recovered_partial_rows_since_start"] == 1
        assert recovered["invalid_rows"] == 0

        # The same history page is available over authenticated peer transport.
        page = json.loads(cli(0, "request", f"127.0.0.1:{ports[1]}", keys[1],
            json.dumps({"op": "infrastructure_history"})))
        assert any(row["request_id"] == lost_reply["request_id"] for row in page["records"])

        first_end = fill_segment(0)
        page = request(0, {"op": "infrastructure_history"})
        assert len(page["records"]) == 128 and page["next_after"] == page["records"][-1]["seq"]
        tail = request(0, {"op": "infrastructure_history", "after": first_end})
        assert tail["records"][0]["seq"] == first_end + 1
        fill_segment(0)
        rotated = request(0, {"op": "infrastructure_history"})
        assert rotated["oldest_seq"] == first_end + 1 and rotated["history_before_cursor_unavailable"]
        print("Paired delivery outcomes, restart recovery, pagination, and bounded retention verified.")
    finally:
        for i in range(2):
            stop(i)
