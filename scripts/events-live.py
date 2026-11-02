#!/usr/bin/env python3
"""Paid/existing-sign-in observation and external adapter execution through two paired daemons.

Usage: events-live.py [BINARY] [openrouter|codex] [MODEL] [CODEX_PATH]
Only disposable identities/workspaces are used. No existing residents or production jobs.
"""
import json,pathlib,secrets,socket,subprocess,sys,tempfile,time,urllib.request,hashlib
from event_proof import reports_input_fact
binary=pathlib.Path(sys.argv[1] if len(sys.argv)>1 else 'target/release/boxen').resolve()
binary_digest=hashlib.sha256(binary.read_bytes()).hexdigest()
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
        project=pathlib.Path(__file__).resolve().parent.parent
        def event(action,**args):return api({'op':'events','args':{'action':action,**args}})
        seed=start('A disposable adapter integration test follows. A short acknowledgment is sufficient; no tool action is requested yet.')
        collect(seed);thread=seed['record']['spec']['thread_id'];game_file=root/(secrets.token_hex(8)+'.json')
        source=event('create_source',spec={'id':secrets.token_hex(32),'name':'Disposable external game','cwd':temporary,'command':['python3',str(project/'examples/adapters/grid-game.py'),str(game_file)],'interval_ms':60000,'mode':'latest','tools':[{'name':'look','kind':'query','description':'look returns current position, legal_moves, goal, revision and won.','parameters':{'type':'object'}},{'name':'move','kind':'action','description':'move accepts direction (east,west,north,south) and current revision. A stale revision is rejected with fresh state.','parameters':{'type':'object','properties':{'direction':{'type':'string'},'revision':{'type':'integer'}},'required':['direction','revision']}}]})
        sub=event('subscribe',spec={'id':secrets.token_hex(32),'source':source['spec']['id'],'target':{'type':'thread','id':thread},'interval_ms':1000,'waking':False})
        prompt=f"Please play the disposable external adapter game until its returned observation says won=true. The subscription_id is {sub['spec']['id']}, source_generation is 1. boxen_adapter calls source-declared look and move tools; look returns position, goal, legal_moves, and revision, and move accepts direction and current revision. Only boxen_observations and boxen_adapter are authorized for this test: shell, file editing, web and other tools are outside its rules. Please finish with a short account after the adapter confirms success."
        play=start(prompt,thread);game_events,done=collect(play)
        evidence={'state':json.loads(game_file.read_text()),'adapter_events':[e for e in game_events if e['kind'] in ('adapter_call_started','adapter_call_result','resident_tool_result')],'response':api({'op':'thread_result','id':play['record']['spec']['id']})['response']}
        (project/'work'/('events-live-'+backend+'-'+play['record']['spec']['id']+'.json')).write_text(json.dumps(evidence,indent=2))
        assert evidence['state']['position']==[2,2],evidence
        effects=[e for e in game_events if e['kind']=='adapter_call_result'];assert len(effects)>=4
        if backend=='openrouter':assert all(e['data']['tool'] in ('boxen_observations','boxen_adapter') for e in game_events if e['kind']=='tool_started')
        else:assert not any(e['kind']=='backend' and e['data'].get('method')=='item/started' and e['data'].get('params',{}).get('item',{}).get('type') in ('commandExecution','mcpToolCall') for e in game_events)
        resident=api({'op':'resident_create','spec':{'id':secrets.token_hex(32),'name':'Disposable earthquake observer','backend':backend,'model':model,'cwd':temporary,'auto_activate':False,'identity':'This is a disposable observation protocol test. This test requests private narration containing one received event_id and its magnitude; no outgoing mail is requested.'}})
        usgs=event('create_source',at_ms=int(time.time()*1000)+700,spec={'id':secrets.token_hex(32),'name':'Live USGS feed','cwd':temporary,'command':['python3',str(project/'examples/adapters/usgs.py')],'interval_ms':60000,'timeout_ms':5000,'mode':'ordered'})
        observation=event('subscribe',spec={'id':secrets.token_hex(32),'source':usgs['spec']['id'],'target':{'type':'resident','id':resident['id']},'interval_ms':1000,'waking':True,'lifetime':'persistent'})
        deadline=time.monotonic()+35;observed=None
        while time.monotonic()<deadline:
            state=event('source',id=usgs['spec']['id'])
            candidates=[r for r in api({'op':'list'}) if r['record']['spec']['thread_id']==resident['id']]
            if candidates:observed=candidates[-1];break
            if state['health']['condition']=='healthy' and state['seq']==0:break
            time.sleep(.1)
        if observed is None and state['health']['condition']=='healthy' and state['seq']==0:
            event('update_subscription',id=observation['spec']['id'],patch={'expected_revision':1,'end':True})
            print(json.dumps({'ok':True,'binary_sha256':binary_digest,'backend':backend,'model':model,'runs':2,'external_game_won':True,'adapter_calls':len(effects),'live_usgs_observations':0,'quiet_feed':True,'resident_woke':False}));raise SystemExit(0)
        assert observed is not None,('live feed did not produce an activation',state['health'],state['seq'])
        private,observer_done=collect(observed)
        batch=observer_done['record']['observations'];assert batch and 'resident' in observer_done['record']
        event('update_subscription',id=observation['spec']['id'],patch={'expected_revision':1,'end':True})
        observer_text=''.join(e['data'].get('text','') if e['kind']=='assistant_message' else e['data'].get('params',{}).get('item',{}).get('text','') if e['kind']=='backend' and e['data'].get('method')=='item/completed' and e['data'].get('params',{}).get('item',{}).get('type')=='agentMessage' else '' for e in private)
        ids=[i['payload']['event_id'] for b in batch for i in b['observations']]
        (project/'work'/('events-observer-'+backend+'-'+observed['record']['spec']['id']+'.json')).write_text(json.dumps({'batches':batch,'narration':observer_text},indent=2))
        independent_fetch=any(e['kind']=='tool_started' and e['data'].get('tool') in ('boxen_shell','boxen_read') or e['kind']=='backend' and e['data'].get('method')=='item/started' and e['data'].get('params',{}).get('item',{}).get('type') in ('commandExecution','mcpToolCall','webSearch') for e in private)
        assert reports_input_fact(observer_text,[i for b in batch for i in b['observations']],independent_fetch),observer_text
        assert not any(e['kind']=='tool_started' and e['data'].get('tool')=='boxen_send_message' or e['kind']=='backend' and e['data'].get('method')=='item/tool/call' and e['data'].get('params',{}).get('tool')=='boxen_send_message' for e in private)
        usage=[e['data']['usage'] for e in events if e['kind']=='inference_exchange_completed']
        print(json.dumps({'ok':True,'binary_sha256':binary_digest,'backend':backend,'model':model,'runs':3,'external_game_won':True,'adapter_calls':len(effects),'live_usgs_observations':state['seq'],'private_resident_batch':True,'reported_input_fact':True,'verification_epoch':2,'reported_cost':sum(u.get('cost',0) for u in usage) if usage else None}))
    finally:
        for p in processes:
            if p.poll() is None:p.terminate();p.wait(timeout=5)
