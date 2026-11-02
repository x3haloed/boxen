#!/usr/bin/env python3
"""Opt-in live desktop queue proof, using an isolated Boxen client (no model inference).

The thread receives a transport notice. Its scoped reply command saves a reply
while this test daemon is offline; the runner then restarts it and verifies mail.
Evidence and transport state remain in the explicitly supplied directory.
"""
import argparse
import hashlib
import json
import os
import pathlib
import secrets
import socket
import sqlite3
import subprocess
import time
import urllib.request

p = argparse.ArgumentParser(description=__doc__)
p.add_argument('--boxen', required=True, type=pathlib.Path)
p.add_argument('--bridge', required=True, type=pathlib.Path)
p.add_argument('--codex', required=True, type=pathlib.Path)
p.add_argument('--thread', required=True)
p.add_argument('--directory', required=True, type=pathlib.Path)
a = p.parse_args()
root = a.directory.resolve()
root.mkdir(parents=True, exist_ok=False)
home = root / 'boxen'
config = root / 'profile' / 'config.json'
env = {**os.environ, 'CODEX_THREAD_ID': a.thread}

def port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]

peer_port, ui = port(), port()
server = None

def start():
    global server, owner, token
    server = subprocess.Popen(
        [str(a.boxen.resolve()), '--data-dir', str(home), 'serve',
         '--listen', f'127.0.0.1:{peer_port}', '--ui', f'127.0.0.1:{ui}',
         '--codex', str(a.codex.resolve())],
        stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
        creationflags=subprocess.CREATE_NO_WINDOW if os.name == 'nt' else 0)
    lines = [server.stdout.readline().strip() for _ in range(3)]
    assert 'Local controls:' in lines[-1], 'test daemon did not start'
    owner = lines[0].split(': ', 1)[1]
    token = lines[-1].split('#')[1]

def stop():
    if server and server.poll() is None:
        server.terminate()
        server.wait(timeout=10)
    if server:
        server.stdout.close()

def operation(request):
    r = urllib.request.Request(f'http://127.0.0.1:{ui}/api/operation',
        data=json.dumps({'peer': owner, 'address': '', 'request': request}).encode(),
        headers={'Authorization': 'Bearer ' + token, 'Content-Type': 'application/json'})
    with urllib.request.urlopen(r, timeout=15) as response:
        return json.load(response)

def command(*parts):
    result = subprocess.run([str(a.bridge.resolve()), '--config', str(config), *parts],
        capture_output=True, text=True, env=env, timeout=45)
    assert result.returncode == 0, result.stderr
    return result.stdout

try:
    start()
    command('init', a.thread, '--codex', str(a.codex.resolve()),
            '--boxen-home', str(home), '--url', f'http://127.0.0.1:{ui}')
    client = json.loads(config.read_text())['client_id']
    probe = secrets.token_hex(32)
    marker = secrets.token_hex(12)
    original = operation({'op': 'mailbox_send', 'client_id': probe, 'message': {
        'id': hashlib.sha256(marker.encode()).hexdigest(),
        'to': {'type': 'client', 'machine': owner, 'id': client},
        'subject': 'Authorized support bridge transport test',
        'body': f'Test client input. An explicit reply containing {marker} verifies the return path. No production action or model execution is requested.'}})
    command('run', '--once')
    stop()
    db_path = config.parent / 'bridge.sqlite3'
    with sqlite3.connect(db_path) as db:
        delivery = db.execute('select id from inbound').fetchone()[0]
        assert db.execute("select count(*) from notices where state='accepted'").fetchone()[0] == 1
    ready = {'config': str(config), 'delivery_id': delivery, 'marker': marker,
             'desktop_queue_accepted': True, 'boxen_offline': True}
    (root / 'ready.json').write_text(json.dumps(ready, indent=2))
    print(json.dumps(ready), flush=True)
    deadline = time.monotonic() + 240
    saved = None
    while time.monotonic() < deadline:
        with sqlite3.connect(db_path) as db:
            saved = db.execute("select id,state from replies where state='saved_locally'").fetchone()
        if saved:
            break
        time.sleep(1)
    assert saved, 'No scoped offline reply arrived during the live test'
    start()
    command('run', '--once')
    command('run', '--once')
    mail = operation({'op': 'client_inbox', 'client_id': probe})['messages']
    assert len(mail) == 1 and marker in mail[0]['body']
    assert mail[0]['in_reply_to'] == original['id']
    assert mail[0]['sender'] == {'type': 'client', 'machine': owner, 'id': client}
    status = json.loads(command('--rpc', 'reply_status', json.dumps({'id': saved[0]})))
    assert status['state'] == 'boxen_accepted'
    result = {**ready, 'offline_reply_saved': True, 'boxen_reply_accepted': True,
              'accepted_reply_count': len(mail), 'source': 'authenticated test client; no model inference'}
    (root / 'result.json').write_text(json.dumps(result, indent=2))
    print(json.dumps(result), flush=True)
finally:
    stop()
