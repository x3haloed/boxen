#!/usr/bin/env python3
"""Deterministic Claude CLI stream fixture. No provider credentials or inference."""
import json
from pathlib import Path
import sys
import time
import uuid


def emit(kind, **fields):
    print(json.dumps({'type': kind, **fields}), flush=True)


def reply(request, **fields):
    emit('control_response', response={
        'subtype': 'success', 'request_id': request['request_id'], 'response': fields,
    })


def argument(flag, default=None):
    return sys.argv[sys.argv.index(flag) + 1] if flag in sys.argv else default


def system(subtype, **fields):
    emit('system', subtype=subtype, session_id=session, **fields)


def part(kind, **fields):
    emit('stream_event', event={'type': kind, **fields})


def result(text, terminal='completed'):
    emit('result', subtype='success', session_id=session, terminal_reason=terminal, result=text)


def answer(text):
    part('message_start')
    part('content_block_start', index=0, content_block={'type': 'text', 'text': ''})
    part('content_block_delta', index=0, delta={'type': 'text_delta', 'text': text})
    part('content_block_stop', index=0)
    part('message_stop')


assert argument('--input-format') == 'stream-json'
assert argument('--output-format') == 'stream-json'
assert '--include-partial-messages' in sys.argv
stored = Path('.claude-fixture-session')
resume = argument('--resume')
session = resume or str(uuid.uuid4())
model = argument('--model', 'fixture-default')
catalog = '--no-session-persistence' in sys.argv
waiting_on_task = False
if resume and (not stored.exists() or stored.read_text() != resume):
    emit('result', subtype='error_during_execution',
         errors=['No conversation found with session ID: ' + resume])
    sys.exit(0)

for line in sys.stdin:
    item = json.loads(line)
    if item['type'] == 'control_request':
        verb = item['request']['subtype']
        if verb == 'initialize':
            if catalog:
                assert '--strict-mcp-config' in sys.argv and argument('--tools') == ''
                with Path('catalog-lookups').open('a') as ledger:
                    ledger.write('catalog\n')
            reply(item, models=[
                {'value': 'default', 'displayName': 'Default (recommended)'},
                {'value': 'fixture-default', 'displayName': 'Fixture Default'},
                {'value': 'fixture-other', 'displayName': 'Fixture Other'},
            ])
        elif verb == 'mcp_set_servers':
            assert not catalog
            info = item['request']['servers']['boxen']
            assert info['type'] == 'http' and info['headers']['Authorization'].startswith('Bearer ')
            reply(item, added=['boxen'], removed=[], errors={})
        elif verb == 'interrupt':
            reply(item, still_queued=[])
            if not waiting_on_task:
                result('', 'aborted_streaming')
                break
        elif verb == 'stop_task':
            assert waiting_on_task and item['request']['task_id'] == 'fixture-task'
            system('task_notification', task_id='fixture-task', status='stopped')
            reply(item)
            break
        continue

    assert item['type'] == 'user' and not catalog
    prompt, context = (block['text'] for block in item['message']['content'])
    stored.write_text(session)
    system('init', model=model)
    if prompt == 'HELD_FOLLOWUP_GATE':
        deadline = time.monotonic() + 15
        while not Path('release-held-followup').exists():
            assert time.monotonic() < deadline, 'fixture gate was not released'
            time.sleep(.025)
    if prompt == 'INTERRUPT':
        continue
    if prompt == 'BROKEN':
        part('message_start')
        part('content_block_start', index=0, content_block={'type': 'tool_use', 'id': 'unknown-effect', 'name': 'Bash'})
        emit('assistant', message={'content': [{
            'type': 'tool_use', 'id': 'unknown-effect', 'name': 'Bash', 'input': {},
        }]})
        sys.exit(3)
    if prompt in ('BACKGROUND', 'STOP_TASK'):
        part('message_start')
        part('content_block_start', index=0, content_block={'type': 'thinking', 'thinking': 'Fixture reasoning'})
        part('content_block_stop', index=0)
        part('content_block_start', index=1, content_block={'type': 'tool_use', 'id': 'fixture-call', 'name': 'Bash'})
        part('content_block_delta', index=1, delta={'type': 'input_json_delta', 'partial_json': '{"run_in_background":true}'})
        emit('assistant', message={'content': [{
            'type': 'tool_use', 'id': 'fixture-call', 'name': 'Bash',
            'input': {'run_in_background': True},
        }]})
        part('content_block_stop', index=1)
        part('message_stop')
        system('task_started', task_id='fixture-task', task_type='local_bash', tool_use_id='fixture-call')
        emit('user', message={'content': [{
            'type': 'tool_result', 'tool_use_id': 'fixture-call', 'content': 'Background dispatch',
        }]})
        answer('Native prompt ended.')
        result('Native prompt ended.')
        if prompt == 'STOP_TASK':
            waiting_on_task = True
            continue
        time.sleep(.3)
        system('task_notification', task_id='fixture-task', status='completed', summary='Command completed.')
        assert sys.stdin.readline() == '', 'Boxen keeps the input open until background work settles'
        system('init', model=model)
        answer('Native wake completed.')
        result('Native wake completed.')
        break
    if prompt == 'RECONSTRUCT':
        assert 'Hidden state and omitted payloads remain unavailable' in context
        assert 'Native wake completed.' in context
    answer('Fixture response.')
    result('Fixture response.')
