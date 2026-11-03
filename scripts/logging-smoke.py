#!/usr/bin/env python3
"""Verify hot diagnostics through two real daemons and authenticated CLI streams."""
from smoke_helpers import fake_codex, PipeLines
import json
import pathlib
import secrets
import socket
import subprocess
import sys
import tempfile
import time

root = pathlib.Path(__file__).resolve().parent.parent
binary = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else root / "target/release/boxen").resolve()
command = (["/bin/bash"] if binary.suffix == ".com" else []) + [str(binary)]


def port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


with tempfile.TemporaryDirectory(prefix="boxen-logging-") as temporary:
    directories = [str(pathlib.Path(temporary) / letter) for letter in "ab"]
    processes = []
    peer_ports, ui_ports = [port(), port()], [port(), port()]

    def cli(index, *args, check=True):
        return subprocess.run(command + ["--data-dir", directories[index], *args],
                              capture_output=True, text=True, check=check, timeout=15)

    keys = [cli(i, "identity").stdout.strip() for i in range(2)]
    cli(0, "trust", keys[1], "B")
    cli(1, "trust", keys[0], "A")

    def daemon(index):
        process = subprocess.Popen(command + ["--data-dir", directories[index], "serve", "--listen",
            f"127.0.0.1:{peer_ports[index]}", "--ui", f"127.0.0.1:{ui_ports[index]}",
            "--codex", str(fake_codex())], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        processes.append(process)
        lines = [process.stdout.readline().strip() for _ in range(3)]
        assert "Local controls:" in lines[-1], lines
        return process

    a, b = daemon(0), daemon(1)
    address = f"127.0.0.1:{peer_ports[1]}"

    def remote(cmd, *args):
        return cli(0, cmd, address, keys[1], *args).stdout.strip()

    def status():
        return json.loads(remote("log-status"))

    def ping(text="diagnostic test"):
        return json.loads(remote("ping", text))

    def file_records(path):
        return [json.loads(line) for line in path.read_text().splitlines() if line]

    try:
        assert status()["enabled"] is False
        ping()
        assert status()["emitted"] == 0
        print("PASS default off: traffic produces no diagnostic records", flush=True)

        stream = subprocess.Popen(command + ["--data-dir", directories[0], "logs", address, keys[1],
            "--targets", "peer", "--events", "message.received", "--payloads"],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        processes.append(stream)
        deadline = time.monotonic() + 5
        while not status()["enabled"]:
            assert time.monotonic() < deadline
            time.sleep(.02)
        secret = "sk-this-is-a-test-not-a-real-credential"
        ping("Bearer " + secret)
        live = PipeLines(stream.stdout)
        for _ in range(20):
            record = json.loads(live.get(5))
            if record.get("payload", {}).get("op") == "ping":
                break
        else:
            raise AssertionError("ping diagnostic payload was not received")
        assert record["target"] == "peer" and record["event"] == "message.received"
        assert record["context"]["peer_id"] == keys[0]
        assert secret not in json.dumps(record)
        assert a.poll() is None and b.poll() is None
        print("PASS authenticated shell attach enables live filtered records and redacts credentials without restart", flush=True)

        before = status()["emitted"]
        time.sleep(1.2)
        assert status()["emitted"] <= before + 2, "log transport recursively generated its own records"
        print("PASS quiet subscription has no logging feedback loop", flush=True)

        path = pathlib.Path(temporary) / "first.jsonl"
        remote("log-file", str(path), "--targets", "peer", "--events", "message.received")
        ping("CONTENT_ONLY_WITH_PAYLOAD_CAPTURE")
        path2 = pathlib.Path(temporary) / "second.jsonl"
        remote("log-file", str(path2), "--targets", "workload,runtime", "--level", "trace")
        first = file_records(path)
        assert first and all(record["target"] == "peer" for record in first)
        assert "CONTENT_ONLY_WITH_PAYLOAD_CAPTURE" not in path.read_text()
        assert all("payload" not in record for record in first)
        print("PASS independent file sink, metadata-only capture, hot destination/filter switch", flush=True)

        run = json.loads(remote("start", temporary, "fixture", "SLOW"))
        run_id = run["record"]["spec"]["id"]
        observer = subprocess.Popen(command + ["--data-dir", directories[0], "watch", address, keys[1], run_id],
                                    stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
        processes.append(observer)
        time.sleep(1.2)
        remote("cancel", run_id)
        observer.wait(timeout=5)
        assert observer.returncode == 0
        remote("log-off")
        stream.wait(timeout=5)
        records = file_records(path2)
        assert records and all(record["target"] in ["workload", "runtime"] for record in records)
        assert any(record["context"].get("run_id") == run_id for record in records)
        assert sum(record["event"] == "events.read" for record in records) < 200, "idle workload watch is busy-spinning"
        assert status()["enabled"] is False
        size = path2.stat().st_size
        ping("logging remains off")
        assert path2.stat().st_size == size
        print("PASS runtime/workload correlation, file drain, hot off closes subscriptions, execution remains responsive", flush=True)

        # A crashed diagnostic consumer must not leave logging on indefinitely.
        leaked = subprocess.Popen(command + ["--data-dir", directories[0], "logs", address, keys[1]],
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        processes.append(leaked)
        deadline = time.monotonic() + 5
        while not status()["enabled"]:
            assert time.monotonic() < deadline
            time.sleep(.02)
        leaked.terminate()
        leaked.wait(timeout=5)
        deadline = time.monotonic() + 15
        while status()["enabled"]:
            assert time.monotonic() < deadline, "disconnected subscriber retained"
            time.sleep(.1)
        print("PASS disconnected consumer is removed automatically", flush=True)

        remote("log-file", str(path2))
        b.terminate()
        b.wait(timeout=5)
        b = daemon(1)
        assert status()["enabled"] is False
        print("PASS diagnostics reset to off after daemon restart", flush=True)
    finally:
        for process in processes:
            if process.poll() is None:
                process.terminate()
            process.wait(timeout=5)
