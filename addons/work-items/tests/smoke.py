#!/usr/bin/env python3
"""Real Boxen HTTP + workflow process, controlled Codex; no Azure/model/product effects."""
import argparse,json,pathlib,subprocess,tempfile,time,socket,sqlite3,hashlib,os,sys,urllib.request
from contextlib import contextmanager
@contextmanager
def database(path):
    c=sqlite3.connect(path)
    try:
        with c:yield c
    finally:c.close()
p=argparse.ArgumentParser(description=__doc__);p.add_argument('--boxen',required=True,type=pathlib.Path);p.add_argument('--service',required=True,type=pathlib.Path);a=p.parse_args();boxen=a.boxen.resolve();service=a.service.resolve();repo=pathlib.Path(__file__).resolve().parents[3]
def port():
    with socket.socket() as s:s.bind(('127.0.0.1',0));return s.getsockname()[1]
def digest(s):return hashlib.sha256(s.encode()).hexdigest()
with tempfile.TemporaryDirectory(prefix='boxen-work-items-') as tmp:
    root=pathlib.Path(tmp).resolve();home=root/'workflow';legacy_home=root/'legacy';boxen_home=root/'boxen';pack=root/'pack';pack.mkdir();(pack/'scripts').mkdir()
    worker=None;daemon=None;peer_port=port();ui=port()
    fixture=root/'fake-codex.py'
    fixture_source=(repo/'tests/fake-codex.py').read_text().replace('m["body"].startswith("REPLY")','(m["body"].startswith("[Work-item service") or m["body"].startswith("The observed assignment")) and "SIMULATE_MISSING_HANDOFF" not in m["body"]')
    fixture_source=fixture_source.replace('            response, sent=invoke("boxen_send_message",send,11)','''            if "boxen.work-report.v1" in incoming["body"]:
                report=json.JSONDecoder().raw_decode(incoming["body"].split("Example: ",1)[1])[0]
                report["summary"]="Controlled explicit structured handoff"
                report["cleanup"]["no_owned_runtime_resources"]=True
                send["body"]=json.dumps(report)
            response, sent=invoke("boxen_send_message",send,11)''')
    fixture_source='import base64,re\n'+fixture_source
    browser_command_marker='    notification("item/started", {"item": {"id": "command",'
    assert fixture_source.count(browser_command_marker)==1
    fixture_source=fixture_source.replace(browser_command_marker,'''    browser_marker=re.search(r'BOXEN_BROWSER_APPROVAL_FIXTURE:([A-Za-z0-9+/=]+)',prompt)
    browser_fixture=json.loads(base64.b64decode(browser_marker.group(1))) if browser_marker else {"command":"fixture command","cwd":""}
    command_for_approval=browser_fixture["command"]
'''+browser_command_marker)
    fixture_source=fixture_source.replace('"command": "fixture command"','"command": command_for_approval')
    fixture_source=fixture_source.replace('"itemId": "command"}})', '"itemId": "command", "cwd":browser_fixture["cwd"]}})')
    fixture.write_text(fixture_source,encoding='utf-8')
    wrapper=root/'codex.cmd';wrapper.write_text('@echo off\n"'+sys.executable+'" "'+str(fixture)+'" %*\n');codex=wrapper if os.name=='nt' else fixture
    if os.name!='nt':fixture.chmod(0o755)
    manifest=digest('fixture pack');definition=digest('fixture coder');client=digest('workflow client')
    (pack/'scripts/Inspect-Assignment.ps1').write_text('param([string]$AssignmentPath,[string]$PackRoot)\n@{assignment_inputs_valid=$true;pack=@{manifest_sha256="'+manifest+'"}} | ConvertTo-Json -Depth 4\n')
    def command(*parts,which=home,check=True):
        r=subprocess.run([str(service),'--home',str(which),*parts],capture_output=True,text=True,timeout=70)
        if check:assert r.returncode==0,(r.returncode,r.stderr,r.stdout)
        return r
    def start_boxen():
        global daemon,owner,token
        daemon=subprocess.Popen([str(boxen),'--data-dir',str(boxen_home),'serve','--listen',f'127.0.0.1:{peer_port}','--ui',f'127.0.0.1:{ui}','--codex',str(codex)],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True,creationflags=subprocess.CREATE_NO_WINDOW if os.name=='nt' else 0)
        lines=[daemon.stdout.readline().strip() for _ in range(3)];assert 'Local controls:' in lines[-1]
        owner=lines[0].split(': ',1)[1];token=lines[-1].split('#')[1]
    def api(request):
        r=urllib.request.Request(f'http://127.0.0.1:{ui}/api/operation',data=json.dumps({'peer':owner,'address':'','request':request}).encode(),headers={'Authorization':'Bearer '+token,'Content-Type':'application/json'})
        return json.load(urllib.request.urlopen(r,timeout=15))
    def start_worker():
        global worker
        worker=subprocess.Popen([str(service),'--home',str(home),'run'],stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL,creationflags=subprocess.CREATE_NO_WINDOW if os.name=='nt' else 0)
    def stop(process):
        if process and process.poll() is None:process.terminate();process.wait(timeout=10)
    def wait(predicate):
        deadline=time.monotonic()+40
        while time.monotonic()<deadline:
            value=predicate()
            if value:return value
            time.sleep(.2)
        evidence=repo/'work'/'approval-smoke-failure';evidence.mkdir(parents=True,exist_ok=True)
        (evidence/'fake-codex.py').write_bytes(fixture.read_bytes())
        runs=api({'op':'list'});(evidence/'runs.json').write_text(json.dumps(runs),encoding='utf-8')
        with sqlite3.connect(home/'work-items.sqlite3') as source_db, sqlite3.connect(evidence/'service.sqlite3') as target_db:source_db.backup(target_db)
        for run in runs:
            ident=run['record']['spec']['id'];journal=boxen_home/'runs'/ident/'events.jsonl'
            if journal.exists():(evidence/(ident+'.jsonl')).write_bytes(journal.read_bytes())
        print('Failure statuses:',[(r['status'],r.get('detail')) for r in runs])
        raise AssertionError('Timed out waiting for workflow condition; synthetic evidence in '+str(evidence))
    def state(job):
        with database(home/'work-items.sqlite3') as c:return c.execute('select state from work where id=?',(job,)).fetchone()[0]
    def idle():return all(r['status'] in ('completed','failed','interrupted','cancelled') for r in api({'op':'list'}))
    def request_file(name,v):
        path=root/(name+'.json');path.write_text(json.dumps(v),encoding='utf-8');return str(path)
    try:
        start_boxen()
        api({'op':'resident_definition','args':{'action':'create','spec':{'id':definition,'name':'coder','identity':'RESIDENT_READ','cwd':str(pack),'model':'fixture','auto_activate':True}}})
        command('status',which=legacy_home,check=False)
        cfg={'enabled':False,'organization':'https://dev.azure.com/example','project':'Example','project_id':'11111111-1111-4111-8111-111111111111','assignee':'operator@example.invalid','marker':'Implementation: V2','board_column_field':'Example.Column','board_done_field':'Example.Done','interval_minutes':15};source=cfg['organization']+'|'+cfg['project_id']
        with database(legacy_home/'work-items.sqlite3') as c:
            c.execute('insert into ado_config values(1,?)',(json.dumps(cfg),))
            c.executescript('create table interpretation_ownership(source,item,json);create table interpretation_requests(source,item,decision,state,seq);create table jobs(source,item,id,state,actor,version,commit_sha,worktree,branch);')
            for item in [1,2]:c.execute('insert into ado_items values(?,?,1,?,?,1,?,?,?)',(source,item,'Synthetic work','Doing','Eligible',json.dumps({'id':item,'rev':1,'fields':{'System.Title':'Synthetic work'}}),int(time.time()*1000)))
        config={'boxen_home':str(boxen_home),'url':f'http://127.0.0.1:{ui}','owner':owner,'client_id':client,'definition_id':definition,'definition_revision':1,'pack_root':str(pack),'pack_hash':manifest,'capacity':1}
        command('init','--config-file',request_file('config',config),'--legacy-db',str(legacy_home/'work-items.sqlite3'))
        # No real Azure calls belong to this controlled suite.
        with database(home/'work-items.sqlite3') as c:
            cfg['enabled']=False;c.execute('update ado_config set json=?',(json.dumps(cfg),))
        def assignment(item,key=None,new=True):
            packet=json.loads(command('packet',str(item)).stdout);d=packet['source_digest'];wt=root/('worktree-'+str(item));wt.mkdir(exist_ok=True)
            return {'item':item,'idempotency_key':key or 'assign-'+str(item),'source_digest':d,'new_copy':new,'context':{'decision':'new_implementation','evidence':['Explicit synthetic operator assignment']},'assignment':{'job_id':'fixture-'+str(item),'assignment_revision':'1','repository_root':str(root),'worktree':str(wt),'branch':'fixture','base_commit':'1'*40,'evidence_dir':str(root/'evidence'),'project_pack':{'manifest_sha256':manifest},'work_item':{'id':item,'revision':1,'source_digest':d},'task':{'request':'Controlled assignment transport check','acceptance_criteria':['One explicit mailbox reply']},'delivery':{f:False for f in ['push','create_or_update_pr','merge','deploy','autocomplete']},'runtime':{'required':False}}}
        first=assignment(1);file1=request_file('first',first);r1=json.loads(command('assign','--request-file',file1).stdout)
        inspector=pack/'scripts/Inspect-Assignment.ps1';original=inspector.read_bytes();inspector.write_text('throw "Readiness changed after accepted assignment"')
        assert json.loads(command('assign','--request-file',file1).stdout)['idempotent']
        inspector.write_bytes(original)
        start_worker();wait(lambda:state(r1['job_id'])=='delivered');wait(idle)
        r2=json.loads(command('assign','--request-file',request_file('second',assignment(2))).stdout)
        time.sleep(2.5);assert state(r2['job_id'])=='queued' and len(api({'op':'resident_list'}))==1
        stop(worker);start_worker();time.sleep(2.5);assert state(r2['job_id'])=='queued' and len(api({'op':'resident_list'}))==1
        assert len(api({'op':'list'}))==1,'Restart replayed accepted assignment'
        release={'job_id':r1['job_id'],'outcome':'finished','resources_cleaned':True,'note':'Controlled fixture is idle; no runtime resources created'}
        command('release','--request-file',request_file('release-first',release))
        wait(lambda:state(r2['job_id'])=='delivered');wait(idle)
        release['job_id']=r2['job_id'];command('release','--request-file',request_file('release-second',release))
        continued=assignment(1,'continue-first',False);r3=json.loads(command('assign','--request-file',request_file('continued',continued)).stdout);assert r3['coder_id']==r1['coder_id'] and r3['version']==2
        wait(lambda:state(r1['job_id'])=='delivered');wait(idle)
        replies=json.loads(command('messages').stdout)['messages'];assert len(replies)==3 and all(m['body']=='DELIVERED_EXPLICIT_REPLY' for m in replies)
        assert len(api({'op':'resident_list'}))==2 and len(api({'op':'list'}))==3
        packet=home/'evidence'/r1['job_id']/'2/packet.json';assert packet.exists()
        release['job_id']=r1['job_id'];release['outcome']='paused';command('release','--request-file',request_file('pause-for-transfer',release))
        transferred=assignment(1,'explicit-replacement',True)
        transferred['replacement']={'expected_coder':r1['coder_id'],'expected_version':2,'reason':'Controlled independent-resume incompatibility','authority':'Explicit synthetic operator approval','handoff':str(packet)}
        r4=json.loads(command('assign','--request-file',request_file('replacement',transferred)).stdout)
        assert r4['version']==3 and r4['coder_id']!=r1['coder_id'] and r4['job_id']==r1['job_id']
        wait(lambda:state(r1['job_id'])=='delivered');wait(idle)
        assert json.loads(command('assign','--request-file',request_file('replacement',transferred)).stdout)['idempotent']
        assert len(api({'op':'resident_list'}))==3 and len(api({'op':'list'}))==4
        assert len(json.loads(command('messages').stdout)['messages'])==4
        assert packet.exists() and (home/'evidence'/r1['job_id']/'1/packet.json').exists()
        # Explicit idle continuation retains the same live reservation without a
        # cleanup assertion, a capacity gap or another resident.
        stop(worker)
        retained=assignment(1,'retained-runtime-continuation',False)
        retained['coder_id']=r4['coder_id']
        retained['continuation']={'expected_version':3,'expected_coder':r4['coder_id'],'reason':'Controlled environment recovered; existing allocation retained'}
        retained_file=request_file('retained-continuation',retained)
        rretained=json.loads(command('assign','--request-file',retained_file).stdout)
        assert rretained['version']==4 and rretained['state']=='reserved' and rretained['coder_id']==r4['coder_id']
        assert json.loads(command('assign','--request-file',retained_file).stdout)['idempotent']
        with database(home/'work-items.sqlite3') as c:
            assert c.execute('select holds_slot from work where id=?',(r1['job_id'],)).fetchone()[0]==1
            assert c.execute('select count(*) from reservations where job=?',(r1['job_id'],)).fetchone()[0]>0
        start_worker();wait(lambda:state(r1['job_id'])=='delivered');wait(idle)
        assert len(api({'op':'resident_list'}))==3 and len(api({'op':'list'}))==5
        # Enable supervision for new assignments only. No old plain messages become outcomes.
        stop(worker)
        release['job_id']=r1['job_id'];command('release','--request-file',request_file('pause-supervised',release))
        observer=digest('supervision observer')
        config['supervision']={'notify_to':{'type':'client','machine':owner,'id':observer},'start_timeout_seconds':10,'inactivity_seconds':10,'report_grace_seconds':1}
        (home/'config.json').write_text(json.dumps(config),encoding='utf-8')
        supervised=assignment(1,'supervised-handoff',False)
        r5=json.loads(command('assign','--request-file',request_file('supervised',supervised)).stdout)
        start_worker();wait(lambda:state(r1['job_id'])=='finished');wait(idle)
        def delivered_conditions():
            return [json.loads(m['body'])['condition'] for m in api({'op':'client_inbox','client_id':observer,'after':0})['messages']]
        wait(lambda:'report_received' in delivered_conditions() and 'allocation_released' in delivered_conditions())
        count=len(api({'op':'list'}));stop(worker);start_worker();time.sleep(2.5)
        assert len(api({'op':'list'}))==count,'Restart reran implementation'
        missing=assignment(1,'supervised-missing-handoff',False)
        missing['assignment']['task']['request']='SIMULATE_MISSING_HANDOFF'
        r6=json.loads(command('assign','--request-file',request_file('missing-handoff',missing)).stdout)
        wait(lambda:state(r1['job_id'])=='finished');wait(idle)
        assert len(api({'op':'list'}))==count+2,'Missing handoff did not use exactly one report-only followup'
        notices=api({'op':'client_inbox','client_id':observer,'after':0})['messages']
        assert sum(json.loads(m['body'])['condition']=='missing_report' for m in notices)==1
        with database(home/'work-items.sqlite3') as c:
            assert c.execute("select count(*) from supervision_outbox where purpose='report_request'").fetchone()[0]==1
            assert c.execute("select count(*) from report_receipts where accepted=1").fetchone()[0]==2
        with database(home/'work-items.sqlite3') as c:assert c.execute('pragma integrity_check').fetchone()[0]=='ok'
        stop(worker)
        command('supervise','--test-notification','controlled-route-proof')
        command('supervise','--test-notification','controlled-route-proof')
        notices=api({'op':'client_inbox','client_id':observer,'after':0})['messages']
        assert sum(json.loads(m['body'])['condition']=='delivery_test' for m in notices)==1
        # Real retained request/control round trip with a controlled backend. No browser starts.
        import base64,shlex
        browser=root/'msedge.exe';browser.write_text('fixture only')
        shell=root/'powershell.exe';shell.write_text('fixture only')
        profile=root/'browser-profile';profile.mkdir()
        policy={'enabled':True,'rule_id':'assigned-browser','revision':1,'authority':'Explicit synthetic fixture authority','browser_executable':str(browser),'shells':[str(shell)]}
        (home/'approval-policy.json').write_text(json.dumps(policy),encoding='utf-8')
        browser_input=assignment(1,'browser-approval-roundtrip',False)
        script=f"Start-Process -FilePath '{browser}' -ArgumentList '--user-data-dir=\"{profile}\"','--no-first-run','http://localhost:4500/?clientId=100' -WindowStyle Normal"
        wire=shlex.join([str(shell),'-Command',script])
        marker=base64.b64encode(json.dumps({'command':wire,'cwd':browser_input['assignment']['worktree']}).encode()).decode()
        browser_input['assignment']['task']['request']='BOXEN_BROWSER_APPROVAL_FIXTURE:'+marker
        browser_input['assignment']['runtime']={'required':True,'reservation_id':'fixture-browser','allocation_source':'Controlled fixture allocation','frontend_port':4500,'backend_port':4501,'database_server':'fixture','database_name':'synthetic','browser_context':str(profile),'synthetic_tenant_id':100}
        command('assign','--request-file',request_file('browser-approval',browser_input))
        start_worker()
        wait(lambda:any(d['state']=='submitted' for d in json.loads(command('approvals').stdout)['decisions']))
        wait(idle)
        decisions=json.loads(command('approvals').stdout)['decisions'];assert len(decisions)==1,decisions
        runs=api({'op':'list'});run=next(r for r in runs if r['record']['spec']['id']==decisions[0]['run'])
        events=[json.loads(x) for x in (boxen_home/'runs'/run['record']['spec']['id']/'events.jsonl').read_text().splitlines()]
        answered=[e for e in events if e['kind']=='request_answered' and e['data']['request_id']=='approval-1'];assert len(answered)==1,answered
        stop(worker);start_worker();time.sleep(2.5);assert len(json.loads(command('approvals').stdout)['decisions'])==1
        assert len(api({'op':'list'}))==len(runs),'Approval recovery replayed inference'
        print('PASS assigned-browser one-time approval through real Boxen retained requests/control, restart without approval or inference replay')
        print('PASS real Boxen create/send/activation/reply, durable migration, exact retries, restart without replay, capacity queue/release, same-coder continuation with retained reservations, explicit owner transfer, immutable packets, supervised handoffs, one report-only recovery, durable notifications and private final isolation')
    finally:
        stop(worker);stop(daemon)
        if daemon:daemon.stdout.close()
