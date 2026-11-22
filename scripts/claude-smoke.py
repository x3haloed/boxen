#!/usr/bin/env python3
"""Claude Code streams, lifecycle and session recovery through two paired daemons."""
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
fixture = python_launcher('fake-claude.py')


def port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


with tempfile.TemporaryDirectory(prefix='boxen-claude-smoke-') as directory:
    scratch = pathlib.Path(directory)
    homes = [scratch / name for name in ('viewer', 'runtime')]
    workspace = scratch / 'workspace'
    workspace.mkdir()
    addresses = [f'127.0.0.1:{port()}' for _ in homes]
    controls = [f'127.0.0.1:{port()}' for _ in homes]
    processes, tokens = [], []

    def cli(index, *args):
        return subprocess.check_output(
            command + ['--data-dir', str(homes[index]), *args], text=True, timeout=15,
        ).strip()

    keys = [cli(index, 'identity') for index in range(len(homes))]
    remote = [addresses[1], keys[1]]

    def boot(index):
        # The fixture works even on a host that has the real runtime installed.
        runtime = ['--claude', str(fixture)] if index else ['--claude', '']
        process = subprocess.Popen(
            command + ['--data-dir', str(homes[index]), 'serve', '--listen', addresses[index],
                       '--ui', controls[index], *runtime],
            stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True,
        )
        lines = PipeLines(process.stdout)
        for _ in range(3):
            line = lines.get()
        assert 'Local controls:' in line, line
        return process, line.strip().split('#')[1]

    def call(request, *, watch=False, local=False, actor=0):
        destination = 0 if local else 1
        request = urllib.request.Request(
            f'http://{controls[actor]}/api/' + ('watch' if watch else 'operation'),
            data=json.dumps({
                'peer': keys[destination], 'address': '' if destination == actor else addresses[destination],
                'request': request,
            }).encode(),
            headers={'Authorization': 'Bearer ' + tokens[actor], 'Content-Type': 'application/json'},
        )
        response = urllib.request.urlopen(request, timeout=15)
        return response if watch else json.load(response)

    def collect(ident, after=0, stop_at=None):
        events = []
        with call({'id': ident, 'after': after}, watch=True) as response:
            for line in response:
                batch = json.loads(line)
                assert 'error' not in batch, batch
                events.extend(batch['events'])
                if batch['done'] or any(event['kind'] == stop_at for event in events):
                    return events, batch['snapshot']
        raise AssertionError('The watcher closed before the expected event')

    def start(prompt, *, ident=None, thread=None, model='fixture-other'):
        args = ['start', *remote, str(workspace), model, prompt,
                '--backend', 'claude', '--id', ident or secrets.token_hex(32)]
        if thread:
            args += ['--thread', thread]
        return json.loads(cli(0, *args))['record']['spec']

    def held(source, action='get', **kwargs):
        return call({'op':'held_followup','id':source['id'],'action':action, **kwargs})['hold']

    def held_phase(source, phase):
        deadline = time.monotonic() + 12
        while True:
            current = held(source)
            if current['phase'] == phase:
                return current
            assert time.monotonic() < deadline, current
            time.sleep(.025)

    try:
        for index in range(len(homes)):
            cli(index, 'trust', keys[1-index], 'Paired fixture', '--address', addresses[1-index])
            p, token = boot(index)
            processes.append(p)
            tokens.append(token)
        for local in (True, False):
            info = call({'op': 'runtime_info'}, local=local)
            runtime = next(backend for backend in info['backends'] if backend['id'] == 'claude')
            assert runtime['configured'] == (not local)
            assert runtime['request_capabilities'] == ['native_session_resume', 'late_activity']
        expected_catalog = {'models': [{'model': 'fixture-default', 'label': 'Fixture Default'},
                                       {'model': 'fixture-other', 'label': 'Fixture Other'}]}
        assert call({'op': 'claude_models'}) == call({'op': 'claude_models'}) == expected_catalog
        assert (homes[1] / 'claude-model-catalog' / 'catalog-lookups').read_text() == 'catalog\n'
        assert call({'op': 'list'}) == []
        assert not json.loads(cli(0, 'log-status', *remote))['enabled']
        diagnostic_path = scratch / 'runtime.jsonl'
        cli(0, 'log-file', *remote, str(diagnostic_path), '--targets', 'runtime', '--level', 'trace')

        first = start('BACKGROUND')
        assert start('BACKGROUND', ident=first['id']) == first
        early, snapshot = collect(first['id'], stop_at='backend_prompt_ended')
        assert snapshot['status'] == 'running' and snapshot['native_prompt'] == 'ended', snapshot
        assert not call({'op': 'thread_result', 'id': first['id']})['done']
        late, snapshot = collect(first['id'], after=early[-1]['seq'])
        events = early + late
        assert snapshot['status'] == 'completed', events
        assert [event['seq'] for event in events] == list(range(1, snapshot['last_seq'] + 1))
        assert [event for event in events if event['kind'] == 'backend_prompt_started'][-1]['data']['source'] == 'native_activity'
        assert any(event['kind'] == 'reasoning_delta' for event in events)
        assert any(event['kind'] == 'tool_result' and event['data']['call_id'] == 'task:fixture-task' for event in events)
        assert call({'op': 'thread_result', 'id': first['id']})['response'] == 'Native prompt ended.\n\nNative wake completed.'
        initial_session = snapshot['backend_thread']

        second = dict(first, id=secrets.token_hex(32), prompt='follow-up', model='fixture-default')
        call({'op': 'start', 'spec': second, 'after': first['id']})
        _, snapshot = collect(second['id'])
        assert snapshot['status'] == 'completed' and snapshot['backend_thread'] == initial_session
        assert call({'op': 'runtime_info', 'run_id': second['id']})['observed']['session']['model'] == 'fixture-default'

        (workspace / '.claude-fixture-session').unlink()
        rebuilt = start('RECONSTRUCT', thread=first['thread_id'])
        recovered, snapshot = collect(rebuilt['id'])
        assert snapshot['status'] == 'completed' and snapshot['backend_thread'] != initial_session
        assert any(event['kind'] == 'backend_session_rebuilt' for event in recovered)

        pending = start('INTERRUPT')
        collect(pending['id'], stop_at='backend_thread')
        try:
            call({'op': 'control', 'id': pending['id'], 'action': {'type': 'steer', 'text': 'Fixture'}})
        except urllib.error.HTTPError as error:
            assert error.code == 422 and 'cancellation only' in error.read().decode()
        else:
            raise AssertionError('Claude advertised an unsupported control')
        cli(0, 'cancel', *remote, pending['id'])
        _, snapshot = collect(pending['id'])
        assert snapshot['status'] == 'cancelled', snapshot

        background = start('STOP_TASK')
        collect(background['id'], stop_at='backend_prompt_ended')
        cli(0, 'cancel', *remote, background['id'])
        stopped, snapshot = collect(background['id'])
        assert snapshot['status'] == 'cancelled', snapshot
        assert any(event['kind'] == 'tool_result' and event['data'].get('status') == 'stopped'
                   for event in stopped)

        source = start('HELD_FOLLOWUP_GATE')
        collect(source['id'], stop_at='backend_thread')
        first_input = {'id':secrets.token_hex(32),'text':'First fixture contribution'}
        try:
            group = held(source, 'append', expected_revision=0, contribution=first_input)
        except urllib.error.HTTPError as error:
            raise AssertionError(error.read().decode()) from error
        assert held(source, 'append', expected_revision=0, contribution=first_input) == group
        second_input = {'id':secrets.token_hex(32),'text':'Second fixture contribution'}
        next_group = call({'op':'held_followup','id':source['id'],'action':'append',
            'expected_revision':group['revision'],'contribution':second_input}, actor=1)['hold']
        assert next_group['run_id'] == group['run_id']
        assert [entry['machine'] for entry in next_group['contributions']] == keys
        try:
            held(source, 'edit', expected_revision=next_group['revision']-1,
                 contribution=first_input|{'text':'Stale edit'})
        except urllib.error.HTTPError as error:
            assert error.code == 422 and 'revision' in error.read().decode()
        else:
            raise AssertionError('A stale hold edit was accepted')
        (workspace / 'release-held-followup').touch()
        follow_up = held_phase(source, 'started')
        _, snapshot = collect(follow_up['run_id'])
        assert snapshot['record']['after'] == source['id'] and snapshot['status'] == 'completed'
        assert all(value in snapshot['record']['spec']['prompt'] for value in
                   (first_input['text'], second_input['text']))

        cancelled_source = start('INTERRUPT')
        collect(cancelled_source['id'], stop_at='backend_thread')
        held(cancelled_source, 'append', expected_revision=0,
             contribution={'id':secrets.token_hex(32),'text':'Returned on cancellation'})
        cli(0, 'cancel', *remote, cancelled_source['id'])
        assert held_phase(cancelled_source, 'returned')['reason'] == 'predecessor_cancelled'

        broken = start('BROKEN')
        events, snapshot = collect(broken['id'])
        assert snapshot['status'] == 'interrupted'
        assert not any(event['kind'] == 'tool_result' for event in events)
        cli(0, 'log-off', *remote)
        retained = diagnostic_path.read_text()
        diagnostics = [json.loads(line) for line in retained.splitlines()]
        first_runtime = [r for r in diagnostics if r['context'].get('run_id') == first['id']]
        assert {'claude.session_ready', 'claude.task_activity', 'claude.prompt_ended'} <= {
            record['event'] for record in first_runtime
        }
        assert 'Bearer ' not in retained and 'Native wake completed.' not in retained
        restarted_source = start('INTERRUPT')
        collect(restarted_source['id'], stop_at='backend_thread')
        held(restarted_source, 'append', expected_revision=0,
             contribution={'id':secrets.token_hex(32),'text':'Retained through restart'})
        processes[1].kill()
        processes[1].wait(timeout=5)
        processes[1], tokens[1] = boot(1)
        assert held_phase(restarted_source, 'returned')['reason'] == 'predecessor_interrupted'
        print('PASS held Claude follow-up, independent contributors, revision checks, cancellation and restart')
        replay, snapshot = collect(broken['id'])
        assert events == replay and snapshot['status'] == 'interrupted'
        print('PASS Claude model discovery, exact-ID submission, late background wake, native model change and resume, explicit reconstruction, cancellation and interrupted replay')
    finally:
        for process in processes:
            if process.poll() is None:
                process.kill()
            process.wait(timeout=5)
