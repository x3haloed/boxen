#!/usr/bin/env python3
"""Real OpenRouter execution through two paired daemons; synthetic scratch-only workloads."""
import json
import pathlib
import secrets
import socket
import subprocess
import sys
import tempfile
import urllib.request

binary=pathlib.Path(sys.argv[1] if len(sys.argv)>1 else 'target/release/boxen').resolve()
command=(['/bin/bash'] if binary.suffix=='.com' else [])+[str(binary)]
credential=pathlib.Path.home()/'.boxen/credentials/openrouter.key'
def port():
    with socket.socket() as s:s.bind(('127.0.0.1',0));return s.getsockname()[1]
with tempfile.TemporaryDirectory(prefix='boxen-runtime-live-') as temp:
    root=pathlib.Path(temp);paths=[root/'a',root/'b'];processes=[];tokens=[];peers=[port(),port()];uis=[port(),port()]
    def cli(i,*args):return subprocess.check_output(command+['--data-dir',str(paths[i]),*args],text=True,timeout=10).strip()
    keys=[cli(i,'identity') for i in range(2)]
    for i in range(2):cli(i,'trust',keys[1-i],'Other')
    try:
        for i in range(2):
            p=subprocess.Popen(command+['--data-dir',str(paths[i]),'serve','--listen',f'127.0.0.1:{peers[i]}','--ui',f'127.0.0.1:{uis[i]}','--openrouter-key-file',str(credential)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
            processes.append(p);lines=[p.stdout.readline() for _ in range(3)];assert 'Local controls:' in lines[-1],lines;tokens.append(lines[-1].strip().split('#')[1])
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
        run=api({'op':'start','spec':{'id':ident,'thread_id':ident,'backend':'openrouter','cwd':temp,'model':'deepseek/deepseek-v4.1-flash',
            'prompt':f'Please run boxen_shell with command `printf {marker} > proof.txt; cat proof.txt`, then respond with the marker.'}})
        events=collect(run);assert (root/'proof.txt').read_text()==marker
        assert marker in api({'op':'thread_result','id':ident})['response']
        assert any(e['kind']=='tool_output' for e in events) and any(e['kind']=='assistant_delta' for e in events)
        # Stateful follow-up uses the daemon's retained provider messages, not an SDK session.
        follow=secrets.token_hex(32)
        events+=collect(api({'op':'start','spec':{'id':follow,'thread_id':ident,'backend':'openrouter','cwd':temp,'model':'deepseek/deepseek-v4.1-flash','prompt':'What exact marker did the earlier command write? Please answer from the conversation without another tool call.'}}))
        assert marker in api({'op':'thread_result','id':follow})['response']
        r=api({'op':'resident_create','spec':{'id':secrets.token_hex(32),'backend':'openrouter','name':'Live protocol proof','cwd':temp,'model':'deepseek/deepseek-v4.1-flash','auto_activate':False,'identity':'This is a disposable protocol-verification resident.'}})
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
        usage=[e['data']['usage'] for e in events if e['kind']=='inference_exchange_completed']
        print(json.dumps({'ok':True,'binary':str(binary),'model':'deepseek/deepseek-v4.1-flash','runs':4,'shell_effect':True,'followup':True,'selective_mail':True,'no_send_completion':True,'retained_events':len(events),'total_tokens':sum(u.get('total_tokens',0) for u in usage),'reported_cost':sum(u.get('cost',0) for u in usage)}))
    finally:
        for p in processes:
            if p.poll() is None:p.kill()
            p.wait(timeout=5)
