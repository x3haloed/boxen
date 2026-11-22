#!/usr/bin/env python3
"""Paired real daemons for native client tests or a disposable interactive workspace.

--interactive leaves them up until Ctrl-C; the printed launch command contains no capability.
"""
import argparse
from smoke_helpers import fake_codex, python_launcher
import http.server
import json
import os
import pathlib
import secrets
import socket
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser()
parser.add_argument('--binary', type=pathlib.Path, default=ROOT/'target/release/boxen')
parser.add_argument('--interactive', action='store_true')
args = parser.parse_args()
binary = args.binary.resolve()
command = (['/bin/bash'] if binary.suffix == '.com' else []) + [str(binary)]

def port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]

with tempfile.TemporaryDirectory(prefix='boxen-desktop-') as temporary:
    directory = pathlib.Path(temporary)
    homes = [directory/'a', directory/'b']
    ports, controls = [port(), port()], [port(), port()]
    processes = []
    def cli(i, *parts):
        return subprocess.check_output(command+['--data-dir',str(homes[i]),*parts],text=True).strip()
    keys = [cli(i,'identity') for i in range(2)]
    for i in range(2):
        cli(i,'trust',keys[1-i], 'Remote fixture' if i == 0 else 'Desktop fixture')
    try:
        tokens=[]
        for i in range(2):
            runtime_flags = ['--codex', str(fake_codex())]
            runtime_flags += ['--claude', str(python_launcher('fake-claude.py')) if i == 1 else '']
            if i == 1:
                runtime_flags += ['--antigravity', str(python_launcher('fake-antigravity.py'))]
            env = {key: value for key, value in os.environ.items() if key != 'BOXEN_ANTIGRAVITY'}
            env['BOXEN_FIXTURE_MODEL_MACHINE'] = ('local', 'remote')[i]
            process=subprocess.Popen(command+['--data-dir',str(homes[i]),'serve','--listen',f'127.0.0.1:{ports[i]}','--ui',f'127.0.0.1:{controls[i]}',*runtime_flags],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True,env=env)
            processes.append(process)
            lines=[process.stdout.readline() for _ in range(3)]
            assert 'Local controls:' in lines[-1]
            tokens.append(lines[-1].strip().split('#')[1])
        destination={'key':keys[1],'label':'Remote fixture','address':f'127.0.0.1:{ports[1]}'}
        def operation(request):
            req=urllib.request.Request(f'http://127.0.0.1:{controls[0]}/api/operation',headers={'Authorization':'Bearer '+tokens[0],'Content-Type':'application/json'},data=json.dumps({'peer':keys[1],'address':destination['address'],'request':request}).encode())
            return json.load(urllib.request.urlopen(req,timeout=12))
        for prompt in ['Alpha conversation','Bravo conversation','Charlie conversation']:
            ident=secrets.token_hex(32)
            spec={'id':ident,'thread_id':ident,'backend':'codex','model':'fixture','prompt':prompt}
            if prompt != 'Alpha conversation':
                spec['cwd']=temporary
            accepted=operation({'op':'start','spec':spec})
            if 'cwd' not in spec:
                expected=homes[1].resolve()/'threads'/ident
                assert accepted['record']['automatic_cwd'] and expected.is_dir()
                assert pathlib.Path(accepted['record']['spec']['cwd']).samefile(expected)
                assert not (homes[0]/'threads'/ident).exists()
                deadline=time.monotonic()+10
                while (snapshot:=operation({'op':'get','id':ident}))['status']!='completed':
                    assert snapshot['status'] in {'starting','running','waiting'} and time.monotonic()<deadline, snapshot['status']
                    time.sleep(.05)
                retry=operation({'op':'start','spec':spec})
                assert retry['record']==accepted['record'] and retry['status']=='completed'
                try:
                    operation({'op':'start','spec':dict(spec,cwd=str(expected))})
                except urllib.error.HTTPError as error:
                    assert 'different input' in error.read().decode()
                else:
                    raise AssertionError('An explicit directory changed the accepted request')
        resident=secrets.token_hex(32)
        operation({'op':'resident_create','spec':{'id':resident,'name':'Not an anonymous thread','cwd':temporary,'auto_activate':False}})
        operation({'op':'resident_activate','id':resident,'activation_id':secrets.token_hex(32)})
        definition=secrets.token_hex(32)
        operation({'op':'resident_definition','args':{'action':'create','spec':{'id':definition,'name':'Fixture workers','cwd':temporary,'model':'fixture','auto_activate':False}}})
        instances=[]
        for name,overrides in [('Idle worker',{}),('Blocked worker',{'backend':'openrouter'}),('Active worker',{})]:
            ident=secrets.token_hex(32);instances.append(ident)
            operation({'op':'resident_definition','args':{'action':'instantiate','spec':{'id':ident,'definition_id':definition,'definition_revision':1,'overrides':dict(overrides,name=name)}}})
        blocked=operation({'op':'resident_activate','id':instances[1],'activation_id':secrets.token_hex(32)})
        deadline=time.monotonic()+20
        while operation({'op':'get','id':blocked['record']['spec']['id']})['status'] not in ['interrupted','failed']:
            assert time.monotonic()<deadline;time.sleep(.05)
        # Retained correspondence predates attachment from a different machine.
        historical=secrets.token_hex(32)
        def host_operation(request):
            req=urllib.request.Request(f'http://127.0.0.1:{controls[1]}/api/operation',headers={'Authorization':'Bearer '+tokens[1],'Content-Type':'application/json'},data=json.dumps({'peer':keys[1],'address':'','request':request}).encode())
            return json.load(urllib.request.urlopen(req,timeout=12))
        # A destination-local decline wins a race with a remote approval.
        gate_run = secrets.token_hex(32)
        operation({'op':'start','spec':{'id':gate_run,'thread_id':gate_run,'backend':'codex',
                                        'prompt':'ATTRIBUTED_GATE_FIXTURE'}})
        deadline = time.monotonic() + 10
        while operation({'op':'get','id':gate_run})['status'] != 'waiting':
            assert time.monotonic() < deadline
            time.sleep(.025)
        winner, contender = secrets.token_hex(32), secrets.token_hex(32)
        decision = {'op':'respond_attempt','id':gate_run,'request_id':0}
        assert host_operation(decision|{'attempt_id':winner,'result':{'decision':'decline'}})['accepted']
        assert operation(decision|{'attempt_id':contender,'result':{'decision':'accept'}})['accepted']
        req = urllib.request.Request(f'http://127.0.0.1:{controls[0]}/api/watch',
            headers={'Authorization':'Bearer '+tokens[0],'Content-Type':'application/json'},
            data=json.dumps({'peer':keys[1],'address':destination['address'],
                             'request':{'id':gate_run,'after':0}}).encode())
        events = []
        with urllib.request.urlopen(req, timeout=12) as stream:
            for line in stream:
                batch = json.loads(line)
                events.extend(batch['events'])
                if batch['done']:
                    break
        attempts = {event['data']['attempt_id']:event['data'] for event in events if event['kind']=='response_attempt'}
        assert attempts[winner]['machine_id']==keys[1] and attempts[winner]['decision']=='decline'
        assert attempts[contender]['machine_id']==keys[0] and attempts[contender]['decision']=='accept'
        writes = [event for event in events if event['kind']=='response_write_started']
        answers = [event for event in events if event['kind']=='request_answered']
        assert len(writes)==len(answers)==1 and writes[0]['seq'] < answers[0]['seq']
        assert writes[0]['data']['attempt_id']==answers[0]['data']['attempt_id']==winner
        assert any(event['kind']=='response_refused' and event['data']['attempt_id']==contender for event in events)
        host_operation({'op':'resident_create','spec':{'id':historical,'name':'Historical correspondence fixture','cwd':temporary,'auto_activate':False}})
        historical_mail=host_operation({'op':'mailbox_send','client_id':keys[1],'message':{'id':secrets.token_hex(32),'to':{'type':'resident','id':historical},'subject':'Earlier human message','body':'REPLY historical human message'}})
        historical_run=host_operation({'op':'resident_activate','id':historical,'activation_id':secrets.token_hex(32)})
        deadline=time.monotonic()+20
        while host_operation({'op':'get','id':historical_run['record']['spec']['id']})['status'] not in ['completed','failed','interrupted']:
            assert time.monotonic()<deadline;time.sleep(.05)
        host_operation({'op':'mailbox_send','client_id':secrets.token_hex(32),'message':{'id':secrets.token_hex(32),'to':{'type':'resident','id':historical},'subject':'Automation message','body':'AUTOMATION_NOT_HUMAN'}})
        env=os.environ|{'BOXEN_GUI_TEST_HOME':str(homes[0]),'BOXEN_GUI_TEST_URL':f'http://127.0.0.1:{controls[0]}','BOXEN_GUI_TEST_DEST':json.dumps(destination),'BOXEN_GUI_TEST_HISTORY_AGENT':historical,'BOXEN_GUI_TEST_HISTORY_MAIL':historical_mail['id'],'BOXEN_GUI_TEST_CLASS':definition,'BOXEN_GUI_TEST_ACTIVE_AGENT':instances[2]}
        repository = directory / 'fixture-repository'
        repository.mkdir()
        def git(*parts):
            return subprocess.check_output(['git', '-C', str(repository), *parts], text=True)
        git('init', '--quiet')
        git('symbolic-ref', 'HEAD', 'refs/heads/fixture-main')
        (repository / 'fixture.txt').write_text('Fixture content\n')
        git('add', 'fixture.txt')
        git('-c', 'user.name=Fixture account', '-c', 'user.email=fixture@example.invalid',
            'commit', '--quiet', '-m', 'Fixture commit')
        inspector_run = secrets.token_hex(32)
        operation({'op': 'start', 'spec': {'id': inspector_run, 'thread_id': inspector_run,
                  'backend': 'codex', 'cwd': str(repository), 'model': 'fixture',
                  'prompt': 'GRID_INSPECTOR_RECEIPT'}})
        query = {'op': 'repo_probe', 'id': inspector_run}
        clean = operation(query)
        assert clean['branch'] == 'fixture-main' and clean['head'] == git('rev-parse', 'HEAD').strip()
        assert clean['dirty'] is False and clean['run_id'] == inspector_run and clean['at'] > 0
        (repository / 'untracked.txt').write_text('Untracked fixture\n')
        assert operation(query)['dirty'] is True
        env['BOXEN_GUI_TEST_INSPECTOR_RUN'] = inspector_run
        env['BOXEN_GUI_TEST_RESPONSE_RACE'] = json.dumps({'run': gate_run, 'local_attempt': contender})
        dropped = []

        class DropResponseAcknowledgment(http.server.BaseHTTPRequestHandler):
            def do_POST(self):
                message = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
                request = message['request']
                response = request['op'] == 'respond_attempt'
                release = request['op'] == 'held_followup' and request.get('action') == 'release'
                if (self.path != '/api/operation' or message['peer'] != keys[1]
                        or not (response or release)):
                    self.send_error(400)
                    return
                # Relay the attempt to the real paired daemon and receive its acknowledgment.
                # Close the client connection before writing any HTTP response.
                result = operation(request)
                if response:
                    assert result['accepted']
                else:
                    assert result['hold']['phase'] == 'released'
                    assert result['hold']['revision'] == request['expected_revision'] + 1
                dropped.append(request['op'])
                self.close_connection = True

            def log_message(self, *_args):
                pass

        with http.server.HTTPServer(('127.0.0.1', 0), DropResponseAcknowledgment) as proxy:
            thread = threading.Thread(target=proxy.serve_forever)
            thread.start()
            try:
                env['BOXEN_GUI_TEST_DROP_RESPONSE_URL'] = f'http://127.0.0.1:{proxy.server_port}'
                subprocess.run(['cargo','test','--locked','--manifest-path',str(ROOT/'desktop/Cargo.toml'),'real_daemon_boundary','--','--ignored','--nocapture'],env=env,check=True)
                assert sorted(dropped) == ['held_followup', 'respond_attempt'], dropped
            finally:
                proxy.shutdown()
                thread.join()
        if args.interactive:
            # Seed the human inbox through real resident tools, never fabricated journal rows.
            for name,subject,body in [('Resident demo','Project review','The review is ready. There are two questions worth discussing.'),('Worker instance','Workload status','The requested workload has finished. Its execution record remains available.')]:
                agent=secrets.token_hex(32)
                operation({'op':'resident_create','spec':{'id':agent,'name':name,'cwd':temporary,'auto_activate':False}})
                call={'action':'send','to':{'instance':'boxen','conversation':f'client:{keys[0]}:{keys[0]}'},'subject':subject,'body':body}
                operation({'op':'mailbox_send','client_id':keys[0],'message':{'id':secrets.token_hex(32),'to':{'type':'resident','id':agent},'subject':'Fixture correspondence','body':'MESSAGING:'+json.dumps(call)}})
                accepted=operation({'op':'resident_activate','id':agent,'activation_id':secrets.token_hex(32)})
                end=time.monotonic()+15
                while True:
                    status=operation({'op':'get','id':accepted['record']['spec']['id']})['status']
                    if status in ('completed','failed','interrupted','cancelled'):assert status=='completed';break
                    assert time.monotonic()<end;time.sleep(.05)
            metadata={'home':str(homes[0]),'url':env['BOXEN_GUI_TEST_URL'],'peer':keys[1],'address':destination['address'],'remote_home':str(homes[1])}
            metadata['response_race']={'id':contender,'run_tag':f'{keys[1]}:{gate_run}','request':0}
            (ROOT/'work/desktop-fixture.json').write_text(json.dumps(metadata))
            print('Interactive fixture ready: work/desktop-fixture.json (no capability). Ctrl-C cleans up.',flush=True)
            while True:
                time.sleep(1)
    finally:
        for process in processes:
            process.terminate()
        for process in processes:
            try:process.wait(timeout=5)
            except subprocess.TimeoutExpired:process.kill();process.wait()
