#!/usr/bin/env python3
"""Paired real daemons for native client tests or a disposable interactive workspace.

--interactive leaves them up until Ctrl-C; the printed launch command contains no capability.
"""
import argparse
import json
import os
import pathlib
import secrets
import socket
import subprocess
import tempfile
import time
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
            process=subprocess.Popen(command+['--data-dir',str(homes[i]),'serve','--listen',f'127.0.0.1:{ports[i]}','--ui',f'127.0.0.1:{controls[i]}','--codex',str(ROOT/'tests/fake-codex.py')],stdout=subprocess.PIPE,stderr=subprocess.DEVNULL,text=True)
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
            operation({'op':'start','spec':{'id':ident,'thread_id':ident,'backend':'codex','cwd':temporary,'model':'fixture','prompt':prompt}})
        resident=secrets.token_hex(32)
        operation({'op':'resident_create','spec':{'id':resident,'name':'Not an anonymous thread','cwd':temporary,'auto_activate':False}})
        operation({'op':'resident_activate','id':resident,'activation_id':secrets.token_hex(32)})
        env=os.environ|{'BOXEN_GUI_TEST_HOME':str(homes[0]),'BOXEN_GUI_TEST_URL':f'http://127.0.0.1:{controls[0]}','BOXEN_GUI_TEST_DEST':json.dumps(destination)}
        subprocess.run(['cargo','test','--locked','--manifest-path',str(ROOT/'desktop/Cargo.toml'),'real_daemon_boundary','--','--ignored','--nocapture'],env=env,check=True)
        if args.interactive:
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
