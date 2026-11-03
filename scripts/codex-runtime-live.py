#!/usr/bin/env python3
"""Real Codex app-server execution through two paired daemons, using host-local sign-in.

Optional live check; not part of the credential-free fixture rotation.
"""
import argparse
import json
import os
import pathlib
import secrets
import socket
import subprocess
import sys
import tempfile
import urllib.request

parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('binary',nargs='?',default='target/release/boxen')
parser.add_argument('--codex',default='codex')
parser.add_argument('--model',default='')
args=parser.parse_args()
binary=pathlib.Path(args.binary).resolve()
command=(['/bin/bash'] if binary.suffix=='.com' else [])+[str(binary)]
def port():
    with socket.socket() as s:s.bind(('127.0.0.1',0));return s.getsockname()[1]
with tempfile.TemporaryDirectory(prefix='boxen-codex-live-') as temp:
    root=pathlib.Path(temp);paths=[root/'a',root/'b'];processes=[];tokens=[];peers=[port(),port()];uis=[port(),port()]
    def cli(i,*args):return subprocess.check_output(command+['--data-dir',str(paths[i]),*args],text=True,timeout=10).strip()
    keys=[cli(i,'identity') for i in range(2)]
    for i in range(2):cli(i,'trust',keys[1-i],'Other')
    try:
        for i in range(2):
            p=subprocess.Popen(command+['--data-dir',str(paths[i]),'serve','--listen',f'127.0.0.1:{peers[i]}','--ui',f'127.0.0.1:{uis[i]}','--codex',args.codex],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
            processes.append(p);lines=[p.stdout.readline() for _ in range(3)];assert 'Local controls:' in lines[-1], 'test daemon did not start';tokens.append(lines[-1].strip().split('#')[1])
        target={'peer':keys[1],'address':f'127.0.0.1:{peers[1]}'}
        def api(op,watch=False):
            request=urllib.request.Request(f'http://127.0.0.1:{uis[0]}/api/'+('watch' if watch else 'operation'),data=json.dumps({**target,'request':op}).encode(),headers={'Authorization':'Bearer '+tokens[0],'Content-Type':'application/json'})
            response=urllib.request.urlopen(request,timeout=150)
            return response if watch else json.load(response)
        def collect(run):
            events=[]
            with api({'id':run['record']['spec']['id'],'after':0},True) as stream:
                for line in stream:
                    batch=json.loads(line);assert 'error' not in batch,batch;events+=batch['events']
                    if batch['done']:
                        assert batch['snapshot']['status']=='completed',batch['snapshot']
                        return events
            raise AssertionError('stream ended early')
        marker='boxen-live-'+secrets.token_hex(5);ident=secrets.token_hex(32)
        run=api({'op':'start','spec':{'id':ident,'thread_id':ident,'backend':'codex','cwd':temp,'model':args.model,
            'prompt':f'Please run a shell command appropriate for this host that writes exactly {marker} to proof.txt with no newline or byte-order mark, then reads it back. Respond with the marker.'}})
        events=collect(run);assert (root/'proof.txt').read_text()==marker
        assert marker in api({'op':'thread_result','id':ident})['response']
        assert any(e['data'].get('method')=='item/completed' and e['data'].get('params',{}).get('item',{}).get('type')=='commandExecution' for e in events) and any(e['data'].get('method')=='item/agentMessage/delta' for e in events)
        # A fresh app-server resumes the retained native Codex conversation.
        follow=secrets.token_hex(32)
        events+=collect(api({'op':'start','spec':{'id':follow,'thread_id':ident,'backend':'codex','cwd':temp,'model':args.model,'prompt':'What exact marker did the earlier command write? Please answer from the conversation without another tool call.'}}))
        assert marker in api({'op':'thread_result','id':follow})['response']
        r=api({'op':'resident_create','spec':{'id':secrets.token_hex(32),'backend':'codex','name':'Live protocol proof','cwd':temp,'model':args.model,'auto_activate':False,'identity':'This is a disposable protocol-verification resident.'}})
        client=secrets.token_hex(32)
        def send(body):return api({'op':'mailbox_send','client_id':client,'message':{'id':secrets.token_hex(32),'to':{'type':'resident','id':r['id']},'subject':'Protocol proof','body':body}})
        send('This message requests no reply; it is silent evidence for the protocol test.')
        reply=send(f'Please reply explicitly to this message with body exactly {marker}, using its reply_to address and linking in_reply_to. No other correspondence is requested.')
        activation=api({'op':'resident_activate','id':r['id'],'activation_id':secrets.token_hex(32),'directions':'The protocol test asks for one explicit reply to the message requesting it. Final narration remains private.'})
        events+=collect(activation)
        inbox=lambda:api({'op':'client_inbox','client_id':client})['messages']
        delivered=inbox();assert len(delivered)==1 and delivered[0]['body']==marker and delivered[0]['in_reply_to']==reply['id'],delivered
        silent=api({'op':'resident_activate','id':r['id'],'activation_id':secrets.token_hex(32),'directions':f'This activation requests no outgoing correspondence. Please finish with a private line containing {marker}.'})
        events+=collect(silent);assert inbox()==delivered
        backend_ids=[e['data']['id'] for e in events if e['kind']=='backend_thread']
        assert len(backend_ids)==4 and backend_ids[0]==backend_ids[1] and backend_ids[2]==backend_ids[3]
        assert not any(e['kind']=='backend_session_rebuilt' for e in events)
        print(json.dumps({'ok':True,'binary':str(binary),'runtime':'codex app-server','model':args.model or 'runtime default','runs':4,'shell_effect':True,'same_backend_thread_followup':True,'selective_mail':True,'no_send_completion':True,'retained_events':len(events)}))

    finally:
        for p in processes:
            if p.poll() is None:
                if os.name == 'nt':
                    # The daemon may still be tearing down its final app-server.
                    # Only this disposable daemon's owned descendants are stopped.
                    subprocess.run(['taskkill','/PID',str(p.pid),'/T','/F'],check=True,stdout=subprocess.DEVNULL)
                else:p.kill()
            p.wait(timeout=5)
