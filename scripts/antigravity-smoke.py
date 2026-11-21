#!/usr/bin/env python3
"""Antigravity ACP contract through two authenticated daemons and the ordinary CLI."""
import json
import pathlib
import secrets
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.request

from smoke_helpers import PipeLines, python_launcher

root = pathlib.Path(__file__).resolve().parent.parent
binary = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else root / 'target/release/boxen').resolve()
command = (['/bin/bash'] if binary.suffix == '.com' else []) + [str(binary)]
fixture = python_launcher('fake-antigravity.py')


def port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


def uid():
    return secrets.token_hex(32)


with tempfile.TemporaryDirectory(prefix='boxen-acp-smoke-') as temp:
    path = pathlib.Path(temp)
    paths = [path / 'a', path / 'b']
    workspaces = [path / name for name in ('anon', 'resident')]
    for workspace in workspaces:
        workspace.mkdir()
    peer_ports = [port(), port()]
    interface_ports = [port(), port()]
    processes, tokens = [], []
    logs = [(path / f'daemon-{index}.stderr').open('w+') for index in range(2)]

    def cli(index, *args):
        return subprocess.check_output(command + ['--data-dir', str(paths[index]), *args], text=True, timeout=15).strip()

    keys = [cli(i, 'identity') for i in range(2)]
    dest = [f'127.0.0.1:{p}' for p in peer_ports]
    remote = [dest[1], keys[1]]

    def api(request, watch=False, local=False):
        target = {'peer': keys[0], 'address': ''} if local else {'peer': keys[1], 'address': dest[1]}
        req = urllib.request.Request(
            f'http://127.0.0.1:{interface_ports[0]}/api/' + ('watch' if watch else 'operation'),
            data=json.dumps({**target, 'request': request}).encode(),
            headers={'Authorization': 'Bearer ' + tokens[0], 'Content-Type': 'application/json'})
        response = urllib.request.urlopen(req, timeout=10)
        if watch:
            return response
        with response:
            return json.load(response)

    def collect(run_id, after=0, kind=None):
        events = []
        deadline = time.monotonic() + 12
        with api({'id': run_id, 'after': after}, watch=True) as stream:
            for line in stream:
                batch = json.loads(line)
                assert 'error' not in batch, batch
                events.extend(batch['events'])
                if batch['done'] or any(event['kind'] == kind for event in events):
                    return events, batch['snapshot']
                assert time.monotonic() < deadline, (kind, events)
        raise AssertionError('The watch closed before the requested observation')

    def start(prompt, thread=None, ident=None, model='fixture-model', effort=None, require=None):
        args = ['start', *remote, str(workspaces[0]), model, prompt, '--backend', 'antigravity', '--id', ident or uid()]
        if thread:
            args += ['--thread', thread]
        if effort:
            args += ['--reasoning-effort', effort]
        if require:
            args += ['--require', require]
        return json.loads(cli(0, *args))['record']['spec']

    def rejected(request, expected):
        try:
            api(request)
        except urllib.error.HTTPError as error:
            assert error.code == 422 and expected in error.read().decode()
        else:
            raise AssertionError('The daemon accepted an unsupported request')

    try:
        for i in range(2):
            cli(i, 'trust', keys[1-i], 'Paired fixture', '--address', dest[1-i])
            args = ['--antigravity', str(fixture)] if i else []
            process = subprocess.Popen(
                command + ['--data-dir', str(paths[i]), 'serve', '--listen', dest[i],
                           '--ui', f'127.0.0.1:{interface_ports[i]}', *args],
                stdout=subprocess.PIPE, stderr=logs[i], text=True)
            processes.append(process)
            lines = PipeLines(process.stdout)
            for _ in range(3):
                line = lines.get()
            assert 'Local controls:' in line
            tokens.append(line.strip().split('#')[1])

        runtime = lambda **kw: next(b for b in api({'op': 'runtime_info'}, **kw)['backends'] if b['id'] == 'antigravity')
        assert runtime()['configured'] and not runtime(local=True)['configured']
        request_id = uid()
        assert 'late_activity' in runtime()['request_capabilities']
        assert 'live_steering' not in runtime()['activation_capabilities']
        probe = {'id': request_id, 'thread_id': request_id, 'backend': 'openrouter',
                 'cwd': str(workspaces[0]), 'prompt': 'No execution', 'required_capabilities': ['live_steering']}
        rejected({'op': 'start_checked', 'spec': probe}, 'live_steering')
        rejected({'op': 'start', 'spec': probe}, 'start_checked')
        rejected({'op': 'get', 'id': request_id}, 'not found')
        first = start('LATE_WAKE', ident=request_id, effort='high', require='late_activity,input_responses')
        assert set(first['required_capabilities']) == {'late_activity', 'input_responses'}
        assert start('LATE_WAKE', ident=request_id, effort='high', require='input_responses,late_activity') == first
        rejected({'op': 'start_checked', 'spec': {**first, 'required_capabilities': ['late_activity']}}, 'different input')
        early, running = collect(first['id'], kind='input_requested')
        assert running['status'] in ('running', 'waiting'), [(event['kind'], event['data'].get('detail')) for event in early]
        assert any(e['kind'] == 'reasoning_delta' for e in early)
        assert any(e['kind'] == 'backend_prompt_ended' and e['data']['pending_tools'] == 1 for e in early)
        assert not api({'op': 'thread_result', 'id': first['id']})['done']
        choose = ['respond', *remote, first['id'], '"wake-question"']
        cli(0, *choose, '{"option_id":"invalid"}')
        errors, observed = collect(first['id'], early[-1]['seq'], kind='control_error')
        assert observed['status'] == 'waiting'
        cli(0, *choose, '{"option_id":"second"}')
        late, done = collect(first['id'], errors[-1]['seq'])
        all_events = early + errors + late
        assert done['status'] == 'completed'
        assert [e['seq'] for e in all_events] == list(range(1, done['last_seq'] + 1))
        assert api({'op': 'thread_result', 'id': first['id']})['response'] == 'Late answer after the scheduled wake.'

        follow = start('follow-up', first['thread_id'])
        events, resumed = collect(follow['id'])
        assert resumed['status'] == 'completed' and resumed['backend_thread'] == done['backend_thread']
        assert any(e['kind'] == 'tool_result' and e['data']['output_kind'] == 'aggregate' for e in events)
        assert not any(e['kind'] == 'tool_output' for e in events)
        (workspaces[0] / '.native-acp-session').unlink()
        rebuilt = start('RECONSTRUCT', first['thread_id'])
        events, restored = collect(rebuilt['id'])
        assert restored['status'] == 'completed' and restored['backend_thread'] != resumed['backend_thread']
        assert any(e['kind'] == 'backend_session_rebuilt' for e in events)

        for action in ('steer', 'cancel'):
            active = start('WAIT')
            collect(active['id'], kind='reasoning_delta')
            if action == 'steer':
                cli(0, action, *remote, active['id'], 'new direction')
                events, result = collect(active['id'])
                assert result['status'] == 'completed'
                assert any(e['kind'] == 'user_message' and e['data'].get('steering') for e in events)
            else:
                cli(0, action, *remote, active['id'])
                assert collect(active['id'])[1]['status'] == 'cancelled'

        failure = start('DROP_TRANSPORT')
        assert collect(failure['id'])[1]['status'] == 'interrupted'
        next_run = start('new request', failure['thread_id'])
        events, result = collect(next_run['id'])
        assert result['status'] == 'completed'
        assert any(e['kind'] == 'recovery_notice' for e in events)
        print('PASS Antigravity exact retries, readiness, late scheduled work, answers, reconnect, native resume, reconstruction, steering, cancellation and transport recovery')

        resident = api({'op': 'resident_create', 'spec': {'id': uid(), 'name': 'Fixture resident',
                       'cwd': str(workspaces[1]), 'backend': 'antigravity', 'model': 'fixture-model', 'auto_activate': False}})
        client = uid()
        for body in ('SILENT', 'REPLY'):
            received = api({'op': 'mailbox_send', 'client_id': client, 'message': {
                'id': uid(), 'to': {'type': 'resident', 'id': resident['id']}, 'subject': 'Fixture mail', 'body': body}})
        inbox = lambda: api({'op': 'client_inbox', 'client_id': client}, local=True)['messages']
        for turn in range(2):
            activation_id = uid()
            activation = {'op': 'resident_activate_checked', 'id': resident['id'],
                          'activation_id': activation_id, 'directions': 'Fixture activation'}
            rejected({**activation, 'required_capabilities': ['live_steering']}, 'live_steering')
            rejected({'op': 'get', 'id': activation_id}, 'not found')
            activation = {**activation, 'required_capabilities': ['native_session_resume']}
            accepted = api(activation)
            assert api(activation)['record'] == accepted['record']
            rejected({**activation, 'required_capabilities': ['late_activity']}, 'different input')
            events, result = collect(accepted['record']['spec']['id'])
            assert result['status'] == 'completed'
            assert any(e['kind'] == 'assistant_message' and e['data']['private'] for e in events)
            deadline = time.monotonic() + 8
            while not (messages := inbox()) and time.monotonic() < deadline:
                time.sleep(.05)
            assert len(messages) == 1, [event['kind'] for event in events]
            delivered, = messages
            assert delivered['body'] == 'Delivered by tool' and delivered['in_reply_to'] == received['id']
            if turn == 0:
                resident_session = result['backend_thread']
                assert any(e['kind'] == 'resident_bridge_call' for e in events)
            else:
                assert result['backend_thread'] == resident_session
                assert not any(e['kind'] == 'resident_bridge_call' and e['data']['params']['name'] == 'boxen_send_message' for e in events)
        print('PASS resident MCP tools, selective explicit reply, attribution, private narration and no-send completion')
    finally:
        for process in processes:
            if process.poll() is None:
                process.kill()
                process.wait(timeout=5)
        for log in logs:
            log.close()
