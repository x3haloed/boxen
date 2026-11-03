#!/usr/bin/env python3
"""Test real daemons/transport/storage against a deterministic backend fixture."""
from smoke_helpers import fake_codex
import json
import os
import pathlib
import secrets
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

root = pathlib.Path(__file__).resolve().parent.parent
binary = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else root / "target/release/boxen").resolve()
command = (["/bin/bash"] if binary.suffix == ".com" else []) + [str(binary)]
fixture = fake_codex()


def port():
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        return sock.getsockname()[1]


with tempfile.TemporaryDirectory(prefix="boxen-workloads-") as temporary:
    directories = [str(pathlib.Path(temporary) / letter) for letter in "ab"]
    processes, tokens = [], []
    peers, uis = [port(), port()], [port(), port()]

    def cli(i, *args):
        return subprocess.check_output(command + ["--data-dir", directories[i], *args], text=True, timeout=10).strip()

    def start_daemon(i):
        process = subprocess.Popen(command + ["--data-dir", directories[i], "serve", "--listen", f"127.0.0.1:{peers[i]}",
                                   "--ui", f"127.0.0.1:{uis[i]}", "--codex", str(fixture)], text=True,
                                   stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        processes.append(process)
        lines = [process.stdout.readline().strip() for _ in range(3)]
        assert "Local controls:" in lines[-1], lines
        return lines[-1].split("#")[1]

    keys = [cli(i, "identity") for i in range(2)]
    cli(0, "trust", keys[1], "B")
    cli(1, "trust", keys[0], "A")
    tokens = [start_daemon(i) for i in range(2)]
    target = {"address": f"127.0.0.1:{peers[1]}", "peer": keys[1]}

    def request(operation, watch=False, i=0, destination=None):
        req = urllib.request.Request(f"http://127.0.0.1:{uis[i]}/api/" + ("watch" if watch else "operation"),
            headers={"Authorization": "Bearer " + tokens[i], "Content-Type": "application/json"},
            data=json.dumps({**(destination or target), "request": operation}).encode())
        response = urllib.request.urlopen(req, timeout=10)
        return response if watch else json.load(response)

    def spec(prompt, thread=None):
        ident = secrets.token_hex(32)
        return {"id": ident, "thread_id": thread or ident, "backend": "codex", "cwd": temporary, "model": "fixture", "prompt": prompt}

    def collect(ident, after=0):
        events = []
        with request({"id": ident, "after": after}, watch=True) as stream:
            for line in stream:
                batch = json.loads(line)
                assert "error" not in batch, batch
                events.extend(batch["events"])
                if batch["done"]:
                    return events, batch["snapshot"]
        raise AssertionError("stream ended without a terminal state")

    def rejected(operation, text=None, **kwargs):
        try:
            request(operation, **kwargs)
            raise AssertionError("unexpected acceptance")
        except urllib.error.HTTPError as error:
            assert error.code==422,error.read()
            if text:assert text in error.read().decode()

    def resident(name,auto=True):
        return request({"op":"resident_create","spec":{"id":secrets.token_hex(32),"name":name,"cwd":temporary,"model":"fixture","auto_activate":auto}})

    def send(resident_id,body,client,ident=None):
        operation={"op":"mailbox_send","client_id":client,"message":{"id":ident or secrets.token_hex(32),"to":{"type":"resident","id":resident_id},"subject":"Test mail","body":body}}
        return request(operation),operation

    def runs(resident_id):
        return [r for r in request({"op":"list"}) if r["record"]["spec"].get("resident_id")==resident_id]

    def wait_runs(resident_id,count):
        deadline=time.monotonic()+10
        while time.monotonic()<deadline:
            incoming=runs(resident_id)
            if len(incoming)>=count:return incoming
            time.sleep(.03)
        raise AssertionError("resident did not activate")

    def inbox(client):return request({"op":"client_inbox","client_id":client})["messages"]
    def activate(resident_id,directions=""):
        return request({"op":"resident_activate","id":resident_id,"activation_id":secrets.token_hex(32),"directions":directions})
    clients=[secrets.token_hex(32),secrets.token_hex(32)]
    try:
        r=resident("Mail resident",False);ident=r["id"]
        assert r["communication"]=="mailbox" and r["mailbox"]=={"type":"resident","id":ident}
        assert runs(ident)==[]
        messages=[]
        for body,client in [("SILENT one",clients[0]),("REPLY one",clients[1]),("SILENT two",clients[0])]:
            message,operation=send(ident,body,client);messages.append(message)
            assert request(operation)==message
            assert message["sender"]=={"type":"client","machine":keys[0],"id":client}
            assert message["accepted_on"]==keys[1]
        assert runs(ident)==[]
        rejected({**operation,"message":{**operation["message"],"body":"changed"}},"different input")
        rejected({**operation,"message":{**operation["message"],"id":secrets.token_hex(32),"sender":{"type":"resident","id":ident}}})
        rejected({"op":"resident_inbox","id":ident},"different peer",i=1,destination={"peer":keys[1],"address":""})
        first=activate(ident)
        assert first["record"]["mailbox"]["message_ids"]==[m["id"] for m in messages]
        events,final=collect(first["record"]["spec"]["id"])
        assert final["status"]=="completed"
        assert any(e["data"].get("params",{}).get("delta")=="PRIVATE_NARRATION_ONLY" for e in events)
        assert inbox(clients[0])==[]
        replies=inbox(clients[1]);assert len(replies)==1
        assert replies[0]["body"]=="DELIVERED_EXPLICIT_REPLY"
        assert replies[0]["in_reply_to"]==messages[1]["id"]
        assert replies[0]["sender"]=={"type":"resident","id":ident}
        rejected({"op":"thread_result","id":first["record"]["spec"]["id"]},"mailboxes")
        rejected({"op":"start","spec":{"id":secrets.token_hex(32),"resident_id":ident,"thread_id":ident,"backend":"codex","cwd":temporary,"model":"fixture","prompt":"direct request"}},"mailbox")
        print("PASS three queued messages form one batch, two correspondents receive only explicit selective sends, retry deduplication and no narration delivery")

        free=activate(ident,"MAIL_THREAD MAIL_BAD_SEND")
        events,final=collect(free["record"]["spec"]["id"])
        assert final["status"]=="completed" and final["record"]["mailbox"]["message_ids"]==[]
        assert any(e["kind"]=="resident_tool_result" and e["data"].get("tool")=="boxen_thread_result" and e["data"]["result"].get("response")=="Fixture result" for e in events)
        assert len(inbox(clients[1]))==1 and inbox(clients[0])==[]
        print("PASS independent activation, optional reply, explicit send rejection and distinct anonymous thread request/response tools")

        recipient=resident("Recipient resident")
        send(ident,"RELAY:"+recipient["id"],clients[0])
        relay_run=activate(ident)
        assert collect(relay_run["record"]["spec"]["id"])[1]["status"]=="completed"
        delivered=wait_runs(recipient["id"],1)[0]
        assert collect(delivered["record"]["spec"]["id"])[1]["status"]=="completed"
        recipient_mail=request({"op":"resident_inbox","id":recipient["id"]})["messages"]
        assert len(recipient_mail)==1 and recipient_mail[0]["sender"]=={"type":"resident","id":ident}
        assert len(runs(recipient["id"]))==1
        print("PASS explicit resident-to-resident mail activates the destination without automatic reply loops")

        auto=resident("Automatic resident");aid=auto["id"];assert auto["auto_activate"]
        initial,_=send(aid,"SLOW SILENT",clients[0])
        active=wait_runs(aid,1)[0]
        with request({"id":active["record"]["spec"]["id"],"after":0},watch=True) as stream:
            for line in stream:
                if json.loads(line)["snapshot"]["status"]=="running":break
        more=[send(aid,"SILENT queued "+str(i),clients[i%2])[0] for i in range(5)]
        assert len(runs(aid))==1
        assert collect(active["record"]["spec"]["id"])[1]["status"]=="completed"
        follow=wait_runs(aid,2)[-1]
        assert follow["record"]["mailbox"]["message_ids"]==[m["id"] for m in more]
        assert collect(follow["record"]["spec"]["id"])[1]["status"]=="completed"
        assert len(runs(aid))==2
        assert inbox(clients[0])==[]
        print("PASS default automatic activation, serialized canonical execution and coalesced mail arriving during a run")

        interrupted_message,_=send(aid,"SLOW SILENT crash",clients[0])
        active=wait_runs(aid,3)[-1]
        with request({"id":active["record"]["spec"]["id"],"after":0},watch=True) as stream:
            for line in stream:
                if json.loads(line)["snapshot"]["status"]=="running":break
        pending,_=send(aid,"SILENT after restart",clients[1])
        processes[1].kill();processes[1].wait(timeout=5)
        journal=pathlib.Path(directories[1],"mailboxes/events.jsonl")
        with journal.open("ab") as file:file.write(b'{"seq":')
        tokens[1]=start_daemon(1)
        recovered=wait_runs(aid,4)[-1]
        assert recovered["record"]["mailbox"]["message_ids"]==[pending["id"]]
        events,final=collect(recovered["record"]["spec"]["id"])
        assert final["status"]=="completed" and any(e["kind"]=="recovery_notice" for e in events)
        old=next(r for r in runs(aid) if r["record"]["spec"]["id"]==active["record"]["spec"]["id"])
        assert old["status"]=="interrupted"
        all_ids=[m for r in runs(aid) for m in r["record"]["mailbox"]["message_ids"]]
        assert all_ids.count(interrupted_message["id"])==1 and all_ids.count(pending["id"])==1
        assert len(inbox(clients[1]))==1
        print("PASS crash/torn-tail recovery preserves accepted mail, batch ownership and private narration without replaying interrupted work")

        huge,_=send(aid,"\x01"*(32*1024),clients[0])
        last=wait_runs(aid,5)[-1]
        assert huge["id"] in last["record"]["mailbox"]["message_ids"]
        assert collect(last["record"]["spec"]["id"])[1]["status"]=="completed"
        print("PASS maximum-size escaped message produces bounded executable activation input")

        processes[-1].kill();processes[-1].wait(timeout=5)
        with journal.open("ab") as file:file.write(b'{"seq":999999,"kind":"broken"}\n')
        damaged=journal.read_bytes()
        tokens[1]=start_daemon(1)
        status=request({"op":"mailbox_status"})
        assert not status["available"] and status["issue"]
        assert journal.read_bytes()==damaged
        assert (journal.parent/"incidents.jsonl").exists()
        assert request({"op":"resident_get","id":ident})["name"]=="Mail resident"
        rejected({"op":"resident_inbox","id":ident},"unavailable")
        ordinary=spec("ordinary work remains available")
        request({"op":"start","spec":ordinary})
        assert collect(ordinary["id"])[1]["status"]=="completed"
        assert request({"op":"thread_result","id":ordinary["id"]})["response"]=="Fixture result"
        print("PASS mailbox corruption is isolated with preserved evidence and durable incident while ordinary workloads stay available")

    finally:
        for process in processes:
            if process.poll() is None:process.terminate()
            process.wait(timeout=5)
