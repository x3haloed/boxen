#!/usr/bin/env python3
"""Deterministic app-server fixture. Never used by Boxen outside tests."""
import json
import pathlib
import secrets
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

def invoke(tool, arguments, number):
    request_id = "tool-"+str(number)
    item_id = "tool-item-"+str(number)
    notification("item/started", {"item":{"id":item_id,"type":"dynamicToolCall","tool":tool,"arguments":arguments,"status":"inProgress"}})
    emit({"id":request_id,"method":"item/tool/call","params":{"threadId":thread,"turnId":"fixture-turn","callId":item_id,"tool":tool,"arguments":arguments}})
    with response_ready:
        assert response_ready.wait_for(lambda:request_id in responses,timeout=10), "resident tool was not answered by daemon"
        response = responses.pop(request_id)
    notification("item/completed", {"item":{"id":item_id,"type":"dynamicToolCall","tool":tool,"status":"completed","contentItems":response["contentItems"],"success":response["success"]}})
    return response, json.loads(response["contentItems"][0]["text"])


def continuity(arguments,number):
    return invoke("boxen_continuity",arguments,number)


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
    if prompt.startswith('EVENT_GAME:'):
        settings=json.loads(prompt.split(':',1)[1]);sub=settings['subscription_id'];generation=settings['source_generation']
        response,listed=invoke('boxen_observations',{'action':'list'},190);assert response['success'] and any(s['spec']['id']==sub for s in listed['subscriptions'])
        response,batch=invoke('boxen_observations',{'action':'read','id':sub},191);assert response['success']
        response,look=invoke('boxen_adapter',{'subscription_id':sub,'source_generation':generation,'tool':'look','arguments':{}},192)
        assert response['success'] and not look.get('unknown'),look
        revision=look['response']['observations'][0]['payload']['revision']
        for number,direction in enumerate(['east','east','south','south']):
            response,result=invoke('boxen_adapter',{'subscription_id':sub,'source_generation':generation,'tool':'move','arguments':{'direction':direction,'revision':revision}},193+number)
            assert response['success'] and result['response']['result']['accepted'],result
            revision=result['response']['observations'][0]['payload']['revision']
        assert result['response']['observations'][0]['payload']['won']
        response,stale=invoke('boxen_adapter',{'subscription_id':sub,'source_generation':generation,'tool':'move','arguments':{'direction':'north','revision':1}},198)
        assert response['success'] and not stale['response']['result']['accepted']
        answer='EXTERNAL_GAME_WON'
    if prompt.startswith('EVENT_CALL:'):
        response,result=invoke('boxen_adapter',json.loads(prompt.split(':',1)[1]),199)
        assert response['success'];answer=json.dumps(result)
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
    if resident_context is not None and prompt.startswith("Mailbox activation."):
        activation = json.loads(prompt.rsplit("\n",1)[1])
        response, inbox = invoke("boxen_mailbox",{},10)
        assert response["success"] and len(inbox["messages"]) >= len(activation["messages"])
        for incoming in activation["messages"]:
            assert any(m["id"]==incoming["id"] and m["delivery"]=="presented" for m in inbox["messages"])
        eligible = [m for m in activation["messages"] if m["body"].startswith("REPLY")]
        if eligible:
            incoming=eligible[0]
            send={"id":secrets.token_hex(32),"to":incoming["reply_to"],"subject":"Re: "+incoming["subject"],"body":"DELIVERED_EXPLICIT_REPLY","in_reply_to":incoming["id"]}
            response, sent=invoke("boxen_send_message",send,11)
            assert response["success"]
            response, retried=invoke("boxen_send_message",send,12)
            assert response["success"] and retried["id"]==sent["id"]
        relays=[m for m in activation["messages"] if m["body"].startswith("RELAY:")]
        if relays:
            response,result=invoke("boxen_send_message",{"to":{"type":"resident","id":relays[0]["body"].split(":",1)[1]},"subject":"Resident relay","body":"SILENT resident relay"},14)
            assert response["success"]
        if "MAIL_BAD_SEND" in prompt:
            response,result=invoke("boxen_send_message",{"to":{"type":"resident","id":resident_context["id"]},"subject":"","body":"invalid"},13)
            assert not response["success"] and "subject" in result["error"]
        if "MAIL_THREAD" in prompt:
            request_id=secrets.token_hex(32)
            response,worker=invoke("boxen_thread_request",{"request_id":request_id,"prompt":"ordinary worker"},20)
            assert response["success"] and "resident_id" not in worker["record"]["spec"]
            for attempt in range(100):
                response,result=invoke("boxen_thread_result",{"request_id":request_id},21+attempt)
                assert response["success"]
                if result["done"]:break
                time.sleep(.05)
            assert result["response"]=="Fixture result"
        if "SCHEDULE_SELF" in activation['directions']:
            response,job=invoke("boxen_schedule",{"action":"create","spec":{"id":secrets.token_hex(32),"name":"Resident-owned future activation","directions":"Authored by the resident fixture","at_ms":int(time.time()*1000)+86400000}},180)
            assert response["success"] and job["definition"]["target"]=={"type":"resident","id":resident_context["id"]}
        if activation['directions'].startswith('SCHEDULE_SCOPE:'):
            response,result=invoke("boxen_schedule",{"action":"get","id":activation['directions'].split(':',1)[1]},181)
            assert not response['success'] and 'only the attached resident' in result['error']
        if not "RESIDENT_" in prompt: answer = "PRIVATE_NARRATION_ONLY"
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
