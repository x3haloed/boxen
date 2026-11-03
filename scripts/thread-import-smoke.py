#!/usr/bin/env python3
"""Import via paired daemons without inference; native fixture history and same-ID resume."""
import json
import concurrent.futures
import os
import pathlib
import secrets
import socket
import subprocess
import sys
import tempfile
import urllib.error
import urllib.request

ROOT=pathlib.Path(__file__).resolve().parent.parent
BINARY=pathlib.Path(sys.argv[1] if len(sys.argv)>1 else ROOT/'target/release/boxen').resolve()
COMMAND=(['/bin/bash'] if BINARY.suffix=='.com' else [])+[str(BINARY)]
NATIVE='019a0000-0000-7000-8000-000000000001'

def port():
    with socket.socket() as s:
        s.bind(('127.0.0.1',0));return s.getsockname()[1]

with tempfile.TemporaryDirectory(prefix='boxen-thread-import-') as temporary:
    base=pathlib.Path(temporary).resolve()
    workspace=base/'native workspace';workspace.mkdir()
    fixture=ROOT/'tests/fake-codex.py'
    if os.name=='nt':
        wrapper=base/'fake-codex.cmd'
        wrapper.write_text('@echo off\n"'+sys.executable+'" "'+str(fixture)+'" %*\n')
        fixture=wrapper
    else:
        fixture.chmod(0o755)
    homes=[base/'a',base/'b'];peers=[port(),port()];uis=[port(),port()]
    processes=[None,None];tokens=[None,None]
    environment={**os.environ,'BOXEN_IMPORT_FIXTURE':str(workspace)}
    def cli(i,*args,check=True):
        return subprocess.run(COMMAND+['--data-dir',str(homes[i]),*args],capture_output=True,text=True,encoding='utf-8',check=check,timeout=20)
    keys=[cli(i,'identity').stdout.strip() for i in range(2)]
    for i in range(2):cli(i,'trust',keys[1-i],'Other test peer')
    def start(i):
        p=subprocess.Popen(COMMAND+['--data-dir',str(homes[i]),'serve','--listen',f'127.0.0.1:{peers[i]}','--ui',f'127.0.0.1:{uis[i]}','--codex',str(fixture)],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True,encoding='utf-8',env=environment)
        processes[i]=p
        lines=[p.stdout.readline().strip() for _ in range(3)]
        assert 'Local controls:' in lines[-1],lines
        tokens[i]=lines[-1].split('#')[1]
    def stop(i):
        p=processes[i]
        if p and p.poll() is None:p.terminate();p.wait(timeout=10)
        if p:
            p.stdout.close();p.stderr.close()
    def operation(i,request,target=1):
        packet={'peer':keys[target],'address':'' if i==target else f'127.0.0.1:{peers[target]}','request':request}
        req=urllib.request.Request(f'http://127.0.0.1:{uis[i]}/api/operation',data=json.dumps(packet).encode(),headers={'Content-Type':'application/json','Authorization':'Bearer '+tokens[i]})
        try:
            with urllib.request.urlopen(req,timeout=20) as response:return json.load(response)
        except urllib.error.HTTPError as error:
            error.add_note(error.read().decode());raise
    def rejected(call):
        try:call()
        except urllib.error.HTTPError as e:
            assert e.code in (422,502),e.code
            return e.read().decode()
        raise AssertionError('invalid operation was accepted')
    def import_request(ident,native=NATIVE):return {'op':'thread_import','spec':{'id':ident,'backend_thread_id':native}}
    def log():return [json.loads(line) for path in workspace.glob('requests-*.jsonl') for line in path.read_text().splitlines()]
    try:
        start(0);start(1)
        diagnostics=base/'import-diagnostics.jsonl'
        cli(0,'log-file',f'127.0.0.1:{peers[1]}',keys[1],str(diagnostics),'--level','debug','--targets','workload,runtime','--events','thread_import')
        for fault in ['mismatch-id','in-progress','partial-items','repeat-cursor','large-item','changing-source']:
            marker=workspace/('fixture-'+fault);marker.touch()
            rejected(lambda:operation(0,import_request(secrets.token_hex(32))))
            marker.unlink(missing_ok=True)
            (workspace/'fixture-mismatch-id').unlink(missing_ok=True)
            assert operation(0,{'op':'list'})==[],'failed capture published a partial import'
        print('PASS malformed, partial, active, oversized and changing native history is rejected without publishing a partial import')
        ident=secrets.token_hex(32)
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            accepted=list(pool.map(lambda _:operation(0,import_request(ident)),range(2)))
        imported=accepted[0];assert accepted[1]==imported
        assert imported['status']=='completed' and imported['backend_thread']==NATIVE
        assert imported['record']['spec']['thread_id']==ident and imported['record']['spec']['prompt']==''
        assert imported['record']['owner']==keys[0]
        assert 'resident' not in imported['record'] and 'mailbox' not in imported['record']
        assert imported['record']['imported']['metadata']['name']=='Imported fixture'
        assert 'turns' not in imported['record']['imported']['metadata']
        assert all(r.get('method') in ('initialize','initialized','thread/read','thread/turns/list') for r in log())
        assert imported['record']['imported']['turn_count']==2
        assert len(operation(0,{'op':'list'}))==1
        assert operation(0,{'op':'resident_list'})==[]
        before=len(log())
        assert operation(0,import_request(ident))==imported
        assert json.loads(cli(0,'import-codex',f'127.0.0.1:{peers[1]}',keys[1],NATIVE,'--id',ident).stdout)==imported
        assert len(log())==before,'idempotent retry reread the native source'
        rejected(lambda:operation(0,import_request(ident,'019a0000-0000-7000-8000-000000000002')))
        rejected(lambda:operation(0,import_request(secrets.token_hex(32))))
        rejected(lambda:operation(1,{'op':'get','id':ident}))
        rejected(lambda:operation(1,import_request(ident)))
        missing=secrets.token_hex(32)
        rejected(lambda:operation(0,import_request(missing,'019a0000-0000-7000-8000-000000000099')))
        assert len(operation(0,{'op':'list'}))==1
        cli(0,'log-off',f'127.0.0.1:{peers[1]}',keys[1])
        records=[json.loads(line) for line in diagnostics.read_text().splitlines()]
        assert any(r['event']=='thread_import.accepted' for r in records)
        assert any(r['event']=='thread_import.history_page' for r in records)
        assert all('payload' not in r for r in records)
        assert 'Original source question' not in diagnostics.read_text()
        print('PASS read-only snapshot import, ownership, retry/conflict and missing-source isolation')

        events=[json.loads(line) for line in cli(0,'watch',f'127.0.0.1:{peers[1]}',keys[1],ident,'--once').stdout.splitlines()]
        assert [e['data']['turn']['id'] for e in events if e['kind']=='codex_import_turn']==['old-turn-1','old-turn-2']
        assert any(e['kind']=='backend' and e['data']['params']['item'].get('text')=='Native historical answer 🐈' for e in events)
        assert all(e['data']['historical'] for e in events if e['kind']=='backend')
        assert [e['seq'] for e in events]==list(range(1,len(events)+1))
        stop(1);start(1)
        (workspace/'fixture-mismatch-id').touch()
        assert operation(0,import_request(ident))==imported
        replay=[json.loads(line) for line in cli(0,'watch',f'127.0.0.1:{peers[1]}',keys[1],ident,'--once').stdout.splitlines()]
        assert replay==events
        (workspace/'fixture-mismatch-id').unlink()
        # Unpublished staging cannot become a discoverable or executed workload.
        stale=homes[1]/'import-staging'/'unpublished';stale.mkdir(parents=True)
        (stale/'record.json').write_text('{}')
        stop(1);start(1)
        assert len(operation(0,{'op':'list'}))==1
        print('PASS complete paginated native snapshot, historical provenance, replay and daemon restart')

        if os.environ.get('BOXEN_PROMOTION_PROBE')=='1':
            sys.path.insert(0,str(ROOT/'tests'))
            from promotion_checks import check_promotion
            check_promotion(globals())
            sys.exit(0)

        follow=operation(0,{'op':'start','spec':{'id':secrets.token_hex(32),'thread_id':ident,'backend':'codex','cwd':str(workspace),'model':'fixture','prompt':'New request after import'}})
        follow_id=follow['record']['spec']['id']
        received=[json.loads(line) for line in cli(0,'watch',f'127.0.0.1:{peers[1]}',keys[1],follow_id,'--once').stdout.splitlines()]
        assert any(e['kind']=='status' and e['data']['status']=='completed' for e in received)
        assert next(e['data']['id'] for e in received if e['kind']=='backend_thread')==NATIVE
        assert not any(e['kind']=='backend_session_rebuilt' for e in received)
        assert operation(0,{'op':'thread_result','id':follow_id})['response']=='Fixture result'
        assert any(r.get('method')=='turn/start' and r['params']['input'][0]['text']=='New request after import' for r in log())
        slow_spec={'id':secrets.token_hex(32),'thread_id':ident,'backend':'codex','cwd':str(workspace),'model':'fixture','prompt':'SLOW interrupted imported request'}
        operation(0,{'op':'start','spec':slow_spec})
        import time
        for _ in range(100):
            if operation(0,{'op':'get','id':slow_spec['id']})['status']=='running':break
            time.sleep(.02)
        else:raise AssertionError('controlled imported follow-up never started')
        stop(1);start(1)
        assert operation(0,{'op':'get','id':slow_spec['id']})['status']=='interrupted'
        repaired={**slow_spec,'id':secrets.token_hex(32),'prompt':'New request after interruption'}
        operation(0,{'op':'start','spec':repaired})
        resumed=[json.loads(line) for line in cli(0,'watch',f'127.0.0.1:{peers[1]}',keys[1],repaired['id'],'--once').stdout.splitlines()]
        assert any(e['kind']=='status' and e['data']['status']=='completed' for e in resumed)
        assert any(e['kind']=='recovery_notice' for e in resumed)
        assert all('developerInstructions' not in r['params'] for r in log() if r.get('method')=='thread/resume')
        (workspace/'fixture-forget-session').touch()
        rejected_run=operation(0,{'op':'start','spec':{'id':secrets.token_hex(32),'thread_id':ident,'backend':'codex','cwd':str(workspace),'model':'fixture','prompt':'Cannot resume'}})
        failed=[json.loads(line) for line in cli(0,'watch',f'127.0.0.1:{peers[1]}',keys[1],rejected_run['record']['spec']['id'],'--once').stdout.splitlines()]
        assert any(e['kind']=='status' and e['data']['status']=='failed' for e in failed)
        assert not any(r.get('method')=='thread/start' for r in log())
        assert not any(e['kind']=='backend_session_rebuilt' for e in failed)
        print('PASS explicit follow-up resumes original native thread; failed resume never reconstructs or starts a replacement')
    finally:
        stop(0);stop(1)
