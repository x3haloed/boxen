"""Live local MCP test with a fresh profile, synthetic site, and real browser."""

import argparse
import contextlib
import http.server
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import sys
import threading
import time
import urllib.request


HTML = b"""<!doctype html><title>Browser fixture</title>
<label>Sample <input></label><button id="calculate">Calculate</button>
<output id="result"></output><a href="/other">Other page</a>
<script src="/app.js"></script>"""
SOURCE = """document.querySelector("#calculate").addEventListener("click", () => {
    const score: number = 41;
    window.answer = score + 1;
    document.querySelector("#result").textContent = String(window.answer);
});"""
JS = ('"use strict";\n' + SOURCE.replace(": number", "") + "\n//# sourceMappingURL=app.js.map\n").encode()
MAP = json.dumps({"version": 3, "file": "app.js", "sources": ["app.ts"],
                  "sourcesContent": [SOURCE], "names": [], "mappings": ";AAAA;AACA;AACA;AACA;AACA"}).encode()


class Fixture(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        content_type, data = {
            "/": ("text/html", HTML),
            "/other": ("text/html", b"<!doctype html><title>Other page</title><p>Other page</p>"),
            "/app.js": ("application/javascript", JS),
            "/app.js.map": ("application/json", MAP),
        }.get(self.path, ("text/plain", b""))
        self.send_response(200)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, *_args):
        pass


class Mcp:
    def __init__(self, proc, safari=False):
        self.proc = proc
        self.id = 0
        version = "2024-11-05" if safari else "2025-03-26"
        assert self.call("initialize", {"protocolVersion": version, "capabilities": {},
                                       "clientInfo": {"name": "boxen-fixture", "version": "1"}})["protocolVersion"] == version
        self.proc.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
        self.proc.stdin.flush()
        tools = {t["name"] for t in self.call("tools/list", {})["tools"]}
        if safari:
            assert {"navigate_to_url", "get_page_content", "page_interactions", "screenshot"} <= tools
        else:
            assert tools == {"browser_action", "browser_cdp", "browser_events"}

    def call(self, method, params):
        self.id += 1
        self.proc.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params}) + "\n")
        self.proc.stdin.flush()
        response = json.loads(self.proc.stdout.readline())
        assert response["id"] == self.id and "error" not in response, response
        return response["result"]

    def tool_text(self, name, **args):
        result = self.call("tools/call", {"name": name, "arguments": args})
        text = result["content"][0]["text"]
        assert not result.get("isError"), text
        return text

    def tool(self, name, **args):
        return json.loads(self.tool_text(name, **args))

    def act(self, action, **args):
        return self.tool("browser_action", action=action, **args)

    def cdp(self, method, session_id=None, **params):
        args = {"method": method, "params": params}
        if session_id:
            args["session_id"] = session_id
        response = self.tool("browser_cdp", **args)
        assert "error" not in response, response
        return response["result"]

    def wait_event(self, method, cursor, session, predicate=lambda _p: True):
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            batch = self.tool("browser_events", after=cursor, wait_ms=1000)
            assert batch["dropped_through"] <= cursor
            for entry in batch["events"]:
                event = entry["event"]
                if event.get("sessionId") == session and event["method"] == method and predicate(event["params"]):
                    return entry["cursor"], event["params"]
            cursor = batch["cursor"]
        raise AssertionError(f"missing event {method}")


def exercise(mcp, origin, evidence):
    mcp.act("open", url=origin + "/")
    snapshot = mcp.act("snapshot", interactive=True)["data"]["snapshot"]
    field = re.search(r'textbox "Sample" \[ref=(e\d+)\]', snapshot).group(1)
    mcp.act("fill", selector="@" + field, text="--literal input")
    mcp.act("press", key="Tab")
    targets = mcp.cdp("Target.getTargets")["targetInfos"]
    page = next(t for t in targets if t["url"] == origin + "/")
    session = mcp.cdp("Target.attachToTarget", targetId=page["targetId"], flatten=True)["sessionId"]
    read = mcp.cdp("Runtime.evaluate", session, expression="[document.querySelector('input').value, document.activeElement.id]", returnByValue=True)
    assert read["result"]["value"] == ["--literal input", "calculate"], read

    mcp.cdp("Debugger.enable", session)
    cursor, parsed = mcp.wait_event("Debugger.scriptParsed", 0, session,
                                    lambda p: p.get("url") == origin + "/app.js")
    assert parsed["sourceMapURL"] == "app.js.map"
    # For this fixture, the source map has one zero-column segment per line.
    # The initial empty segment is the transpiler's strict-mode prelude.
    with urllib.request.urlopen(origin + "/" + parsed["sourceMapURL"], timeout=5) as response:
        mapping = json.load(response)
    assert mapping["sources"][0] == "app.ts" and "score: number" in mapping["sourcesContent"][0]
    mapped_lines = mapping["mappings"].split(";")
    assert mapped_lines == ["", "AAAA", "AACA", "AACA", "AACA", "AACA"]
    original_line = 2  # Zero-based TypeScript assignment to window.answer.
    generated_line = [i for i, line in enumerate(mapped_lines) if line][original_line]
    breakpoint = mcp.cdp("Debugger.setBreakpointByUrl", session,
                         url=parsed["url"], lineNumber=generated_line, condition="score === 41")
    mcp.cdp("Runtime.evaluate", session, expression="setTimeout(() => document.querySelector('#calculate').click(), 100)")
    cursor, paused = mcp.wait_event("Debugger.paused", cursor, session)
    assert breakpoint["breakpointId"] in paused["hitBreakpoints"]
    local = mcp.cdp("Debugger.evaluateOnCallFrame", session,
                    callFrameId=paused["callFrames"][0]["callFrameId"], expression="score", returnByValue=True)
    assert local["result"]["value"] == 41
    mcp.cdp("Debugger.stepOver", session)
    _, step = mcp.wait_event("Debugger.paused", cursor, session)
    assert step["callFrames"][0]["location"]["lineNumber"] == generated_line + 1
    mcp.cdp("Debugger.resume", session)
    mcp.cdp("Debugger.disable", session)
    assert "42" in mcp.act("snapshot")["data"]["snapshot"]
    shot = Path(mcp.act("screenshot")["path"])
    assert shot.resolve().parent == evidence and shot.read_bytes().startswith(b"\x89PNG\r\n\x1a\n")
    snapshot = mcp.act("snapshot", interactive=True)["data"]["snapshot"]
    link = re.search(r'link "Other page" \[ref=(e\d+)\]', snapshot).group(1)
    mcp.act("click", selector="@" + link)
    assert "Other page" in mcp.act("snapshot")["data"]["snapshot"]
    mcp.act("back")
    assert "Calculate" in mcp.act("snapshot")["data"]["snapshot"]


def exercise_safari(mcp, origin, evidence):
    assert "Calculate" in mcp.tool_text("navigate_to_url", url=origin + "/")
    snapshot = json.loads(mcp.tool_text("get_page_content"))["content"]
    assert "Sample" in snapshot and "Calculate" in snapshot, snapshot
    field = re.search(r"uid=(\d+).*Sample", snapshot).group(1)
    button = re.search(r"uid=(\d+).*Calculate", snapshot).group(1)
    mcp.tool_text("page_interactions", interactions=[
        {"type": "type", "node": field, "value": "--literal input", "purpose": "Enter fixture text"},
        {"type": "click", "node": button, "purpose": "Calculate fixture result"},
    ])
    output = mcp.tool_text("evaluate_javascript", expression=
                          "return [document.querySelector('input').value, document.querySelector('output').textContent];")
    assert json.loads(output) == ["--literal input", "42"], output
    mcp.tool_text("evaluate_javascript", expression="console.log('boxen-safari-fixture');")
    assert "boxen-safari-fixture" in mcp.tool_text("browser_console_messages")
    # Safari starts buffering network requests after the first inspection.
    mcp.tool_text("list_network_requests")
    assert mcp.tool_text("evaluate_javascript", expression=
                         "return await fetch('/other').then(response => response.status);") == "200"
    network = json.loads(mcp.tool_text("list_network_requests"))
    assert any(r["url"] == origin + "/other" and r["status"] == 200 for r in network["requests"]), network
    outside = evidence.parent / "outside.png"
    response = mcp.tool_text("screenshot", savePath=str(outside))
    shots = list(evidence.glob("*.png"))
    assert len(shots) == 1 and str(shots[0]) in response and not outside.exists(), response
    assert shots[0].read_bytes().startswith(b"\x89PNG\r\n\x1a\n")


def chrome_process_group(port):
    pid = int(subprocess.check_output(["/usr/sbin/lsof", "-nP", "-t", f"-iTCP@127.0.0.1:{port}",
                                       "-sTCP:LISTEN"], text=True).strip())
    pids = [int(row.split()[0]) for row in subprocess.check_output(
        ["/bin/ps", "-axo", "pid=,pgid="], text=True).splitlines()
        if int(row.split()[1]) == pid]
    assert len(pids) > 1, "expected Chrome and its helpers in the owned process group"
    return pids


def child_pids(pid):
    return [int(row.split()[0]) for row in subprocess.check_output(
        ["/bin/ps", "-axo", "pid=,ppid="], text=True).splitlines()
        if int(row.split()[1]) == pid]


def wait_processes_closed(pids):
    deadline = time.monotonic() + 10
    while any(pid_exists(pid) for pid in pids):
        assert time.monotonic() < deadline, "Owned browser processes survived MCP owner exit"
        time.sleep(.1)


def pid_exists(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


def main():
    parser = argparse.ArgumentParser()
    for flag in ["boxen-browser", "executable", "scratch"]:
        parser.add_argument("--" + flag, required=True, type=lambda p: Path(p).resolve())
    parser.add_argument("--agent-browser", type=lambda p: Path(p).resolve())
    parser.add_argument("--safari", action="store_true")
    parser.add_argument("--headed", action="store_true")
    args = parser.parse_args()
    if not args.safari and args.agent_browser is None:
        parser.error("Chromium requires --agent-browser")
    args.scratch.mkdir()
    evidence = args.scratch / "evidence"
    profile = args.scratch / "profile"
    settings = args.scratch / "config.json"
    engine = ({"backend": "safari"} if args.safari else
              {"agent_browser": str(args.agent_browser), "headless": not args.headed})
    settings.write_text(json.dumps({"executable": str(args.executable), "evidence": str(evidence),
                                    "profile": str(profile), **engine}), encoding="utf-8")
    command = [args.boxen_browser, "mcp", settings]
    with http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture) as site:
        worker = threading.Thread(target=site.serve_forever, daemon=True)
        worker.start()
        origin = f"http://127.0.0.1:{site.server_port}"
        try:
            with subprocess.Popen(command, stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, text=True, encoding="utf-8") as proc:
                try:
                    mcp = Mcp(proc, safari=args.safari)
                    assert not profile.exists(), "MCP discovery must not launch the browser"
                    if args.safari:
                        exercise_safari(mcp, origin, evidence)
                        print("Safari passed the local MCP smoke")
                    else:
                        exercise(mcp, origin, evidence)
                        print(mcp.cdp("Browser.getVersion")["product"], "passed the local MCP smoke")
                    with subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                          text=True, encoding="utf-8") as contender:
                        try:
                            other = Mcp(contender, safari=args.safari)
                            name, arguments = (("page_info", {}) if args.safari else
                                               ("browser_cdp", {"method": "Browser.getVersion"}))
                            rejected = other.call("tools/call", {"name": name, "arguments": arguments})
                            assert rejected.get("isError") and "reservation" in rejected["content"][0]["text"]
                        finally:
                            close_mcp(contender)
                    if args.safari:
                        assert "boxen_fixture=temporary" in mcp.tool_text("evaluate_javascript", expression=
                            "document.cookie = 'boxen_fixture=temporary; Max-Age=3600'; return document.cookie;")
                    else:
                        mcp.cdp("Storage.setCookies", cookies=[{
                            "name": "boxen_fixture", "value": "retained", "url": origin,
                            "expires": int(time.time()) + 3600
                        }])
                finally:
                    close_mcp(proc)
                if not args.safari:
                    wait_listener_closed(debug_port(profile))
                    with subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                          text=True, encoding="utf-8") as reopened:
                        try:
                            cookies = Mcp(reopened).cdp("Storage.getCookies")["cookies"]
                            assert any(c["name"] == "boxen_fixture" and c["value"] == "retained" for c in cookies)
                        finally:
                            close_mcp(reopened)
                    print("Profile contention and reuse passed")
                else:
                    with subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                          text=True, encoding="utf-8") as reopened:
                        try:
                            fresh = Mcp(reopened, safari=True)
                            fresh.tool_text("navigate_to_url", url=origin + "/")
                            assert "boxen_fixture" not in fresh.tool_text("evaluate_javascript",
                                                                          expression="return document.cookie;")
                            driver, = child_pids(reopened.pid)
                        finally:
                            reopened.kill()
                            reopened.wait(timeout=10)
                    wait_processes_closed([driver])
                    print("Safari reservation, session isolation, and owner-exit cleanup passed")
        finally:
            site.shutdown()
    if not args.safari and (os.name == "nt" or sys.platform == "darwin"):
        # Killing MCP leaves cleanup to the Windows Job Object or macOS owner.
        with subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                              text=True, encoding="utf-8") as proc:
            try:
                Mcp(proc).cdp("Browser.getVersion")
                port = debug_port(profile)
                pids = chrome_process_group(port) if sys.platform == "darwin" else []
            finally:
                proc.kill()
                proc.wait(timeout=10)
            wait_listener_closed(port)
        wait_processes_closed(pids)
        with subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                              text=True, encoding="utf-8") as proc:
            try:
                Mcp(proc).cdp("Browser.getVersion")
                port = debug_port(profile)
            finally:
                close_mcp(proc)
            wait_listener_closed(port)
        print("Abrupt browser owner exit closed the listener and released the profile")


def debug_port(profile):
    return int((profile / "DevToolsActivePort").read_text().splitlines()[0])


def close_mcp(proc):
    with contextlib.suppress(BrokenPipeError):
        proc.stdin.close()
    try:
        code = proc.wait(timeout=40)
    except subprocess.TimeoutExpired:
        proc.kill()
        raise
    assert code == 0, code


def wait_listener_closed(port):
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        with socket.socket() as probe:
            probe.settimeout(1)
            if probe.connect_ex(("127.0.0.1", port)) != 0:
                return
        time.sleep(.1)
    raise AssertionError("MCP shutdown left a debugger listener")


if __name__ == "__main__":
    main()
