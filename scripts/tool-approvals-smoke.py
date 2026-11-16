#!/usr/bin/env python3
"""Exercise live computer/browser approval settings over the authenticated peer path."""
import json
import pathlib
import secrets
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request

from smoke_helpers import fake_codex, PipeLines

ROOT = pathlib.Path(__file__).resolve().parent.parent
binary = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else ROOT / 'target/release/boxen').resolve()
command = (['/bin/bash'] if binary.suffix == '.com' else []) + [str(binary)]


def port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


with tempfile.TemporaryDirectory(prefix='boxen-tool-approvals-') as directory:
    homes = [pathlib.Path(directory) / str(index) for index in range(2)]
    ports = [port(), port()]
    controls = [port(), port()]
    keys = []
    processes = []
    tokens = []
    for home in homes:
        keys.append(subprocess.check_output(command + ['--data-dir', str(home), 'identity'], text=True).strip())
    for index, home in enumerate(homes):
        subprocess.run(command + ['--data-dir', str(home), 'trust', keys[1-index], 'Fixture'], check=True, stdout=subprocess.DEVNULL)

    def launch(index):
        process = subprocess.Popen(command + ['--data-dir', str(homes[index]), 'serve',
            '--listen', f'127.0.0.1:{ports[index]}', '--ui', f'127.0.0.1:{controls[index]}',
            '--codex', str(fake_codex())], stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        lines = PipeLines(process.stdout)
        startup = [lines.get() for _ in range(3)]
        assert 'Local controls:' in startup[-1], startup
        return process, startup[-1].strip().split('#')[1]

    def operation(request):
        body = {'peer': keys[1], 'address': f'127.0.0.1:{ports[1]}', 'request': request}
        req = urllib.request.Request(f'http://127.0.0.1:{controls[0]}/api/operation',
            headers={'Authorization': 'Bearer ' + tokens[0], 'Content-Type': 'application/json'},
            data=json.dumps(body).encode())
        with urllib.request.urlopen(req, timeout=8) as response:
            return json.load(response)

    def settings(**updates):
        return operation({'op': 'tool_approvals', 'args': {'action': 'set' if updates else 'get', **updates}})

    def start(cases):
        ident = secrets.token_hex(32)
        spec = {'id': ident, 'thread_id': ident, 'backend': 'codex', 'prompt': 'TOOL_APPROVALS:' + json.dumps(cases)}
        operation({'op': 'start', 'spec': spec})
        return ident

    def wait(ident, state):
        deadline = time.monotonic() + 12
        while time.monotonic() < deadline:
            snapshot = operation({'op': 'get', 'id': ident})
            if snapshot['status'] == state:
                return snapshot
            assert snapshot['status'] not in {'failed', 'cancelled', 'interrupted'}, snapshot
            time.sleep(.04)
        raise AssertionError((ident, state, snapshot))

    def respond(ident, request_id, action='accept'):
        operation({'op': 'control', 'id': ident, 'action': {'type': 'respond', 'request_id': request_id,
            'result': {'action': action, 'content': {} if action == 'accept' else None}}})

    def events(ident):
        return [json.loads(line) for line in (homes[1] / 'runs' / ident / 'events.jsonl').read_text().splitlines()]

    try:
        for index in range(2):
            process, token = launch(index)
            processes.append(process)
            tokens.append(token)
        assert settings() == {'computer_use': True, 'browser_use': True}
        default = start([{'connector': 'computer-use'}, {'connector': 'browser-use'}])
        wait(default, 'completed')
        journal = events(default)
        answers = [event['data'] for event in journal if event['kind'] == 'request_answered']
        assert len(answers) == 2 and all(answer['automatic'] for answer in answers)
        assert not any(event['kind'] == 'status' and event['data']['status'] == 'waiting' for event in journal)

        assert settings(computer_use=False) == {'computer_use': False, 'browser_use': True}
        computer = start([{'connector': 'computer-use'}, {'connector': 'browser-use'}])
        wait(computer, 'waiting')
        respond(computer, 0)
        wait(computer, 'completed')
        answers = [event['data'] for event in events(computer) if event['kind'] == 'request_answered']
        assert 'automatic' not in answers[0] and answers[1]['category'] == 'browser_use'

        settings(computer_use=True, browser_use=False)
        live = start([{'connector': 'browser-use', 'action': 'decline'},
                      {'connector': 'computer-use'}, {'connector': 'browser-use'}])
        wait(live, 'waiting')
        settings(computer_use=False, browser_use=True)
        respond(live, 0, 'decline')
        deadline = time.monotonic() + 10
        while not any(event['kind'] == 'backend' and event['data'].get('id') == 'tool-permission-1' for event in events(live)):
            assert time.monotonic() < deadline
            time.sleep(.04)
        wait(live, 'waiting')
        respond(live, 'tool-permission-1')
        wait(live, 'completed')
        answers = [event['data'] for event in events(live) if event['kind'] == 'request_answered']
        assert len(answers) == 3 and answers[-1]['automatic'] and 'automatic' not in answers[1]

        settings(computer_use=True, browser_use=True)
        other = start([{'connector': 'unrelated'}, {'connector': 'browser-use', 'kind': 'browser_auth'}])
        wait(other, 'waiting')
        respond(other, 0)
        deadline = time.monotonic() + 10
        while not any(event['kind'] == 'backend' and event['data'].get('id') == 'tool-permission-1' for event in events(other)):
            assert time.monotonic() < deadline
            time.sleep(.04)
        wait(other, 'waiting')
        respond(other, 'tool-permission-1')
        wait(other, 'completed')
        assert not any(event['kind'] == 'tool_approval_decision' for event in events(other))

        settings(browser_use=False)
        processes[1].terminate()
        processes[1].wait(timeout=8)
        processes[1], tokens[1] = launch(1)
        assert settings() == {'computer_use': True, 'browser_use': False}
        persisted = start([{'connector': 'browser-use'}])
        wait(persisted, 'waiting')
        respond(persisted, 0)
        wait(persisted, 'completed')

        (homes[1] / 'tool-approvals.json').write_text('{invalid')
        malformed = start([{'connector': 'computer-use'}])
        wait(malformed, 'waiting')
        assert any(event['kind'] == 'tool_approval_error' for event in events(malformed))
        respond(malformed, 0)
        wait(malformed, 'completed')
        (homes[1] / 'tool-approvals.json').write_text('{"computer_use":true,"browser_use":true}')
        recovered = start([{'connector': 'computer-use'}])
        wait(recovered, 'completed')
        print('Default-on CU/browser approval, independent toggles, live updates, manual responses, replay, restart and invalid-setting recovery passed')
    finally:
        for process in processes:
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=8)
