#!/usr/bin/env python3
"""Opt-in real-model work coordination over paired daemons and native MCP tools.

Uses host-local Codex sign-in and can consume plan usage. Isolated machine state
and an opaque attachment establish tool use, destination effects and retrieval.
"""
import argparse
import hashlib
import json
import pathlib
import secrets
import socket
import subprocess
import tempfile
import time
import urllib.request

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('binary', nargs='?', default='target/release/boxen')
parser.add_argument('--codex', default='codex')
parser.add_argument('--model', default='')
parser.add_argument('--evidence', type=pathlib.Path)
args = parser.parse_args()
binary = pathlib.Path(args.binary).resolve()
command = (['/bin/bash'] if binary.suffix=='.com' else [])+[str(binary)]

def port():
    with socket.socket() as s:
        s.bind(('127.0.0.1',0))
        return s.getsockname()[1]

def run(base):
    base.mkdir(parents=True,exist_ok=True)
    homes = [base/'source',base/'target']; ports = [port(),port()]; uis = [port(),port()]
    workspace = base/'workspace'; workspace.mkdir(exist_ok=True)
    attachment = workspace/('input-'+secrets.token_hex(4)+'.txt')
    content = ('Opaque work evidence '+secrets.token_hex(24)+' 🐈').encode()
    attachment.write_bytes(content)
    processes = []; tokens = []
    def cli(i,*parts):
        return subprocess.check_output(command+['--data-dir',str(homes[i]),*parts],text=True,timeout=20).strip()
    keys = [cli(i,'identity') for i in range(2)]
    cli(0,'trust',keys[1],'Evidence target');cli(1,'trust',keys[0],'Coordinator')
    def api(op):
        request = urllib.request.Request(f'http://127.0.0.1:{uis[0]}/api/operation',data=json.dumps({'peer':keys[0],'address':'','request':op}).encode(),headers={'Authorization':'Bearer '+tokens[0],'Content-Type':'application/json'})
        with urllib.request.urlopen(request,timeout=30) as response:return json.load(response)
    def finish(ident):
        deadline = time.monotonic()+300
        while time.monotonic()<deadline:
            state = api({'op':'get','id':ident})
            if state['status'] in ('completed','interrupted','cancelled','failed'):
                assert state['status']=='completed',state
                return state
            time.sleep(.25)
        raise AssertionError('live coordination deadline expired')
    try:
        for i in range(2):
            output=(base/f'daemon-{i}.log').open('w')
            process=subprocess.Popen(command+['--data-dir',str(homes[i]),'serve','--listen',f'127.0.0.1:{ports[i]}','--advertise',f'127.0.0.1:{ports[i]}','--ui',f'127.0.0.1:{uis[i]}','--codex',args.codex],stdout=subprocess.PIPE,stderr=output,text=True)
            processes.append((process,output))
            lines=[process.stdout.readline() for _ in range(3)]
            assert 'Local controls:' in lines[-1],lines
            tokens.append((homes[i]/'control-token').read_text().strip())
        cli(0,'ping',f'127.0.0.1:{ports[1]}',keys[1],'live test discovery')
        ident=secrets.token_hex(32)
        prompt=f'''Please exercise Boxen's agent-facing work interface end to end. Discover the machine named Evidence target with boxen_machines. Use boxen_work to request a new codex conversation on that machine, with request label evidence and this local input attachment: {attachment}. The child task is: read the attached text file, write its exact bytes to evidence.txt in your working directory, compute its SHA256, and reply with that SHA256. Follow retained progress with boxen_work reads until the child ends. Retrieve evidence.txt through boxen_thread_files into a locally readable file. Then use boxen_work to continue the child's same thread, asking it to report the saved file's byte count without rewriting it; follow that request until completion. All orchestration is through the Boxen tools, with no custom HTTP, CLI or shell control clients. Final response contains the child's SHA256, byte count, and retrieved local file path. This is an authorized isolated test.'''
        api({'op':'start','spec':{'id':ident,'thread_id':ident,'backend':'codex','cwd':str(workspace),'model':args.model,'prompt':prompt}})
        finish(ident)
        events=[json.loads(line) for line in (homes[0]/'runs'/ident/'events.jsonl').read_text().splitlines()]
        calls=[e['data']['params'] for e in events if e['kind']=='resident_bridge_call']
        results=[e['data'] for e in events if e['kind']=='resident_bridge_result']
        assert {'boxen_machines','boxen_work','boxen_thread_files'} <= {c['name'] for c in calls}
        requests=[e['data'] for e in events if e['kind']=='work_request']
        assert len(requests)==2,requests
        assert requests[0]['request']['spec']['thread_id']==requests[1]['request']['spec']['thread_id']
        assert requests[1]['request']['after']==requests[0]['request_id']
        pulled=[json.loads(r['result']['content'][0]['text']) for r in results if r['tool']=='boxen_thread_files' and not r['result'].get('isError')]
        pulled=[p for p in pulled if p.get('path')]
        assert pulled and pathlib.Path(pulled[-1]['path']).read_bytes()==content,pulled
        response=api({'op':'thread_result','id':ident})['response']
        assert hashlib.sha256(content).hexdigest() in response,response
        # A fresh app-server resumes the coordinator's existing native conversation.
        follow=secrets.token_hex(32)
        api({'op':'start','after':ident,'spec':{'id':follow,'thread_id':ident,'backend':'codex','cwd':str(workspace),'model':args.model,'prompt':'Please use boxen_work to read the existing evidence request on Evidence target once more, then report its retained checksum. This checks that the current tools remain available after native session resumption; no new child request or file write is needed.'}})
        finish(follow)
        follow_events=[json.loads(line) for line in (homes[0]/'runs'/follow/'events.jsonl').read_text().splitlines()]
        assert any(e['kind']=='resident_bridge_call' and e['data']['params']['name']=='boxen_work' for e in follow_events)
        assert not any(e['kind']=='work_request' for e in follow_events)
        assert hashlib.sha256(content).hexdigest() in api({'op':'thread_result','id':follow})['response']
        # Promotion changes the communication contract while preserving native
        # ownership; the work interface still attaches at the next run boundary.
        resident=api({'op':'resident_promote','spec':{'thread_id':ident,'name':'Disposable work-interface resident','auto_activate':False}})
        activation=api({'op':'resident_activate','id':resident['id'],'activation_id':secrets.token_hex(32),'directions':'This isolated compatibility check requests no outgoing correspondence. Please use boxen_work to read the existing evidence request on Evidence target and finish with its retained checksum in private narration. No new child requests or file writes are needed.'})
        promoted_id=activation['record']['spec']['id']
        finish(promoted_id)
        promoted_events=[json.loads(line) for line in (homes[0]/'runs'/promoted_id/'events.jsonl').read_text().splitlines()]
        assert any(e['kind']=='resident_bridge_call' and e['data']['params']['name']=='boxen_work' for e in promoted_events)
        assert not any(e['kind'] in ('work_request','message_send_reserved') for e in promoted_events)
        report={'ok':True,'scope':'two isolated daemons on one host; real Codex models','machine_discovery':True,'remote_work_and_continuation':True,'immutable_attachment_transfer':True,'verified_file_retrieval':True,'resumed_native_tools':True,'native_promotion_work_tools':True,'source_runs':[ident,follow],'child_requests':[r['request_id'] for r in requests],'tool_calls':len(calls),'sha256':hashlib.sha256(content).hexdigest()}
        (base/'result.json').write_text(json.dumps(report,indent=2))
        print(json.dumps(report))
    finally:
        for process,output in processes:
            if process.poll() is None:process.terminate();process.wait(timeout=20)
            process.stdout.close();output.close()

if args.evidence:run(args.evidence.resolve())
else:
    with tempfile.TemporaryDirectory(prefix='boxen-work-live-') as temp:run(pathlib.Path(temp).resolve())
