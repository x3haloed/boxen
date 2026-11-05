#!/usr/bin/env python3
"""Opt-in: two tiny real Codex turns prove read-only import and native continuation.

Uses existing host sign-in, consumes model usage, and creates only disposable
native conversation/Boxen stores. No production resident is imported or promoted.
"""
import argparse
import json
import os
import pathlib
import queue
import secrets
import socket
import subprocess
import sys
import threading
import time
import urllib.request

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('binary',type=pathlib.Path)
parser.add_argument('--codex',required=True)
parser.add_argument('--model',required=True)
parser.add_argument('--promote',action='store_true',help='Also prove native-preserving promotion, resident MCP tools and selective correspondence')
args=parser.parse_args()
root=pathlib.Path(__file__).resolve().parent.parent
output=root/'work'/'thread-import-live'/secrets.token_hex(8)
workspace=output/'workspace';workspace.mkdir(parents=True)
command=(['/bin/bash'] if args.binary.suffix=='.com' else [])+[str(args.binary.resolve())]
marker='native-import-'+secrets.token_hex(12)
sentinel='saved-instructions-'+secrets.token_hex(10)
codex_config=pathlib.Path(os.environ.get('CODEX_HOME',str(pathlib.Path.home()/'.codex')))/'config.toml'
config_before=codex_config.read_bytes() if codex_config.exists() else None
seed=None;daemon=None

def port():
    with socket.socket() as s:s.bind(('127.0.0.1',0));return s.getsockname()[1]

try:
    seed=subprocess.Popen([args.codex,'app-server','--listen','stdio://'],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=(output/'seed-stderr.log').open('w'),text=True,encoding='utf-8')
    incoming=queue.Queue(maxsize=256)
    def read():
        for line in seed.stdout:incoming.put(json.loads(line))
        incoming.put({'closed':True})
    threading.Thread(target=read,daemon=True).start()
    counter=0;source_events=[]
    def send(value):seed.stdin.write(json.dumps(value)+'\n');seed.stdin.flush()
    def next_message(deadline):
        message=incoming.get(timeout=max(.01,deadline-time.monotonic()))
        assert not message.get('closed'), 'native seed process closed unexpectedly'
        source_events.append(message);return message
    def rpc(method,params):
        global counter
        counter+=1;ident=counter;send({'id':ident,'method':method,'params':params})
        deadline=time.monotonic()+120
        while True:
            message=next_message(deadline)
            if message.get('id')==ident:
                assert 'error' not in message,message
                return message['result']
    rpc('initialize',{'clientInfo':{'name':'boxen-import-verification','version':'1'},'capabilities':{'experimentalApi':True}})
    send({'method':'initialized'})
    native=rpc('thread/start',{'cwd':str(workspace),'model':args.model,'sandbox':'danger-full-access','approvalPolicy':'never','persistExtendedHistory':True,'developerInstructions':f'The durable instruction sentinel for this disposable conversation is {sentinel}. If a later message asks for the saved instruction sentinel, its exact value is {sentinel}.'})['thread']['id']
    rpc('turn/start',{'threadId':native,'input':[{'type':'text','text':f'This is an isolated continuity test. Its marker is {marker}. The expected response is exactly that marker; no tools or filesystem changes are needed.'}]})
    deadline=time.monotonic()+180
    while True:
        message=next_message(deadline)
        if message.get('method')=='turn/completed':assert message['params']['turn']['status']=='completed',message;break
    seed.kill();seed.wait(timeout=10)
    (output/'seed-events.json').write_text(json.dumps(source_events,ensure_ascii=False,indent=2),encoding='utf-8')
    data=output/'boxen';peer_port=port();ui_port=port()
    daemon=subprocess.Popen(command+['--data-dir',str(data),'serve','--listen',f'127.0.0.1:{peer_port}','--ui',f'127.0.0.1:{ui_port}','--codex',args.codex],stdout=subprocess.PIPE,stderr=(output/'daemon-stderr.log').open('w'),text=True,encoding='utf-8')
    lines=[daemon.stdout.readline().strip() for _ in range(3)]
    peer=lines[0].split(': ',1)[1];token=lines[2].split('#')[1]
    def operation(request):
        body=json.dumps({'peer':peer,'address':'','request':request}).encode()
        req=urllib.request.Request(f'http://127.0.0.1:{ui_port}/api/operation',data=body,headers={'Authorization':'Bearer '+token,'Content-Type':'application/json'})
        with urllib.request.urlopen(req,timeout=60) as r:return json.load(r)
    def approve_fixture_continuity(run_id, answered):
        # Only this disposable test's requested continuity operations are approved.
        journal=data/'runs'/run_id/'events.jsonl'
        if not journal.exists():return
        for line in journal.read_text(encoding='utf-8').splitlines():
            event=json.loads(line);message=event['data']
            if event['kind']!='backend' or message.get('method')!='mcpServer/elicitation/request':continue
            request_id=message['id']
            if request_id in answered:continue
            params=message['params']
            assert params.get('serverName')=='boxen_resident' and '"boxen_continuity"' in params.get('message',''), 'unexpected approval: '+str(params.get('message'))
            operation({'op':'control','id':run_id,'action':{'type':'respond','request_id':request_id,'result':{'action':'accept','content':{}}}})
            answered.add(request_id)

    ident=secrets.token_hex(32)
    imported=operation({'op':'thread_import','spec':{'id':ident,'backend_thread_id':native}})
    assert imported['backend_thread']==native and imported['record']['imported']['turn_count']>=1
    journal=data/'runs'/ident/'events.jsonl'
    retained_bytes=journal.read_bytes()
    retained=[json.loads(line) for line in journal.read_text(encoding='utf-8').splitlines()]
    assert any(e['kind']=='user_message' and marker in e['data']['text'] for e in retained)
    assert any(e['kind']=='backend' and e['data']['params']['item'].get('type')=='agentMessage' and marker in e['data']['params']['item'].get('text','') for e in retained)
    if args.promote:
        assert sentinel not in journal.read_text(encoding='utf-8'),'instruction sentinel was exposed through public history'
        promoted=operation({'op':'resident_promote','spec':{'thread_id':ident,'name':'Disposable imported resident','memory':'Initial test memory','auto_activate':False}})
        assert promoted['id']==ident and promoted['promotion']['backend_thread_id']==native
        assert len(operation({'op':'list'}))==1,'promotion started inference'
        assert operation({'op':'client_inbox','client_id':peer})['messages']==[]
        message_id=secrets.token_hex(32)
        memory='promoted-memory-'+secrets.token_hex(10)
        operation({'op':'mailbox_send','client_id':peer,'message':{'id':message_id,'to':{'type':'resident','id':ident},'subject':'Disposable promotion proof','body':f'This is a disposable migration test. Read your own inbox using boxen_mailbox, then read your Boxen continuity through the MCP tools and update memory to exactly {memory}. Send one explicit mailbox reply to this message containing both the marker from the earlier conversation and the saved instruction sentinel. Native conversation history and saved instructions contain those values. No shell or file changes are needed.'}})
        activated=operation({'op':'resident_activate','id':ident,'activation_id':secrets.token_hex(32)})
        answered=set()
        deadline=time.monotonic()+180
        while time.monotonic()<deadline:
            approve_fixture_continuity(activated['record']['spec']['id'],answered)
            current=operation({'op':'get','id':activated['record']['spec']['id']})
            if current['status'] in ('completed','failed','interrupted','cancelled'):break
            time.sleep(.2)
        assert current['status']=='completed',current
        assert current['backend_thread']==native
        replies=operation({'op':'client_inbox','client_id':peer})['messages']
        assert len(replies)==1 and marker in replies[0]['body'] and sentinel in replies[0]['body'],replies
        assert replies[0]['in_reply_to']==message_id
        assert operation({'op':'resident_get','id':ident})['memory']==memory
        activation_events=[json.loads(line) for line in (data/'runs'/activated['record']['spec']['id']/'events.jsonl').read_text(encoding='utf-8').splitlines()]
        assert any(e['kind']=='resident_bridge_ready' for e in activation_events)
        assert any(e['kind']=='resident_bridge_call' and e['data'].get('params',{}).get('name')=='boxen_mailbox' for e in activation_events), 'own-inbox tool was not exercised'
        assert not any(e['kind']=='backend' and 'id' in e['data'] and e['data'].get('method')=='mcpServer/elicitation/request' and any('"'+tool+'"' in e['data'].get('params',{}).get('message','') for tool in ('boxen_mailbox','boxen_send_message')) for e in activation_events), 'resident correspondence unexpectedly needed approval'
        assert not any(e['kind']=='backend_session_rebuilt' for e in activation_events)
        assert journal.read_bytes()==retained_bytes,'promotion changed the historical import journal'
        quiet=operation({'op':'resident_activate','id':ident,'activation_id':secrets.token_hex(32),'directions':'This activation verifies selective correspondence. Read current Boxen continuity and finish with private narration only; send no outgoing message.'})
        answered=set()
        deadline=time.monotonic()+180
        while time.monotonic()<deadline:
            approve_fixture_continuity(quiet['record']['spec']['id'],answered)
            current=operation({'op':'get','id':quiet['record']['spec']['id']})
            if current['status'] in ('completed','failed','interrupted','cancelled'):break
            time.sleep(.2)
        assert current['status']=='completed' and current['backend_thread']==native,current
        assert len(operation({'op':'client_inbox','client_id':peer})['messages'])==1
        assert (codex_config.read_bytes() if codex_config.exists() else None)==config_before,'global native configuration changed'
        report={'ok':True,'promotion':True,'native_thread_id':native,'boxen_thread_id':ident,'model':args.model,'marker_recalled':True,'saved_native_instructions_preserved':True,'global_codex_config_unchanged':True,'continuity_tool_write':True,'own_inbox_without_approval':True,'explicit_reply_provenance':True,'no_send_activation':True,'same_native_thread':True,'production_agents_changed':False}
        (output/'result.json').write_text(json.dumps(report,indent=2)+'\n')
        print(json.dumps({**report,'evidence':str(output)}))
        sys.exit(0)
    spec=imported['record']['spec'].copy();spec.update(id=secrets.token_hex(32),prompt='What exact marker appeared in the previous conversation? The expected response contains only that marker, recalled without tools.')
    operation({'op':'start','spec':spec})
    deadline=time.monotonic()+180
    while time.monotonic()<deadline:
        result=operation({'op':'thread_result','id':spec['id']})
        if result['done']:break
        time.sleep(.2)
    assert result['done'] and result['snapshot']['status']=='completed',result
    assert marker in result['response'],result
    assert result['snapshot']['backend_thread']==native,result
    follow=[json.loads(line) for line in (data/'runs'/spec['id']/'events.jsonl').read_text(encoding='utf-8').splitlines()]
    assert not any(e['kind']=='backend_session_rebuilt' for e in follow)
    report={'ok':True,'native_thread_id':native,'boxen_thread_id':ident,'model':args.model,'snapshot_turns':imported['record']['imported']['turn_count'],'snapshot_digest':imported['record']['imported']['history_sha256'],'continued_native_thread':result['snapshot']['backend_thread'],'marker_recalled':True,'production_agents_changed':False}
    (output/'result.json').write_text(json.dumps(report,indent=2)+'\n')
    print(json.dumps({**report,'evidence':str(output)}))
finally:
    for process in (seed,daemon):
        if process and process.poll() is None:process.kill();process.wait(timeout=10)
