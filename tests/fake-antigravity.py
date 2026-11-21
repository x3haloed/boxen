#!/usr/bin/env python3
"""Controlled native ACP counterpart for Antigravity daemon integration tests."""
import json
import pathlib
import secrets
import sys
import time
import urllib.request

assert sys.argv[1:3] == ['acp', '--process-per-session']
session = 'fixture-session'
state = pathlib.Path('.native-acp-session')
prompt_id = None
mcp = None
opts = [
    {'id': 'model', 'type': 'select', 'currentValue': 'fixture-default',
     'options': [{'value': value, 'name': value} for value in ('fixture-default', 'fixture-model')]},
    {'id': 'thought_level', 'type': 'select', 'currentValue': 'low',
     'options': [{'value': value, 'name': value} for value in ('low', 'high')]},
]


def send(**message):
    print(json.dumps({'jsonrpc': '2.0', **message}), flush=True)


def update(update_type, **fields):
    send(method='session/update', params={'sessionId': session, 'update': {'sessionUpdate': update_type, **fields}})


def chunk(text, thought=False):
    update('agent_thought_chunk' if thought else 'agent_message_chunk', content={'type': 'text', 'text': text})


def bridge(method, params=None, ident=1):
    request = urllib.request.Request(
        mcp['url'], data=json.dumps({'jsonrpc': '2.0', 'id': ident, 'method': method, 'params': params or {}}).encode(),
        headers={'Authorization': mcp['headers'][0]['value'], 'Content-Type': 'application/json'})
    with urllib.request.urlopen(request, timeout=5) as response:
        return json.load(response)['result']


def tool(name, arguments, ident):
    result = bridge('tools/call', {'name': name, 'arguments': arguments}, ident)
    assert not result.get('isError'), result
    return json.loads(result['content'][0]['text'])


for line in sys.stdin:
    message = json.loads(line)
    assert message['jsonrpc'] == '2.0'
    method = message.get('method')
    ident = message.get('id')
    params = message.get('params', {})
    if method == 'initialize':
        assert params['clientInfo']['name'] == 'boxen'
        send(id=ident, result={'protocolVersion': 1, 'agentInfo': {'version': '1.5.13'},
             'agentCapabilities': {'sessionCapabilities': {'resume': {}}, 'mcpCapabilities': {'http': True}}})
    elif method in ('session/new', 'session/resume'):
        mcp, = params['mcpServers']
        assert pathlib.Path(params['cwd']).resolve() == pathlib.Path.cwd().resolve()
        if method == 'session/resume' and (not state.exists() or state.read_text() != params['sessionId']):
            send(id=ident, error={'code': -32602, 'message': 'Unknown session'})
            continue
        if method == 'session/new':
            session = 'fixture-session-' + secrets.token_hex(8)
            state.write_text(session)
        else:
            session = params['sessionId']
        tools = bridge('tools/list')['tools']
        assert any(tool['name'] == 'boxen_work' for tool in tools)
        send(id=ident, result={'sessionId': session, 'configOptions': opts})
    elif method == 'session/set_config_option':
        option = next(option for option in opts if option['id'] == params['configId'])
        option['currentValue'] = params['value']
        send(id=ident, result={'configOptions': opts})
    elif method == 'session/prompt':
        prompt, *additional = [part['text'] for part in params['prompt']]
        if prompt == 'DROP_TRANSPORT':
            sys.exit(0)
        if prompt_id is not None:
            send(id=prompt_id, result={'stopReason': 'end_turn'})
            prompt_id = None
        chunk('Fixture reasoning.', thought=True)
        if prompt == 'LATE_WAKE':
            update('tool_call', toolCallId='timer', kind='other', title='Running schedule', status='in_progress')
            send(id=ident, result={'stopReason': 'end_turn'})
            send(id='wake-question', method='session/request_permission', params={
                'sessionId': session, 'toolCall': {'toolCallId': 'interaction_wake', 'title': 'Which option?'},
                'options': [{'optionId': 'first', 'name': 'First', 'kind': 'allow_once'},
                            {'optionId': 'second', 'name': 'Second', 'kind': 'allow_once'}]})
            continue
        if prompt == 'WAIT':
            prompt_id = ident
            continue
        if prompt == 'RECONSTRUCT':
            assert 'Historical effects' in additional[0] and 'LATE_WAKE' in additional[0]
        if prompt.startswith('HANDOFF_REQUEST '):
            child = prompt.split(' ', 1)[1]
            result = tool('boxen_work', {'action': 'handoff', 'request_id': child, 'prompt': 'ACP delegated task'}, 5)
            assert result['acceptance'] == 'accepted' and result['work']['request_id'] == child
            chunk('ACP transfer accepted.')
            send(id=ident, result={'stopReason': 'end_turn'})
            continue
        if prompt.startswith(('Mailbox activation.', 'Message activation.')):
            activation = json.loads(prompt.rsplit('\n', 1)[1])
            incoming = activation['messages']
            if activation['directions'] == 'SCHEDULE_FIXTURE':
                tool('boxen_schedule', {'action': 'create', 'spec': {
                    'id': secrets.token_hex(32), 'name': 'ACP fixture wake',
                    'at_ms': int(time.time() * 1000) + 900, 'directions': 'ACP scheduled follow-up',
                }}, 4)
            if activation['directions'] == 'ACP scheduled follow-up':
                assert activation['schedule']['handoff']['run_id']
                assert '"handoff"' in additional[0]
            inbox = tool('boxen_mailbox', {}, 2)
            for mail in incoming:
                assert any(item['id'] == mail['id'] and item['delivery'] == 'presented' for item in inbox['messages'])
            for mail in incoming:
                if mail['body'] == 'REPLY':
                    tool('boxen_send_message', {'to': mail['reply_to'], 'subject': 'Explicit reply',
                         'body': 'Delivered by tool', 'in_reply_to': mail['id']}, 3)
            chunk('Private ACP narration.')
        else:
            update('tool_call', toolCallId='cmd', kind='execute', title='Fixture command', status='in_progress')
            update('tool_call_update', toolCallId='cmd', status='completed',
                   rawOutput={'combinedOutput': 'aggregate from native command', 'exitCode': 0})
            chunk('ACP response: ' + prompt)
        send(id=ident, result={'stopReason': 'end_turn'})
    elif method == 'session/cancel':
        if prompt_id is not None:
            send(id=prompt_id, result={'stopReason': 'cancelled'})
            prompt_id = None
    elif method is None and ident == 'wake-question':
        assert message['result'] == {'outcome': {'outcome': 'selected', 'optionId': 'second'}}
        update('tool_call_update', toolCallId='timer', status='completed')
        chunk('Late answer after the scheduled wake.')
        update('usage_update', used=20, size=100)
    else:
        raise AssertionError(method)
