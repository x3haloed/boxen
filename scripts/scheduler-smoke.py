#!/usr/bin/env python3
"""Paired real daemons + controlled Codex: durable timing, contracts, recovery."""
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
    try:
        idle=api({'op':'resident_create','spec':{'id':secrets.token_hex(32),'name':'Idle timer target','cwd':temporary,'auto_activate':False}})
        schedule('create',spec=definition(idle['id'],at=now()+86400000,resident=True))
        idle_cpu()
        trace=directory/'schedule-trace.jsonl';api({'op':'log_file','path':str(trace),'filter':{'level':'trace','events':['rpc.sent','schedule.'],'payloads':True}})
        seed=start('Original thread');_,seed_snapshot=collect(seed);assert seed_snapshot['status']=='completed';thread=seed['record']['spec']['thread_id']
        spec=definition(thread)
        job=json.loads(cli(0,'schedule',f'127.0.0.1:{peers[1]}',keys[1],json.dumps({'action':'create','spec':spec})))
        assert schedule('create',spec=spec)['revision']==1
        job=accepted(job);run=run_for(job);early,_=collect(run,True);events,done=collect(run)
        assert done['status']=='completed' and done['backend_thread']==seed_snapshot['backend_thread']
        assert run['record']['spec']['thread_id']==thread and 'resident' not in run['record']
        assert run['record']['scheduled']['schedule_id']==spec['id']
        assert api({'op':'thread_result','id':run['record']['spec']['id']})['response']=='Fixture result'
        api({'op':'log_off'});diagnostics=[json.loads(l) for l in trace.read_text().splitlines()]
        assert any(e['event']=='rpc.sent' and e['fields'].get('method')=='thread/resume' for e in diagnostics)
        assert any(e['event']=='schedule.dispatching' and e['fields']['schedule_id']==spec['id'] for e in diagnostics)
        starts=[e for e in diagnostics if e['event']=='rpc.sent' and e['fields'].get('method')=='turn/start']
        assert starts[-1]['payload']['params']['input'][1]['text'].find('host-local scheduled event')>=0
        assert not any('developerInstructions' in e['payload']['params'] for e in diagnostics if e['event']=='rpc.sent' and e['fields'].get('method') in ('thread/start','thread/resume'))
        assert not job['enabled'] and job['next_at_ms'] is None
        assert api({'op':'schedule','args':{'action':'get','id':spec['id']}},local_owner=True)['definition']['id']==spec['id']
        print('PASS CLI/API one-time wake, native thread resumption, ordinary response, early stream and shared operator access',flush=True)

        repeating=schedule('create',spec=definition(thread,at=now()-3600000,interval=60000));repeating=accepted(repeating)
        assert repeating['last']['origin']['coalesced_occurrences']>=60 and repeating['next_at_ms']>now()
        collect(run_for(repeating))
        paused=schedule('update',id=repeating['definition']['id'],patch={'expected_revision':1,'enabled':False})
        rejected(lambda:schedule('update',id=paused['definition']['id'],patch={'expected_revision':1,'enabled':True}))
        resumed=schedule('update',id=paused['definition']['id'],patch={'expected_revision':2,'enabled':True,'at_ms':now()+250,'interval_ms':0,'directions':'Edited scheduled request'})
        resumed=accepted(resumed);assert resumed['last']['origin']['revision']==3
        collect(run_for(resumed));schedule('update',id=resumed['definition']['id'],patch={'expected_revision':3,'delete':True})
        assert schedule('get',id=resumed['definition']['id'])['deleted']
        print('PASS anchored recurring coalescing, pause/edit/resume, revision conflict and retained deletion',flush=True)

        slow=start('SLOW ordinary request',thread)
        queued=schedule('create',spec=definition(thread));queued=wait(queued,lambda j:j['blocked'])
        assert not queued['claimed']
        collect(slow);queued=accepted(queued);collect(run_for(queued))
        print('PASS busy-target deferral and completion notification wake without concurrent thread execution',flush=True)

        resident=api({'op':'resident_create','spec':{'id':secrets.token_hex(32),'name':'Scheduled resident','cwd':temporary,'auto_activate':False}})
        client=secrets.token_hex(32)
        api({'op':'mailbox_send','client_id':client,'message':{'id':secrets.token_hex(32),'to':{'type':'resident','id':resident['id']},'subject':'Silent batch','body':'SILENT queued mail'}})
        beat=accepted(schedule('create',spec=definition(resident['id'],prompt='A heartbeat pattern without a reply obligation',resident=True)))
        e,s=collect(run_for(beat));assert s['status']=='completed' and s['record']['mailbox']['reason']=='schedule'
        assert len(s['record']['mailbox']['message_ids'])==1
        assert api({'op':'client_inbox','client_id':client})['messages']==[]
        independent=accepted(schedule('create',spec=definition(resident['id'],resident=True)))
        _,s=collect(run_for(independent));assert s['record']['mailbox']['message_ids']==[]
        own=api({'op':'resident_activate','id':resident['id'],'activation_id':secrets.token_hex(32),'directions':'SCHEDULE_SELF'});assert collect(own)[1]['status']=='completed'
        another=api({'op':'resident_create','spec':{'id':secrets.token_hex(32),'name':'Other identity','cwd':temporary,'auto_activate':False}})
        otherjob=schedule('create',spec=definition(another['id'],at=now()+86400000,resident=True))
        scope=api({'op':'resident_activate','id':resident['id'],'activation_id':secrets.token_hex(32),'directions':'SCHEDULE_SCOPE:'+otherjob['definition']['id']});assert collect(scope)[1]['status']=='completed'
        print('PASS resident scheduling, queued mail/no-reply and empty activations, self-management and identity scope',flush=True)

        offline=schedule('create',spec=definition(thread,at=now()+1000));p1.kill();p1.wait(timeout=5);time.sleep(1.1);restart()
        offline=accepted(offline);collect(run_for(offline))
        # Simulate a crash after durable claim, before acceptance: no run exists.
        reserved=schedule('create',spec=definition(thread,at=now()+1500));p1.kill();p1.wait(timeout=5)
        rows=[json.loads(l) for l in journal(reserved).read_text().splitlines()];event=rows[-1];state=event['state'];j=state['job'];seq=event['seq']+1
        origin={'schedule_id':j['definition']['id'],'revision':j['revision'],'due_at_ms':j['next_at_ms'],'dispatch_started_at_ms':now(),'coalesced_occurrences':0};j['claimed']=origin;j['last_seq']=seq
        with journal(reserved).open('a') as f:f.write(json.dumps({'seq':seq,'at':now(),'kind':'claimed','detail':{},'state':state})+'\n')
        expected=hashlib.sha256(f"boxen-schedule-v1:{origin['schedule_id']}:{origin['revision']}:{origin['due_at_ms']}".encode()).hexdigest()
        time.sleep(1.6);restart();reserved=accepted(reserved);assert reserved['last']['run_id']==expected;collect(run_for(reserved))
        # Rewind timing state to its committed claim, leaving accepted run evidence.
        accepted_job=schedule('create',spec=definition(thread,prompt='SLOW accepted occurrence'));accepted_job=accepted(accepted_job);old_id=accepted_job['last']['run_id']
        p1.kill();p1.wait(timeout=5);lines=journal(accepted_job).read_text().splitlines();index=next(i for i,l in enumerate(lines) if json.loads(l)['kind']=='claimed');journal(accepted_job).write_text('\n'.join(lines[:index+1])+'\n')
        restart();recovered=accepted(accepted_job);assert recovered['last']['run_id']==old_id
        old=api({'op':'get','id':old_id});assert old['status']=='interrupted'
        fresh=start('New work after an uncertain scheduled run',thread);assert collect(fresh)[1]['status']=='completed'
        print('PASS offline one-time recovery, unaccepted-claim dispatch, accepted-claim reconciliation and continued uncertain thread availability',flush=True)

        failing=schedule('create',spec=definition(thread,at=now()+700));path=journal(failing);saved=path.with_suffix('.saved');path.rename(saved);path.mkdir()
        failed=wait(failing,lambda j:j.get('unavailable'));assert failed['unavailable'];idle_cpu()
        healthy=accepted(schedule('create',spec=definition(thread)));collect(run_for(healthy))
        path.rmdir();saved.rename(path);restart();failing=accepted(failing);collect(run_for(failing))
        bad_id=secrets.token_hex(32);bad=data[1]/'schedules'/(bad_id+'.jsonl');bad.write_text('{broken}\n');restart()
        assert any(i['id']==bad_id for i in schedule('list')['issues'])
        healthy=accepted(schedule('create',spec=definition(thread)));collect(run_for(healthy))
        assert not api({'op':'log_status'})['enabled']
        history=schedule('history',id=offline['definition']['id']);assert history['events'] and history['next_after']>0
        first=schedule('list',limit=2);assert len(first['jobs'])==2 and first['next_after_id']
        second=schedule('list',limit=2,after_id=first['next_after_id'])
        assert len(second['jobs'])==2 and not ({j['definition']['id'] for j in first['jobs']} & {j['definition']['id'] for j in second['jobs']})
        assert all('directions' not in j['definition'] for j in first['jobs']+second['jobs'])
        print('PASS isolated storage failure/recovery, corrupt-schedule isolation, paged history and diagnostics-off execution',flush=True)
    finally:
        for p in processes:
            if p.poll() is None:p.terminate();p.wait(timeout=5)
