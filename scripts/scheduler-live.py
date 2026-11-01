#!/usr/bin/env python3
"""Paid/existing-sign-in scheduled execution through two paired daemons.

Usage: scheduler-live.py [BINARY] [openrouter|codex] [MODEL] [CODEX_PATH]
Only disposable identities/workspaces are used. No Aster or production jobs.
"""
import json,pathlib,secrets,socket,subprocess,sys,tempfile,time,urllib.request
binary=pathlib.Path(sys.argv[1] if len(sys.argv)>1 else 'target/release/boxen').resolve()
command=(['/bin/bash'] if binary.suffix=='.com' else [])+[str(binary)]
backend=sys.argv[2] if len(sys.argv)>2 else 'openrouter'
model=sys.argv[3] if len(sys.argv)>3 else ('gpt-6-luna' if backend=='codex' else 'openai/gpt-4.1-mini')
codex=sys.argv[4:5]
def port():
    with socket.socket() as s:s.bind(('127.0.0.1',0));return s.getsockname()[1]
with tempfile.TemporaryDirectory(prefix='boxen-scheduler-live-') as temporary:
    root=pathlib.Path(temporary);data=[root/'a',root/'b'];peers=[port(),port()];uis=[port(),port()];processes=[];tokens=[]
    def cli(i,*a):return subprocess.check_output(command+['--data-dir',str(data[i]),*a],text=True,timeout=15).strip()
    keys=[cli(i,'identity') for i in range(2)]
    for i in range(2):cli(i,'trust',keys[1-i],'Live scheduling peer')
    try:
        for i in range(2):
            p=subprocess.Popen(command+['--data-dir',str(data[i]),'serve','--listen',f'127.0.0.1:{peers[i]}','--ui',f'127.0.0.1:{uis[i]}','--openrouter-key-file',str(pathlib.Path.home()/'.boxen/credentials/openrouter.key')]+(['--codex',codex[0]] if codex else []),stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True);processes.append(p)
            lines=[p.stdout.readline() for _ in range(3)];assert 'Local controls:' in lines[-1],lines;tokens.append(lines[-1].strip().split('#')[1])
        def api(op,watch=False):
            request=urllib.request.Request(f'http://127.0.0.1:{uis[0]}/api/'+('watch' if watch else 'operation'),headers={'Authorization':'Bearer '+tokens[0],'Content-Type':'application/json'},data=json.dumps({'peer':keys[1],'address':f'127.0.0.1:{peers[1]}','request':op}).encode())
            r=urllib.request.urlopen(request,timeout=150);return r if watch else json.load(r)
        events=[]
        def collect(run):
            got=[]
            with api({'id':run['record']['spec']['id'],'after':0},True) as stream:
                for line in stream:
                    batch=json.loads(line);assert 'error' not in batch,batch;got+=batch['events']
                    if batch['done']:
                        assert batch['snapshot']['status']=='completed',(batch['snapshot'],[e for e in got if e['kind']=='status'][-1:]);events.extend(got);return got,batch['snapshot']
            raise AssertionError('stream closed early')
        def start(prompt,thread=None):
            ident=secrets.token_hex(32);return api({'op':'start','spec':{'id':ident,'thread_id':thread or ident,'backend':backend,'cwd':temporary,'model':model,'prompt':prompt}})
        def wake(target,directions):
            spec={'id':secrets.token_hex(32),'name':'Live one-time wake','target':target,'directions':directions,'at_ms':int(time.time()*1000)+700}
            api({'op':'schedule','args':{'action':'create','spec':spec}})
            deadline=time.monotonic()+30
            while time.monotonic()<deadline:
                job=api({'op':'schedule','args':{'action':'get','id':spec['id']}})
                if job['last'] and job['last']['outcome']=='dispatched':return job,api({'op':'get','id':job['last']['run_id']})
                time.sleep(.1)
            raise AssertionError(('scheduled wake not accepted',job))
        marker='m'+secrets.token_hex(6);filename=secrets.token_hex(8)+'.txt'
        seed=start(f'This is a disposable scheduling test. The continuity marker is {marker}. Please run a shell command that writes one UTF-8 line containing seed followed by a line-feed character to {filename}, then acknowledge briefly.')
        _,seed_done=collect(seed);assert (root/filename).read_text()=='seed\n',repr((root/filename).read_text())
        job,run=wake({'type':'thread','id':seed['record']['spec']['thread_id']},f'This is the scheduled follow-up. Please append one UTF-8 line containing scheduled followed by a line-feed character exactly once to {filename}, then give the continuity marker from our earlier conversation. No other file changes are requested.')
        follow,done=collect(run);assert (root/filename).read_text()=='seed\nscheduled\n',repr((root/filename).read_text())
        answer=api({'op':'thread_result','id':run['record']['spec']['id']})['response'];assert marker in answer,answer
        assert done['backend_thread']==seed_done['backend_thread']
        manual=start('What is the current Boxen run trigger according to its injected metadata? A short label is sufficient.',seed['record']['spec']['thread_id'])
        collect(manual);answer=api({'op':'thread_result','id':manual['record']['spec']['id']})['response'];assert 'ordinary' in answer.lower(),answer
        resident=api({'op':'resident_create','spec':{'id':secrets.token_hex(32),'name':'Disposable scheduled resident','backend':backend,'model':model,'cwd':temporary,'auto_activate':False,'identity':'A disposable scheduling protocol test identity.'}})
        _,activation=wake({'type':'resident','id':resident['id']},'This activation requests no tools or outgoing mail. A private line about the current trigger is enough.')
        private,resident_done=collect(activation);assert resident_done['record']['mailbox']['message_ids']==[] and resident_done['record']['mailbox']['reason']=='schedule'
        assert not any((e['kind']=='tool_started' and e['data'].get('tool')=='boxen_send_message') or (e['kind']=='backend' and e['data'].get('method')=='item/tool/call' and e['data'].get('params',{}).get('tool')=='boxen_send_message') for e in private)
        if backend=='openrouter':
            assert any(e['kind']=='assistant_message' and e['data'].get('private') for e in private)
        else:
            assert any(e['kind']=='backend' and e['data'].get('method')=='item/completed' and e['data'].get('params',{}).get('item',{}).get('type')=='agentMessage' for e in private)
        usage=[e['data']['usage'] for e in events if e['kind']=='inference_exchange_completed']
        print(json.dumps({'ok':True,'backend':backend,'model':model,'runs':4,'scheduled_thread_effect_once':True,'canonical_thread_continuity':True,'manual_trigger_reset':True,'resident_without_mail':True,'retained_events':len(events),'reported_cost':sum(u.get('cost',0) for u in usage) if usage else None}))
    finally:
        for p in processes:
            if p.poll() is None:p.terminate();p.wait(timeout=5)
