#!/usr/bin/env python3
"""Deterministic app-server fixture. Never used by Boxen outside tests."""
import json
import pathlib
import sys
import threading
import time

lock = threading.Lock()
cancel = threading.Event()
approved = threading.Event()
thread = "fixture-thread"
responses = {}
response_ready = threading.Condition()
resident_context = None

def continuity(arguments, number):
    request_id = "continuity-"+str(number)
    item_id = "continuity-item-"+str(number)
    notification("item/started", {"item":{"id":item_id,"type":"dynamicToolCall","tool":"boxen_continuity","arguments":arguments,"status":"inProgress"}})
    emit({"id":request_id,"method":"item/tool/call","params":{"threadId":thread,"turnId":"fixture-turn","callId":item_id,"tool":"boxen_continuity","arguments":arguments}})
    with response_ready:
        assert response_ready.wait_for(lambda:request_id in responses,timeout=10), "continuity tool was not answered by daemon"
        response = responses.pop(request_id)
    notification("item/completed", {"item":{"id":item_id,"type":"dynamicToolCall","tool":"boxen_continuity","status":"completed","contentItems":response["contentItems"],"success":response["success"]}})
    return response, json.loads(response["contentItems"][0]["text"])



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
    answer = "Fixture result"
    if "RESIDENT_" in prompt:
        assert resident_context is not None
        response, state = continuity({"action":"read"}, 1)
        assert response["success"] and state["id"] == resident_context["id"]
        if "RESIDENT_WRITE" in prompt:
            response, state = continuity({"action":"write","expected_revision":state["revision"],"memory":"fixture durable memory"},2)
            assert response["success"]
        elif "RESIDENT_QUOTA" in prompt:
            response, result = continuity({"action":"write","expected_revision":state["revision"],"memory":"x"*300000},2)
            assert not response["success"] and "quota exceeded" in result["error"]
        elif "RESIDENT_STALE" in prompt:
            response, result = continuity({"action":"write","expected_revision":state["revision"]-1,"memory":"stale overwrite"},2)
            assert not response["success"] and "revision conflict" in result["error"]
        answer = json.dumps(state)
    for number in range(30 if "SLOW" in prompt else 5):
        if cancel.wait(0.1):
            break
        notification("item/commandExecution/outputDelta", {"itemId": "command", "delta": f"tick {number}\n"})
    if "LARGE" in prompt:
        notification("item/commandExecution/outputDelta", {"itemId": "command", "delta": "🐈" * 50000})
    status = "interrupted" if cancel.is_set() else "completed"
    notification("item/completed", {"item": {"id": "command", "type": "commandExecution", "command": "fixture command", "status": status, "aggregatedOutput": "fixture output"}})
    notification("item/started", {"item": {"id": "answer", "type": "agentMessage", "text": ""}})
    notification("item/agentMessage/delta", {"itemId": "answer", "delta": answer})
    notification("item/completed", {"item": {"id": "answer", "type": "agentMessage", "text": answer}})
    notification("turn/completed", {"turn": {"id": "fixture-turn", "status": status, "error": None}})


for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method is None:
        if request.get("id") == "approval-1": approved.set()
        else:
            with response_ready:
                responses[request["id"]] = request["result"]
                response_ready.notify_all()
        continue
    if method == "initialized":
        continue
    if method == "initialize":
        result = {"userAgent": "fixture"}
    elif method in ["thread/start", "thread/resume"]:
        assert request["params"]["sandbox"] == "danger-full-access"
        assert request["params"]["approvalPolicy"] == "never"
        missing=pathlib.Path(request["params"]["cwd"])/"fixture-forget-session"
        if method=="thread/resume" and missing.exists():
            missing.unlink()
            emit({"id":request["id"],"error":{"code":-32000,"message":"fixture backend session missing"}})
            continue
        if "developerInstructions" in request["params"] and "Saved continuity snapshot at turn acceptance (JSON):" in request["params"]["developerInstructions"]:
            resident_context = json.loads(request["params"]["developerInstructions"].split("Saved continuity snapshot at turn acceptance (JSON):\n",1)[1].split("\n",1)[0])
            assert resident_context["machine_id"] in request["params"]["developerInstructions"]
            assert resident_context["environment"]["os"] == {"darwin":"macos","win32":"windows"}.get(sys.platform,sys.platform)
            if method == "thread/start": assert request["params"]["dynamicTools"][0]["name"] == "boxen_continuity"
            if "The previous backend session could not be resumed." in request["params"]["developerInstructions"]:
                assert "retained_runs_directory" in request["params"]["developerInstructions"]
                assert "total_prior_events" in request["params"]["developerInstructions"]
                assert "RESIDENT_" in request["params"]["developerInstructions"]
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
