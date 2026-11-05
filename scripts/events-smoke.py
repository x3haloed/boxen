#!/usr/bin/env python3
"""Real paired daemons, external adapter processes, bounded event delivery."""
import hashlib
from smoke_helpers import fake_codex, windows_cpu
import json
import pathlib
import os
import secrets
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

ROOT=pathlib.Path(__file__).resolve().parent.parent
BINARY=pathlib.Path(sys.argv[1] if len(sys.argv)>1 else ROOT/'target/release/boxen').resolve()
COMMAND=(['/bin/bash'] if BINARY.suffix=='.com' else [])+[str(BINARY)]
def port():
    with socket.socket() as s:s.bind(('127.0.0.1',0));return s.getsockname()[1]
def now():return int(time.time()*1000)
with tempfile.TemporaryDirectory(prefix='boxen-schedules-') as temporary:
    directory=pathlib.Path(temporary);data=[directory/'a',directory/'b'];peers=[port(),port()];uis=[port(),port()];processes=[]
    def cli(i,*args):return subprocess.check_output(COMMAND+['--data-dir',str(data[i]),*args],text=True,timeout=15).strip()
    keys=[cli(i,'identity') for i in range(2)]
    for i in range(2):cli(i,'trust',keys[1-i],'Schedule fixture peer')
    def daemon(i):
        p=subprocess.Popen(COMMAND+['--data-dir',str(data[i]),'serve','--listen',f'127.0.0.1:{peers[i]}','--ui',f'127.0.0.1:{uis[i]}','--codex',str(fake_codex())],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
        processes.append(p);lines=[p.stdout.readline() for _ in range(3)];assert 'Local controls:' in lines[-1],lines
        return p,lines[-1].strip().split('#')[1]
    p0,t0=daemon(0);p1,t1=daemon(1)
    def api(request,watch=False,local_owner=False):
        index=1 if local_owner else 0;token=t1 if local_owner else t0
        r=urllib.request.Request(f'http://127.0.0.1:{uis[index]}/api/'+('watch' if watch else 'operation'),headers={'Authorization':'Bearer '+token,'Content-Type':'application/json'},data=json.dumps({'peer':keys[1],'address':f'127.0.0.1:{peers[1]}','request':request}).encode())
        response=urllib.request.urlopen(r,timeout=15);return response if watch else json.load(response)
    def schedule(action,**args):return api({'op':'schedule','args':{'action':action,**args}})
    def wait(job,predicate,seconds=10):
        deadline=time.monotonic()+seconds
        while time.monotonic()<deadline:
            value=schedule('get',id=job['definition']['id'])
            if predicate(value):return value
            time.sleep(.04)
        raise AssertionError(('schedule timeout',value))
    def collect(run,early=False):
        events=[]
        with api({'id':run['record']['spec']['id'],'after':0},True) as stream:
            for line in stream:
                b=json.loads(line);assert 'error' not in b,b;events+=b['events']
                if early and any(e['kind']=='backend' and e['data'].get('method')=='item/commandExecution/outputDelta' for e in events):
                    assert not b['done'];return events,b['snapshot']
                if b['done']:return events,b['snapshot']
        raise AssertionError('stream ended without completion')
    def start(prompt,thread=None):
        ident=secrets.token_hex(32)
        return api({'op':'start','spec':{'id':ident,'thread_id':thread or ident,'backend':'codex','cwd':temporary,'model':'fixture','prompt':prompt}})
    def definition(thread,prompt='Scheduled request',at=None,interval=None,resident=False):
        return {'id':secrets.token_hex(32),'name':'Timing proof','target':{'type':'resident' if resident else 'thread','id':thread},'directions':prompt,'at_ms':at or now()+350,'interval_ms':interval}
    def accepted(job):return wait(job,lambda j:j['last'] and j['last']['outcome']=='dispatched' and j['last'].get('origin',{}).get('revision')==job['revision'])
    def run_for(job):return api({'op':'get','id':job['last']['run_id']})
    def restart():
        global p1,t1
        if p1.poll() is None:p1.kill();p1.wait(timeout=5)
        p1,t1=daemon(1)
    def journal(job):return data[1]/'schedules'/(job['definition']['id']+'.jsonl')
    def rejected(call):
        try:call();raise AssertionError('operation was accepted unexpectedly')
        except urllib.error.HTTPError as e:assert e.code==422
    def cpu(pid):
        if os.name == 'nt':
            assert pid == p1.pid
            return windows_cpu(p1)
        path=pathlib.Path(f'/proc/{pid}/stat')
        if path.exists():
            parts=path.read_text().rsplit(')',1)[1].split();return (int(parts[11])+int(parts[12]))/os.sysconf('SC_CLK_TCK')
        value=subprocess.check_output(['ps','-p',str(pid),'-o','time='],text=True).strip();total=0
        for part in value.split(':'):total=total*60+float(part)
        return total
    def idle_cpu():
        start=cpu(p1.pid);begin=time.monotonic();time.sleep(1.4);elapsed=time.monotonic()-begin;used=cpu(p1.pid)-start
        assert used<elapsed*.08+.01,('daemon busy loop',used,elapsed)
        print(json.dumps({'daemon_idle_cpu_seconds':used,'wall_seconds':round(elapsed,3)}),flush=True)
    def event(action,**args):return api({'op':'events','args':{'action':action,**args}})
    def until(test,seconds=12):
        deadline=time.monotonic()+seconds
        while time.monotonic()<deadline:
            value=test()
            if value:return value
            time.sleep(.05)
        raise AssertionError('observation condition did not become true')
    def resident():return api({'op':'resident_create','spec':{'id':secrets.token_hex(32),'name':'Observation fixture','cwd':temporary,'auto_activate':False}})
    def source(command=None,mode='ordered',tools=None,interval=1000):
        spec={'id':secrets.token_hex(32),'name':'External source proof','cwd':temporary,'command':command or [],'interval_ms':interval,'mode':mode,'timeout_ms':1000,'tools':tools or []}
        value=event('create_source',spec=spec);assert event('create_source',spec=spec)['generation']==1;return value
    def subscribe(src,target,waking=False,kind='resident',expiry=None):
        spec={'id':secrets.token_hex(32),'source':src['spec']['id'],'target':{'type':kind,'id':target},'interval_ms':1000,'waking':waking,'lifetime':'until' if expiry else 'persistent'}
        if expiry:spec['expires_at_ms']=expiry
        value=event('subscribe',spec=spec);assert event('subscribe',spec=spec)['revision']==1;return value
    def current(src):return event('source',id=src['spec']['id'])
    def substate(sub):return event('subscription',id=sub['spec']['id'])
    def activate(r):return api({'op':'resident_activate','id':r['id'],'activation_id':secrets.token_hex(32)})
    def push(src,n,items):return event('publish',id=src['spec']['id'],generation=src['generation'],sequence=n,response={'protocol':'boxen-adapter-v1','observations':items})
    def item(n,payload=None):return {'id':str(n),'observed_at_ms':now(),'payload':payload or {'value':n}}
    def runs(target):return [r for r in api({'op':'list'}) if r['record']['spec']['thread_id']==target]
    def tool_started(run):
        with api({'id':run['record']['spec']['id'],'after':0},True) as stream:
            for line in stream:
                batch=json.loads(line)
                if any(e['kind']=='adapter_call_started' for e in batch['events']):return True
                assert not batch['done'],batch
        return False
    import http.server,threading
    report_token=secrets.token_hex(8)
    class Feed(http.server.BaseHTTPRequestHandler):
        def log_message(self,*args):pass
        def do_GET(self):
            data={'type':'FeatureCollection','features':[{'id':'opaque-event','properties':{'time':1700000000000,'updated':1700000001000,'mag':2.1,'place':report_token,'url':'https://example.invalid/event','status':'reviewed'},'geometry':{'type':'Point','coordinates':[1,2,3]}}]}
            self.send_response(200);self.send_header('Content-Type','application/json');self.end_headers();self.wfile.write(json.dumps(data).encode())
    http=http.server.ThreadingHTTPServer(('127.0.0.1',0),Feed);threading.Thread(target=http.serve_forever,daemon=True).start()
    try:
        passive=resident();awake=resident();usgs=source([sys.executable,str(ROOT/'examples/adapters/usgs.py'),'--url',f'http://127.0.0.1:{http.server_port}/feed'])
        psub=subscribe(usgs,passive['id']);wsub=subscribe(usgs,awake['id'],True)
        until(lambda:current(usgs)['seq']==1);until(lambda:substate(psub)['cutoff']==1)
        assert runs(passive['id'])==[]
        waking=until(lambda:runs(awake['id']));_,done=collect(waking[-1]);assert done['status']=='completed' and done['record']['resident']['id']==awake['id'] and done['record']['mailbox']['reason']=='observations'
        assert report_token in json.dumps(done['record'].get('observations',[]));assert 'private' in json.dumps(done['record'].get('observations',[])).lower() or done['record']['resident']
        manual=activate(passive);_,done=collect(manual);assert report_token in json.dumps(done['record'].get('observations',[]))
        assert substate(psub)['cursor']==1 and substate(wsub)['cursor']==1
        time.sleep(1.1);assert current(usgs)['seq']==1 and len(runs(awake['id']))==1
        assert api({'op':'events','args':{'action':'source','id':usgs['spec']['id']}},local_owner=True)['spec']['id']==usgs['spec']['id']
        print('PASS real USGS adapter parsing, independent collection, passive/waking batches, private resident activation, deduplication and shared operator access',flush=True)

        anonymous=start('Anonymous game thread seed');collect(anonymous);thread=anonymous['record']['spec']['thread_id'];game_path=directory/'game-state.json'
        game=source([sys.executable,str(ROOT/'examples/adapters/grid-game.py'),str(game_path)],mode='latest',interval=60000,tools=[{'name':'look','description':'look returns current position, legal moves and revision.','kind':'query'},{'name':'move','description':'move accepts direction and current revision; stale revisions are rejected.','kind':'action'}]);gsub=subscribe(game,thread,kind='thread')
        play=start('EVENT_GAME:'+json.dumps({'subscription_id':gsub['spec']['id'],'source_generation':game['generation']}),thread)
        events,done=collect(play);assert done['status']=='completed',events[-3:]
        assert json.loads(game_path.read_text())['position']==[2,2]
        assert api({'op':'thread_result','id':play['record']['spec']['id']})['response']=='EXTERNAL_GAME_WON'
        assert sum(e['kind']=='adapter_call_result' for e in events)==6
        assert current(game)['seq']==5 and len(current(game)['items'])==1
        print('PASS external game query/action/fresh feedback through managed tools, real moves, stale-action rejection, latest-state coalescing and anonymous response',flush=True)

        trace=directory/'observation-trace.jsonl';secret='fixture-'+secrets.token_hex(8)
        api({'op':'log_file','path':str(trace),'filter':{'level':'debug','events':['observation.'],'payloads':False}})
        raw=source();slow=resident();slow_sub=subscribe(raw,slow['id'])
        push(raw,1,[item(i,{'text':'x'*700}) for i in range(64)]);push(raw,2,[item(i,{'text':'y'*700}) for i in range(64,128)])
        assert push(raw,2,[])['duplicate'];assert current(raw)['seq']==128 and len(current(raw)['items'])<=64
        until(lambda:substate(slow_sub)['cutoff']==128)
        preview=event('read',id=slow_sub['spec']['id'])['batches'];assert preview[0]['gap']['through_seq']>=64 and len(json.dumps(preview))<16000
        push(raw,3,[item(129,{'api_key':secret})]);api({'op':'log_off'})
        diagnostics=[json.loads(l) for l in trace.read_text().splitlines()]
        assert any(e['event']=='observation.accepted' and e['fields']['source_id']==raw['spec']['id'] for e in diagnostics)
        assert secret not in trace.read_text() and all('payload' not in e for e in diagnostics)
        expiring=subscribe(raw,slow['id'],True,expiry=now()+700);time.sleep(1.1);until(lambda:substate(expiring)['ended'])
        assert runs(slow['id'])==[]
        print('PASS slow-consumer retention bounds, explicit loss, duplicate producer input and finite activity expiry without activation',flush=True)

        control=directory/'connector-mode';control.write_text('fail');broken=source([sys.executable,str(ROOT/'tests/event-adapter.py'),str(control)])
        ignored=resident();badsub=subscribe(broken,ignored['id'])
        until(lambda:current(broken)['health']['failures']>=3,seconds=15)
        health=current(broken)['health'];assert health['notice_seq']==1 and health['next_probe_ms']>health['last_failure_ms'];assert not current(broken)['items'];assert runs(ignored['id'])==[]
        collect(activate(ignored));again=activate(ignored);_,done=collect(again);assert not any(b['health'] for b in done['record'].get('observations',[]))
        control.write_text('recovered');until(lambda:current(broken)['health']['condition']=='healthy',seconds=18)
        assert current(broken)['health']['notice_seq']==1 and current(broken)['health']['pending_notice']
        control.write_text('flood');paused=event('update_source',id=broken['spec']['id'],patch={'expected_revision':1,'enabled':False});assert paused['generation']==2
        resumed=event('update_source',id=broken['spec']['id'],patch={'expected_revision':2,'enabled':True});assert resumed['generation']==3
        until(lambda:current(broken)['health']['condition']=='unavailable');assert not any('xxxxxx' in json.dumps(x) for x in current(broken)['items'])
        assert len(json.dumps(current(broken)['health']))<1600
        paused=event('update_source',id=broken['spec']['id'],patch={'expected_revision':3,'enabled':False})
        rejected(lambda:push(broken,1,[item(1)]))
        print('PASS unattended retry backoff, bounded health notices, recovery/flapping state, oversized adapter output and stale generation rejection',flush=True)

        action_control=directory/'action-mode';action_control.write_text('effect');effect=source([sys.executable,str(ROOT/'tests/event-adapter.py'),str(action_control)],interval=60000,tools=[{'name':'act','kind':'action','description':'A controlled effect followed by an interrupted response.'}])
        esub=subscribe(effect,thread,kind='thread')
        pending=start('EVENT_CALL:'+json.dumps({'subscription_id':esub['spec']['id'],'source_generation':1,'tool':'act','arguments':{}}),thread);assert tool_started(pending)
        until(lambda:action_control.with_suffix('.effect').exists())
        begin=time.monotonic();event('update_subscription',id=esub['spec']['id'],patch={'expected_revision':1,'end':True});_,done=collect(pending)
        assert time.monotonic()-begin<3 and done['status']=='completed';assert 'unknown' in api({'op':'thread_result','id':pending['record']['spec']['id']})['response']
        assert action_control.with_suffix('.effect').read_text()=='1'
        restart();assert substate(esub)['ended'];collect(start('New work after the unknown adapter outcome',thread));assert action_control.with_suffix('.effect').read_text()=='1'
        rejected(lambda:event('update_subscription',id=esub['spec']['id'],patch={'expected_revision':2,'enabled':True}))
        print('PASS stop during adapter effect, bounded interruption, retained unknown result, no revival/replay on restart and continued ordinary work',flush=True)
        cancelled_sub=subscribe(effect,thread,kind='thread')
        cancelled=start('EVENT_CALL:'+json.dumps({'subscription_id':cancelled_sub['spec']['id'],'source_generation':1,'tool':'act','arguments':{}}),thread);assert tool_started(cancelled)
        until(lambda:action_control.with_suffix('.effect').read_text()=='2')
        api({'op':'control','id':cancelled['record']['spec']['id'],'action':{'type':'cancel'}})
        _,cancelled_done=collect(cancelled);assert cancelled_done['status'] in ('cancelled','interrupted')
        event('update_subscription',id=cancelled_sub['spec']['id'],patch={'expected_revision':1,'end':True})
        print('PASS ordinary run cancellation interrupts an adapter call without replaying its effect',flush=True)
        crash_sub=subscribe(effect,thread,kind='thread')
        crash=start('EVENT_CALL:'+json.dumps({'subscription_id':crash_sub['spec']['id'],'source_generation':1,'tool':'act','arguments':{}}),thread);assert tool_started(crash)
        until(lambda:action_control.with_suffix('.effect').read_text()=='3')
        child_pid=int(action_control.with_suffix('.pid').read_text());p1.kill();p1.wait(timeout=5)
        # This deliberately uncooperative fixture outlives a killed daemon. Its
        # exact PID is private fixture output, so cleanup never targets a service.
        try:os.kill(child_pid,9)
        except ProcessLookupError:pass
        restart();assert api({'op':'get','id':crash['record']['spec']['id']})['status']=='interrupted'
        collect(start('New work after an interrupted adapter effect',thread));assert action_control.with_suffix('.effect').read_text()=='3'
        event('update_subscription',id=crash_sub['spec']['id'],patch={'expected_revision':1,'end':True})
        print('PASS crash with a missing adapter result preserves uncertainty and never dispatches the historical effect again',flush=True)


        timers=api({'op':'schedule','args':{'action':'list','limit':50}})['jobs'];source_timer=next(j for j in timers if j['definition']['target']=={'type':'source','id':usgs['spec']['id']})
        timer_path=data[1]/'schedules'/(source_timer['definition']['id']+'.jsonl');assert timer_path.stat().st_size<100000
        history=schedule('history',id=source_timer['definition']['id']);assert history['first_retained_sequence']>1
        store=data[1]/'observations/pending.json';assert store.stat().st_size<4*1024*1024
        saved=store.with_suffix('.saved');store.rename(saved);store.mkdir()
        until(lambda:event('list')['unavailable'],seconds=6);idle_cpu()
        before=len(api({'op':'list'}));time.sleep(1.1);assert len(api({'op':'list'}))==before
        store.rmdir();saved.rename(store);restart();assert event('list')['unavailable'] is None
        assert not api({'op':'log_status'})['enabled']
        print('PASS bounded internal timer history, isolated observation-store failure, idle CPU, no activation loop and repair/reload with diagnostics off',flush=True)
    finally:
        http.shutdown()
        for p in processes:
            if p.poll() is None:p.terminate();p.wait(timeout=5)
