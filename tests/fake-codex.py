#!/usr/bin/env python3
"""Deterministic app-server fixture. Never used by Boxen outside tests."""
import json
import sys
import threading
import time

lock = threading.Lock()
cancel = threading.Event()
approved = threading.Event()
thread = "fixture-thread"


def emit(message):
    with lock:
        print(json.dumps(message), flush=True)


def notification(method, params):
    emit({"method": method, "params": {"threadId": thread, "turnId": "fixture-turn", **params}})


def work(prompt):
    notification("turn/started", {"turn": {"id": "fixture-turn"}})
    notification("item/started", {"item": {"id": "thought", "type": "reasoning", "summary": []}})
    notification("item/reasoning/summaryTextDelta", {"itemId": "thought", "delta": "Checking the remote workspace."})
    notification("item/started", {"item": {"id": "command", "type": "commandExecution", "command": "fixture command", "status": "inProgress"}})
    notification("item/commandExecution/outputDelta", {"itemId": "command", "delta": "first output\n"})
    if "APPROVAL" in prompt:
        emit({"id": "approval-1", "method": "item/commandExecution/requestApproval", "params": {"threadId": thread, "turnId": "fixture-turn", "command": "fixture command", "itemId": "command"}})
        while not approved.wait(0.02) and not cancel.is_set():
            pass
        notification("serverRequest/resolved", {"requestId": "approval-1"})
    for number in range(30 if "SLOW" in prompt else 5):
        if cancel.wait(0.1):
            break
        notification("item/commandExecution/outputDelta", {"itemId": "command", "delta": f"tick {number}\n"})
    if "LARGE" in prompt:
        notification("item/commandExecution/outputDelta", {"itemId": "command", "delta": "🐈" * 50000})
    status = "interrupted" if cancel.is_set() else "completed"
    notification("item/completed", {"item": {"id": "command", "type": "commandExecution", "command": "fixture command", "status": status, "aggregatedOutput": "fixture output"}})
    notification("item/started", {"item": {"id": "answer", "type": "agentMessage", "text": ""}})
    notification("item/agentMessage/delta", {"itemId": "answer", "delta": "Fixture result"})
    notification("item/completed", {"item": {"id": "answer", "type": "agentMessage", "text": "Fixture result"}})
    notification("turn/completed", {"turn": {"id": "fixture-turn", "status": status, "error": None}})


for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method is None:
        approved.set()
        continue
    if method == "initialized":
        continue
    if method == "initialize":
        result = {"userAgent": "fixture"}
    elif method in ["thread/start", "thread/resume"]:
        assert request["params"]["sandbox"] == "danger-full-access"
        assert request["params"]["approvalPolicy"] == "never"
        result = {"thread": {"id": thread}, "model": "fixture"}
    elif method == "turn/start":
        result = {"turn": {"id": "fixture-turn", "status": "inProgress"}}
        emit({"id": request["id"], "result": result})
        threading.Thread(target=work, args=(request["params"]["input"][0]["text"],), daemon=True).start()
        continue
    elif method == "turn/interrupt":
        cancel.set()
        result = {}
    elif method == "turn/steer":
        result = {"turnId": "fixture-turn"}
    else:
        raise AssertionError(method)
    emit({"id": request["id"], "result": result})
