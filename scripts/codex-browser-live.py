#!/usr/bin/env python3
"""Exercise the assignment browser through a disposable Boxen and real Codex."""

import argparse
import http.server
import json
import os
from pathlib import Path
import runpy
import secrets
import socket
import subprocess
import threading
import time
import urllib.request


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ["boxen", "browser-server", "agent-browser", "executable", "scratch"]:
        parser.add_argument("--" + name, required=True, type=lambda p: Path(p).resolve())
    parser.add_argument("--codex", default="auto")
    args = parser.parse_args()
    args.scratch.mkdir()
    home = args.scratch / "boxen"
    fixture = Path(__file__).resolve().parent.parent / "addons/browser/tests/mcp-smoke.py"
    handler = runpy.run_path(str(fixture))["Fixture"]
    with http.server.ThreadingHTTPServer(("127.0.0.1", 0), handler) as site:
        threading.Thread(target=site.serve_forever, daemon=True).start()
        try:
            run(args, home, f"http://127.0.0.1:{site.server_port}/")
        finally:
            site.shutdown()


def run(args, home, url):
    command = [str(args.boxen), "--data-dir", str(home)]
    machine = subprocess.check_output(command + ["identity"], text=True, timeout=15).strip()
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        ui = listener.getsockname()[1]
    with (args.scratch / "daemon-stderr.log").open("w") as stderr:
        daemon = subprocess.Popen(command + ["serve", "--listen", "127.0.0.1:0",
                                  "--ui", f"127.0.0.1:{ui}", "--codex", args.codex],
                                  stdout=subprocess.PIPE, stderr=stderr, text=True)
        try:
            control = [daemon.stdout.readline() for _ in range(3)][-1]
            assert "Local controls:" in control, "test daemon did not start"
            token = control.strip().split("#")[1]

            def api(operation, watch=False):
                request = urllib.request.Request(
                    f"http://127.0.0.1:{ui}/api/" + ("watch" if watch else "operation"),
                    data=json.dumps({"peer": machine, "address": "", "request": operation}).encode(),
                    headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"},
                )
                response = urllib.request.urlopen(request, timeout=150)
                if watch:
                    return response
                with response:
                    return json.load(response)

            resident, sender, job = (secrets.token_hex(32) for _ in range(3))
            api({"op": "resident_create", "spec": {"id": resident, "name": "Browser fixture",
                 "backend": "codex", "cwd": str(args.scratch), "auto_activate": False}})
            registry = home / "browser-assignments"
            registry.mkdir()
            native = None
            for version in [1, 2, 3]:
                scoped = version != 2
                message = secrets.token_hex(32)
                evidence = args.scratch / f"evidence-{version}"
                if scoped:
                    grant = {"message": message, "resident": resident, "sender_machine": machine,
                             "sender_id": sender, "job": job, "version": version,
                             "server": str(args.browser_server),
                             "settings": {"executable": str(args.executable), "agent_browser": str(args.agent_browser),
                                          "profile": str(args.scratch / "profile"), "evidence": str(evidence),
                                          "headless": True}}
                    (registry / f"{message}.json").write_text(json.dumps(grant), encoding="utf-8")
                prompt = (f"Use the boxen_browser MCP browser_action tool to open {url}, then use "
                          "browser_action screenshot and report its path. This is a local disposable fixture. "
                          "No shell or other browser tools are needed." if scoped else
                          "Finish with a private line saying NO BROWSER REQUEST. This activation needs no tools.")
                api({"op": "mailbox_send", "client_id": sender, "message": {"id": message,
                     "to": {"type": "resident", "id": resident}, "subject": "Browser fixture", "body": prompt}})
                start = api({"op": "resident_activate", "id": resident, "activation_id": secrets.token_hex(32)})
                run_id = start["record"]["spec"]["id"]
                events = []
                with api({"id": run_id, "after": 0}, watch=True) as stream:
                    for line in stream:
                        batch = json.loads(line)
                        events.extend(batch["events"])
                        if batch["done"]:
                            break
                (args.scratch / f"run-{version}.json").write_text(json.dumps(events), encoding="utf-8")
                assert batch["snapshot"]["status"] == "completed", batch["snapshot"]["status"]
                current = batch["snapshot"]["backend_thread"]
                assert current and native in (None, current), "the Codex thread was replaced"
                native = current
                ready = [event for event in events if event["kind"] == "browser_assignment_ready"]
                assert bool(ready) == scoped, ready
                if scoped:
                    assert ready[0]["data"]["version"] == version
                    shots = list(evidence.glob("*.png"))
                    assert len(shots) == 1 and shots[0].read_bytes().startswith(b"\x89PNG\r\n\x1a\n")
                    endpoint = args.scratch / "profile/DevToolsActivePort"
                    port = int(endpoint.read_text().splitlines()[0])
                    deadline = time.monotonic() + 20
                    while time.monotonic() < deadline:
                        with socket.socket() as probe:
                            probe.settimeout(1)
                            if probe.connect_ex(("127.0.0.1", port)) != 0:
                                break
                        time.sleep(.2)
                    else:
                        raise AssertionError("the activation left a browser debugger listener")
                else:
                    assert not evidence.exists()
                print(f"Activation {version} completed (assigned browser {scoped})", flush=True)
        finally:
            if daemon.poll() is None:
                if os.name == "nt":
                    subprocess.run(["taskkill", "/PID", str(daemon.pid), "/T", "/F"],
                                   stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
                else:
                    daemon.terminate()
                daemon.wait(timeout=10)


if __name__ == "__main__":
    main()
