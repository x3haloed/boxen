#!/usr/bin/env python3
"""Exercise the shipped CLI and local control surface using two real daemons."""
import json
import pathlib
import socket
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request


def port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


binary = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/boxen").resolve()
executable = ["/bin/bash", str(binary)] if binary.suffix == ".com" else [str(binary)]
processes = []
with tempfile.TemporaryDirectory(prefix="boxen-smoke-") as temporary:
    roots = [str(pathlib.Path(temporary) / name) for name in ["a", "b"]]

    def cli(index, *args, check=True):
        return subprocess.run(executable + ["--data-dir", roots[index], *args], text=True,
                              capture_output=True, check=check, timeout=20)

    keys = [cli(i, "identity").stdout.strip() for i in range(2)]
    assert keys[0] != keys[1]
    cli(0, "trust", keys[1], "B")
    cli(1, "trust", keys[0], "A")
    peer_ports, ui_ports = [port(), port()], [port(), port()]

    def start(index):
        process = subprocess.Popen(executable + ["--data-dir", roots[index], "serve", "--listen",
                                   f"127.0.0.1:{peer_ports[index]}", "--ui", f"127.0.0.1:{ui_ports[index]}"],
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        processes.append(process)
        lines = [process.stdout.readline().strip() for _ in range(3)]
        assert "Local controls:" in lines[2], lines
        return lines[2].split("#", 1)[1]

    try:
        tokens = [start(0), start(1)]
        for source, destination in [(0, 1), (1, 0)]:
            reply = json.loads(cli(source, "ping", f"127.0.0.1:{peer_ports[destination]}",
                                   keys[destination], "hello from the other box 🐈").stdout)
            assert reply["from"] == keys[destination]
            assert reply["message"] == "hello from the other box 🐈"
        print("PASS real CLI exchange in both directions")

        base = f"http://127.0.0.1:{ui_ports[0]}"
        page = urllib.request.urlopen(base, timeout=5).read()
        assert b"Send test message" in page
        try:
            urllib.request.urlopen(base + "/api/status", timeout=5)
            raise AssertionError("unauthenticated status accepted")
        except urllib.error.HTTPError as error:
            assert error.code == 401
        headers = {"Authorization": "Bearer " + tokens[0], "Content-Type": "application/json"}
        request = urllib.request.Request(base + "/api/ping", headers=headers, data=json.dumps({
            "address": f"127.0.0.1:{peer_ports[1]}", "peer": keys[1], "message": "from local controls"
        }).encode())
        assert json.load(urllib.request.urlopen(request, timeout=15))["message"] == "from local controls"
        request = urllib.request.Request(base + "/api/status", headers={**headers, "Host": "attacker.invalid"})
        try:
            urllib.request.urlopen(request, timeout=5)
            raise AssertionError("foreign host accepted")
        except urllib.error.HTTPError as error:
            assert error.code == 403
        print("PASS embedded controls, authenticated API, foreign-host rejection")

        processes[1].terminate()
        processes[1].wait(timeout=5)
        assert cli(0, "ping", f"127.0.0.1:{peer_ports[1]}", keys[1], check=False).returncode != 0
        start(1)
        assert cli(1, "identity").stdout.strip() == keys[1]
        cli(0, "ping", f"127.0.0.1:{peer_ports[1]}", keys[1], "after restart")
        cli(1, "untrust", keys[0])
        assert cli(0, "ping", f"127.0.0.1:{peer_ports[1]}", keys[1], "revoked", check=False).returncode != 0
        print("PASS unreachable peer, restart with retained pairing, live revocation")
    finally:
        for process in processes:
            if process.poll() is None:
                process.terminate()
            process.wait(timeout=5)
