#!/usr/bin/env python3
"""Paired real daemons for native client tests or a disposable interactive workspace.

--interactive leaves them up until Ctrl-C; the printed launch command contains no capability.
"""
import argparse
from smoke_helpers import fake_codex
import json
import os
import pathlib
import secrets
import socket
import subprocess
import tempfile
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
            process=subprocess.Popen(command+['--data-dir',str(homes[i]),'serve','--listen',f'127.0.0.1:{ports[i]}','--ui',f'127.0.0.1:{controls[i]}','--codex',str(fake_codex())],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True)
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
                assert pathlib.Path(accepted['record']['spec']['cwd'])==expected
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
        host_operation({'op':'resident_create','spec':{'id':historical,'name':'Historical correspondence fixture','cwd':temporary,'auto_activate':False}})
        historical_mail=host_operation({'op':'mailbox_send','client_id':keys[1],'message':{'id':secrets.token_hex(32),'to':{'type':'resident','id':historical},'subject':'Earlier human message','body':'REPLY historical human message'}})
        historical_run=host_operation({'op':'resident_activate','id':historical,'activation_id':secrets.token_hex(32)})
        deadline=time.monotonic()+20
        while host_operation({'op':'get','id':historical_run['record']['spec']['id']})['status'] not in ['completed','failed','interrupted']:
            assert time.monotonic()<deadline;time.sleep(.05)
        host_operation({'op':'mailbox_send','client_id':secrets.token_hex(32),'message':{'id':secrets.token_hex(32),'to':{'type':'resident','id':historical},'subject':'Automation message','body':'AUTOMATION_NOT_HUMAN'}})
        env=os.environ|{'BOXEN_GUI_TEST_HOME':str(homes[0]),'BOXEN_GUI_TEST_URL':f'http://127.0.0.1:{controls[0]}','BOXEN_GUI_TEST_DEST':json.dumps(destination),'BOXEN_GUI_TEST_HISTORY_AGENT':historical,'BOXEN_GUI_TEST_HISTORY_MAIL':historical_mail['id'],'BOXEN_GUI_TEST_CLASS':definition,'BOXEN_GUI_TEST_ACTIVE_AGENT':instances[2]}
        subprocess.run(['cargo','test','--locked','--manifest-path',str(ROOT/'desktop/Cargo.toml'),'real_daemon_boundary','--','--ignored','--nocapture'],env=env,check=True)
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
