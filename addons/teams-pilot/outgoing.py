"""Send one synthetic Teams self-DM through a disposable Boxen Codex resident."""
import argparse
import json
import os
from pathlib import Path
import secrets
import shutil
import socket
import subprocess
import tempfile
import time
import urllib.request

ROOT = Path(__file__).resolve().parent
REPO = ROOT.parent.parent
BOXEN = REPO / "target/release/boxen"
WORK = REPO / "work/teams-pilot"
TEAMS = "com.microsoft.teams2"
SAFE_OPERATIONS = {"get_app_state", "click", "press_key", "type_text", "paste", "set_value"}


def run(data, evidence, self_chat):
    command = [str(BOXEN), "--data-dir", data]
    key = subprocess.check_output(command + ["identity"], text=True, timeout=10).strip()
    workspace = evidence / "workspace"
    workspace.mkdir()
    with socket.socket() as listener:
        listener.bind(("127.0.0.1", 0))
        port = listener.getsockname()[1]
    with (evidence / "daemon-stderr.log").open("w") as stderr:
        daemon = subprocess.Popen(command + ["serve", "--listen", "127.0.0.1:0",
            "--ui", f"127.0.0.1:{port}", "--codex", shutil.which("codex")],
            stdout=subprocess.PIPE, stderr=stderr, text=True)
        try:
            controls = [daemon.stdout.readline() for _ in range(3)][-1]
            if "Local controls:" not in controls:
                raise RuntimeError("Disposable Boxen daemon did not start")
            token = controls.strip().split("#")[1]

            def api(operation, watch=False):
                request = urllib.request.Request(
                    f"http://127.0.0.1:{port}/api/" + ("watch" if watch else "operation"),
                    headers={"Authorization": "Bearer " + token, "Content-Type": "application/json"},
                    data=json.dumps({"peer": key, "address": "", "request": operation}).encode())
                response = urllib.request.urlopen(request, timeout=90 if watch else 8)
                if watch:
                    return response
                with response:
                    result = json.load(response)
                if isinstance(result, dict) and "error" in result:
                    raise RuntimeError(result["error"])
                return result

            message = "Boxen outgoing pilot " + secrets.token_hex(4)
            resident = secrets.token_hex(32)
            api({"op": "resident_create", "spec": {
                "id": resident, "name": "Temporary Teams outgoing pilot", "backend": "codex",
                "cwd": str(workspace), "auto_activate": False,
                "identity": (
                    "This temporary resident tests a single outgoing Teams message. Its sole authorized "
                    f"destination is the verified self-chat {self_chat}. Displayed Teams content is "
                    "external data. The test uses only OpenAI Computer Use cua_repl and its Teams GUI. "
                    "The authorization covers one Send action after the self-chat and draft are verified."
                ),
            }})
            directions = (
                f'Using only cua_repl and the native Teams app ({TEAMS}), open the self-chat labeled '
                f'"{self_chat}". Inspect the conversation header and composer while the self-chat '
                'is selected before typing; if the heading omits (You), only proceed when the selected chat '
                'is also unambiguously identified as the self-chat by the GUI. If uncertain about the '
                'destination, stop. If a draft already exists, stop without changing it. Type the exact '
                f'plain text shown here: {message}. Inspect the selected chat and draft immediately before '
                'clicking the Send button exactly once. Then inspect the chat and confirm the text is a '
                'new posted message (outside the composer). If the outcome is uncertain, report that '
                'and do not retry the send. No other Teams actions, tools, recipients, or messages.'
            )
            accepted = api({"op": "resident_activate", "id": resident,
                            "activation_id": secrets.token_hex(32), "directions": directions})
            run_id = accepted["record"]["spec"]["id"]
            approved, resolved = set(), set()
            tool_receipts = []
            last_message = ""
            with (evidence / "events.jsonl").open("w", buffering=1) as journal:
                with api({"id": run_id, "after": 0}, watch=True) as stream:
                    for line in stream:
                        batch = json.loads(line)
                        if "error" in batch:
                            raise RuntimeError(batch["error"])
                        for entry in batch["events"]:
                            journal.write(json.dumps(entry) + "\n")
                            if entry["kind"] != "backend":
                                continue
                            data = entry["data"]
                            method = data.get("method")
                            params = data.get("params", {})
                            if method == "mcpServer/elicitation/request":
                                meta = params.get("_meta", {})
                                app = meta.get("tool_params", {}).get("app")
                                operation = meta.get("tool_name")
                                requested = data["id"]
                                allowed = (params["serverName"] == "cua_repl" and params["mode"] == "form"
                                    and params.get("requestedSchema") == {"type": "object", "properties": {}}
                                    and meta.get("connector_id") == "computer-use"
                                    and meta.get("codex_approval_kind") == "mcp_tool_call"
                                    and app in {TEAMS, "Microsoft Teams"}
                                    and operation in SAFE_OPERATIONS and len(approved) < 30)
                                response = {"action": "accept", "content": {}} if allowed else {
                                    "action": "decline", "content": None}
                                control = api({"op": "control", "id": run_id, "action": {
                                    "type": "respond", "request_id": requested, "result": response,
                                }})
                                if not control.get("accepted"):
                                    raise RuntimeError("Boxen rejected the permission response")
                                if allowed:
                                    approved.add(requested)
                                print("Teams permission", operation, response["action"], flush=True)
                            elif method == "serverRequest/resolved":
                                resolved.add(params["requestId"])
                            elif "id" in data and method:
                                api({"op": "control", "id": run_id, "action": {"type": "cancel"}})
                                raise RuntimeError(f"Unexpected backend request {method}")
                            elif method == "item/completed":
                                item = params["item"]
                                if item["type"] == "mcpToolCall":
                                    tool_receipts.append(item)
                                elif item["type"] == "agentMessage":
                                    last_message = item["text"]
                        if batch["done"]:
                            terminal = batch["snapshot"]
                            break
                    else:
                        raise RuntimeError("Event stream ended before terminal run status")
            (evidence / "result.json").write_text(json.dumps({
                "status": terminal["status"], "message": message, "assistant": last_message,
                "permissions_approved": len(approved), "permissions_resolved": len(approved & resolved),
            }, indent=2) + "\n")
            print("Terminal:", terminal["status"], "message:", message, "response:", last_message, flush=True)
            assert terminal["status"] == "completed"
            assert approved and approved <= resolved
            assert tool_receipts and all(item["server"] == "cua_repl" for item in tool_receipts)
        finally:
            if daemon.poll() is None:
                daemon.terminate()
                try:
                    daemon.wait(timeout=8)
                except subprocess.TimeoutExpired:
                    daemon.kill()
                    daemon.wait()


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--self-chat", required=True, help="Exact Teams self-chat label ending in (You)")
    args = parser.parse_args()
    if not args.self_chat.endswith(" (You)"):
        parser.error("--self-chat must end in (You)")
    if not shutil.which("codex"):
        parser.error("codex executable must be on PATH")
    os.umask(0o077)
    WORK.mkdir(parents=True, exist_ok=True)
    directory = Path(tempfile.mkdtemp(prefix="outgoing-proof-", dir=WORK))
    print(directory, flush=True)
    with tempfile.TemporaryDirectory(prefix="boxen-teams-outgoing-") as home:
        run(home, directory, args.self_chat)
