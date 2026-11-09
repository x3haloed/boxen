"""Live local MCP test with a fresh profile, synthetic site, and real browser."""

import argparse
import contextlib
import http.server
import json
from pathlib import Path
import re
import socket
import subprocess
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
    def __init__(self, proc):
        self.proc = proc
        self.id = 0
        assert self.call("initialize", {"protocolVersion": "2025-03-26"})["protocolVersion"] == "2025-03-26"
        self.proc.stdin.write('{"jsonrpc":"2.0","method":"notifications/initialized"}\n')
        self.proc.stdin.flush()
        assert {t["name"] for t in self.call("tools/list", {})["tools"]} == {
            "browser_action", "browser_cdp", "browser_events"
        }

    def call(self, method, params):
        self.id += 1
        self.proc.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.id, "method": method, "params": params}) + "\n")
        self.proc.stdin.flush()
        response = json.loads(self.proc.stdout.readline())
        assert response["id"] == self.id and "error" not in response, response
        return response["result"]

    def tool(self, name, **args):
        result = self.call("tools/call", {"name": name, "arguments": args})
        text = result["content"][0]["text"]
        assert not result["isError"], text
        return json.loads(text)

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


def main():
    parser = argparse.ArgumentParser()
    for flag in ["boxen-browser", "agent-browser", "executable", "scratch"]:
        parser.add_argument("--" + flag, required=True, type=lambda p: Path(p).resolve())
    args = parser.parse_args()
    args.scratch.mkdir()
    evidence = args.scratch / "evidence"
    profile = args.scratch / "profile"
    settings = args.scratch / "config.json"
    settings.write_text(json.dumps({"executable": str(args.executable), "agent_browser": str(args.agent_browser),
                                    "evidence": str(evidence), "profile": str(profile), "headless": True}), encoding="utf-8")
    with http.server.ThreadingHTTPServer(("127.0.0.1", 0), Fixture) as site:
        worker = threading.Thread(target=site.serve_forever, daemon=True)
        worker.start()
        origin = f"http://127.0.0.1:{site.server_port}"
        try:
            with subprocess.Popen([args.boxen_browser, "mcp", settings], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, text=True, encoding="utf-8") as proc:
                try:
                    mcp = Mcp(proc)
                    assert not profile.exists(), "MCP discovery must not launch the browser"
                    exercise(mcp, origin, evidence)
                    print(mcp.cdp("Browser.getVersion")["product"], "passed the local MCP smoke")
                finally:
                    with contextlib.suppress(BrokenPipeError):
                        proc.stdin.close()
                    try:
                        code = proc.wait(timeout=40)
                    except subprocess.TimeoutExpired:
                        proc.kill()
                        raise
                assert code == 0, code
                port = int((profile / "DevToolsActivePort").read_text().splitlines()[0])
                with socket.socket() as probe:
                    probe.settimeout(2)
                    assert probe.connect_ex(("127.0.0.1", port)) != 0, "MCP shutdown left a debugger listener"
        finally:
            site.shutdown()


if __name__ == "__main__":
    main()
