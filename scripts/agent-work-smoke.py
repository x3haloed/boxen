#!/usr/bin/env python3
"""Real paired daemons and native MCP tools; controlled runtimes, no credentials."""
import hashlib
import json
import pathlib
import secrets
import socket
import subprocess
import sys
import tempfile
import time
import threading
import urllib.request
from smoke_helpers import fake_codex

ROOT = pathlib.Path(__file__).resolve().parent.parent
BINARY = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else ROOT/'target/debug/boxen').resolve()
COMMAND = (['/bin/bash'] if BINARY.suffix == '.com' else []) + [str(BINARY)]

def port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]

def ref(path): return {'$ref': path}
def step(tool, args, save=None, expect=None, **extra):
    return {'tool': tool, 'args': args, **({'save': save} if save else {}), 'expect': expect or {}, **extra}
def work(action, **args): return {'action': action, 'machine': 'Target', **args}

with tempfile.TemporaryDirectory(prefix='boxen-agent-work-') as temporary:
    base = pathlib.Path(temporary).resolve()
    homes = [base/'a', base/'b']; ports = [port(), port()]; uis = [port(), port()]
    processes = [None, None]; tokens = [None, None]
    workspace = base/'source workspace'; workspace.mkdir()
    attachment = workspace/'input.txt'; attachment.write_text('immutable original 🐈')
    def cli(i, *args):
        return subprocess.check_output(COMMAND+['--data-dir', str(homes[i]), *args], text=True, timeout=20).strip()
    keys = [cli(i, 'identity') for i in range(2)]
    cli(0, 'trust', keys[1], 'Target'); cli(1, 'trust', keys[0], 'Source')
    offline = subprocess.check_output(COMMAND+['--data-dir', str(base/'offline'), 'identity'], text=True).strip()
    cli(0, 'trust', offline, 'Offline')
    def launch(i):
        processes[i] = subprocess.Popen(COMMAND+['--data-dir', str(homes[i]), 'serve', '--listen', f'127.0.0.1:{ports[i]}', '--advertise', f'127.0.0.1:{ports[i]}', '--ui', f'127.0.0.1:{uis[i]}', '--codex', str(fake_codex())], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        lines = [processes[i].stdout.readline() for _ in range(3)]
        assert 'Local controls:' in lines[-1], lines
        tokens[i] = (homes[i]/'control-token').read_text().strip()
    def stop(i):
        p = processes[i]
        if p and p.poll() is None: p.terminate(); p.wait(timeout=15)
        if p: p.stdout.close(); p.stderr.close()
    def api(i, op):
        req = urllib.request.Request(f'http://127.0.0.1:{uis[i]}/api/operation', data=json.dumps({'peer': keys[i], 'address': '', 'request': op}).encode(), headers={'Authorization': 'Bearer '+tokens[i], 'Content-Type': 'application/json'})
        with urllib.request.urlopen(req, timeout=30) as response: return json.load(response)
    parent_thread = secrets.token_hex(32)
    def scenario(steps, after=None):
        ident = secrets.token_hex(32)
        packet = {'op':'start', 'spec':{'id':ident, 'thread_id':parent_thread, 'backend':'codex', 'cwd':str(workspace), 'model':'fixture', 'prompt':'WORK_INTERFACE:'+json.dumps(steps)}}
        if after: packet['after'] = after
        api(0, packet)
        deadline = time.monotonic()+90
        while time.monotonic() < deadline:
            state = api(0, {'op':'get', 'id':ident})
            if state['status'] in ('completed','interrupted','failed','cancelled'): break
            time.sleep(.1)
        assert state['status'] == 'completed', state
        assert api(0, {'op':'thread_result','id':ident})['response'] == 'WORK_INTERFACE_COMPLETE'
        return ident, json.loads((workspace/'work-interface-results.json').read_text())
    try:
        launch(0); launch(1)
        cli(0, 'ping', f'127.0.0.1:{ports[1]}', keys[1], 'endpoint discovery')
        diagnostic=base/'work-diagnostics.jsonl'
        api(0,{'op':'log_file','path':str(diagnostic),'filter':{'targets':['runtime'],'events':['work.']}})
        first, results = scenario([
            step('boxen_machines', {}, 'machines', {'total':3}),
            step('boxen_machines', {'machine':keys[1][:12]}, 'target', {'machines.0.reachable':True}),
            step('boxen_machines', {'machine':'Offline'}, 'offline', {'machines.0.reachable':False}),
            step('boxen_work', work('request', request_id='evidence', prompt='WORK_CHILD', model='fixture', attachments=[str(attachment)]), 'child', {'acceptance':'accepted'}),
            step('boxen_work', work('request', request_id='evidence', prompt='changed'), error=True),
            step('boxen_work', {'action':'read','request_id':'evidence'}, 'read', {'done':True,'response':'WORK_CHILD_COMPLETE'}, until_done=True),
            step('boxen_thread_files', {'action':'list','machine':'Target','run_id':ref('child.work.request_id')}, 'files'),
            step('boxen_thread_files', {'action':'pull','run_id':ref('child.work.request_id'),'path':'evidence.txt'}, 'pull'),
            step('boxen_thread_files', {'action':'pull','run_id':ref('child.work.request_id'),'path':'evidence.txt'}, 'native_pull', dynamic=True),
            step('boxen_thread_files', {'action':'pull','machine':'Target','run_id':ref('child.work.request_id'),'path':'../forbidden'}, error=True),
            step('boxen_work', {'action':'read','request_id':'evidence','event_seq':1}, 'raw'),
            step('boxen_work', work('list',thread_id=ref('child.work.thread_id')), 'catalog'),
            step('boxen_work', {'action':'request','request_id':'followup','after_request_id':ref('child.work.request_id'),'prompt':'Follow-up'}, 'followup', {'acceptance':'accepted','work.thread_id':ref('child.work.thread_id'),'work.cwd':ref('child.work.cwd')}),
            step('boxen_work', work('read', request_id='followup'), 'followup_read', {'done':True,'response':'Fixture result'}, until_done=True),
            step('boxen_work', work('request', request_id='question', prompt='ELICITATION_ACCEPT'), 'question', {'acceptance':'accepted'}),
            step('boxen_work', {'action':'read','request_id':'question'}, 'pending', until_request=True),
            step('boxen_work', {'action':'respond','request_id':'question','backend_request_id':ref('pending.backend_request_id'),'result':{'action':'accept','content':{}}}, 'answered'),
            step('boxen_work', {'action':'read','request_id':'question'}, 'answered_read', {'done':True,'work.status':'completed'}, until_done=True),
            step('boxen_work', work('request', request_id='slow', prompt='SLOW'), 'slow', {'acceptance':'accepted'}),
            step('boxen_work', work('steer', request_id='slow', prompt='extra input'), 'steer'),
            step('boxen_work', work('cancel', request_id='slow'), 'cancel'),
            step('boxen_work', work('read', request_id='slow'), 'cancel_read', {'done':True,'work.status':'cancelled'}, until_done=True),
        ])
        api(0,{'op':'log_off'})
        records=[json.loads(line) for line in diagnostic.read_text().splitlines()]
        assert {'work.tool_started','work.tool_completed'} <= {r['event'] for r in records}
        assert all(r.get('payload') is None and r['context']['run_id']==first for r in records)
        assert all(r['fields']['call_id'] for r in records)
        assert pathlib.Path(results['pull']['path']).read_text() == 'retained remote evidence 🐈'
        assert results['native_pull']['path']==results['pull']['path']
        child_id = results['child']['work']['request_id']
        child = api(1, {'op':'get','id':child_id})
        assert child['record']['automatic_cwd']
        assert child['record']['spec']['model']=='fixture'
        digest = child['record']['spec']['attachments'][0]['digest']
        assert digest == hashlib.sha256(attachment.read_bytes()).hexdigest()
        assert results['catalog']['requests'][0]['work']['request_id'] == child_id
        assert results['catalog']['machine']==keys[1] and results['catalog']['scanned']>=results['catalog']['returned']
        assert results['files']['selection']=={'thread_id':child_id,'machine':keys[1]}
        assert results['files']['recorded_count']==1 and results['files']['has_more'] is False
        unknown=api(1,{'op':'thread_workspace','thread_id':secrets.token_hex(32),'action':'list'})
        assert unknown['status']=='unknown_thread' and unknown['selection']['machine']==keys[1],unknown
        past=api(1,{'op':'thread_workspace','thread_id':child_id,'action':'list','offset':2,'through_run_id':results['files']['through_run_id']})
        assert past['status']=='offset_past_end' and past['recorded_count']==1,past
        resident=secrets.token_hex(32)
        api(1,{'op':'resident_create','spec':{'id':resident,'name':'Workspace scope fixture','cwd':str(workspace)}})
        out=api(1,{'op':'thread_workspace','thread_id':resident,'action':'list'})
        assert out['status']=='out_of_scope' and out['selection']['thread_id']==resident,out
        assert json.loads(results['raw']['text'])['seq']==1
        assert results['target']['machines'][0]['runtime']['platform']['os']
        assert not any(m['reachable'] for m in results['offline']['machines'])
        print('PASS native MCP discovery, reachability isolation, labels, immutable transfer, remote stream/read, follow-up, steering, cancellation and file retrieval')
        attachment.unlink(); stop(0); launch(0)
        second, retry = scenario([
            step('boxen_work', work('request', request_id='evidence', prompt='WORK_CHILD', model='fixture', attachments=[str(attachment)]), 'retry', {'acceptance':'accepted','work.request_id':child_id}),
            step('boxen_work', work('read', request_id='evidence',after=results['read']['next_after']), 'tail', {'done':True,'events':[]}),
            step('boxen_thread_files', {'action':'attachment','machine':'Target','run_id':child_id,'digest':digest}, 'original'),
            step('boxen_work', {'action':'request','request_id':'local','prompt':'WORK_CHILD'}, 'local', {'acceptance':'accepted'}),
            step('boxen_work', {'action':'read','request_id':'local'}, 'local_read', {'done':True,'response':'WORK_CHILD_COMPLETE'},until_done=True),
            step('boxen_work', {'action':'request','request_id':'large','prompt':'WORK_LONG'}, 'large'),
            step('boxen_work', {'action':'read','request_id':'large'}, 'large_read', {'response_truncated':True},until_done=True),
            step('boxen_work', {'action':'read','request_id':'large','after':0}, 'large_replay'),
            step('boxen_work', {'action':'read','request_id':'large','response_offset':ref('large_read.next_response_offset')}, 'response_page'),
        ], after=first)
        assert pathlib.Path(retry['original']['path']).read_text() == 'immutable original 🐈'
        assert retry['large_read']['next_response_offset']==16384
        assert retry['response_page']['response']=='🐈'*4096
        assert retry['response_page']['next_response_offset']==32768
        assert any(e.get('truncated') for e in retry['large_replay']['events'])
        assert len([r for r in api(1,{'op':'list'}) if r['record']['spec']['id']==child_id]) == 1
        print('PASS resumed native conversation, source restart, receipt-backed exact retry after original removal, cursor replay, retained attachment and local work')

        # A deliberately slow encrypted link makes transfer cancellation observable.
        # The proxy never sees application plaintext or machine credentials.
        proxy=socket.socket();proxy.bind(('127.0.0.1',0));proxy.listen(16);proxy.settimeout(.1)
        proxy_port=proxy.getsockname()[1];proxy_stop=threading.Event()
        def relay(front):
            back=socket.create_connection(('127.0.0.1',ports[1]),timeout=5)
            def pipe(source,target):
                try:
                    while data:=source.recv(65536):
                        time.sleep(.03);target.sendall(data)
                except OSError:pass
                finally:
                    try:target.shutdown(socket.SHUT_WR)
                    except OSError:pass
            upstream=threading.Thread(target=pipe,args=(front,back),daemon=True);upstream.start()
            pipe(back,front);upstream.join(timeout=5);front.close();back.close()
        def serve_proxy():
            while not proxy_stop.is_set():
                try:front,_=proxy.accept()
                except socket.timeout:continue
                except OSError:break
                threading.Thread(target=relay,args=(front,),daemon=True).start()
        threading.Thread(target=serve_proxy,daemon=True).start()
        try:
            api(0,{'op':'network','args':{'action':'admit','key':keys[1],'label':'Target','address':f'127.0.0.1:{proxy_port}'}})
            large=workspace/'transfer-input.txt';large.write_bytes(b'opaque transfer bytes'*419431)
            cancelling=secrets.token_hex(32)
            steps=[step('boxen_work',work('request',request_id='cancelled-transfer',prompt='WORK_CHILD',attachments=[str(large)]),allow_error=True)]
            api(0,{'op':'start','after':second,'spec':{'id':cancelling,'thread_id':parent_thread,'backend':'codex','cwd':str(workspace),'model':'fixture','prompt':'WORK_INTERFACE:'+json.dumps(steps)}})
            request=urllib.request.Request(f'http://127.0.0.1:{uis[0]}/api/watch',data=json.dumps({'peer':keys[0],'address':'','request':{'id':cancelling,'after':0}}).encode(),headers={'Authorization':'Bearer '+tokens[0],'Content-Type':'application/json'})
            with urllib.request.urlopen(request,timeout=20) as stream:
                for line in stream:
                    batch=json.loads(line)
                    receipt=next((e['data'] for e in batch['events'] if e['kind']=='work_request'),None)
                    if receipt:break
                    assert not batch['done'],batch
            assert receipt
            begin=time.monotonic();api(0,{'op':'control','id':cancelling,'action':{'type':'cancel'}})
            while time.monotonic()-begin<5:
                state=api(0,{'op':'get','id':cancelling})
                if state['status']=='cancelled':break
                time.sleep(.05)
            assert state['status']=='cancelled' and time.monotonic()-begin<3.5,state
            assert not any(r['record']['spec']['id']==receipt['request_id'] for r in api(1,{'op':'list'}))
            print('PASS coordinating-run cancellation interrupts a slow attachment transfer before child dispatch')
        finally:
            proxy_stop.set();proxy.close()

    finally:
        stop(0); stop(1)
