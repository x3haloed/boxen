#!/usr/bin/env python3
"""Two cheap, synthetic OpenRouter exchanges with fresh-process native-state replay."""
import argparse
import json
import pathlib
import subprocess
import tempfile

parser = argparse.ArgumentParser()
parser.add_argument('binary', nargs='?', default='target/release/boxen')
parser.add_argument('--key-file', type=pathlib.Path, default=pathlib.Path.home()/'.boxen/credentials/openrouter.key')
parser.add_argument('--model', default='deepseek/deepseek-v4.1-flash')
args = parser.parse_args()
binary = pathlib.Path(args.binary).resolve()
command = (['/bin/bash'] if binary.suffix == '.com' else []) + [str(binary)]
marker = 'boxen-replay-proof'
with tempfile.TemporaryDirectory(prefix='boxen-live-probe-') as temp:
    root = pathlib.Path(temp)
    body = {'model': args.model, 'messages': [{'role': 'user', 'content':
            f'Please call probe_echo with text exactly "{marker}". After its result, reply with that same marker.'}],
            'tools': [{'type': 'function', 'function': {'name': 'probe_echo',
                'description': 'Returns its text argument unchanged. This probe has no external effects.',
                'parameters': {'type': 'object', 'properties': {'text': {'type': 'string'}},
                               'required': ['text'], 'additionalProperties': False}}}],
            'tool_choice': {'type': 'function', 'function': {'name': 'probe_echo'}},
            'max_tokens': 256, 'provider': {'require_parameters': True}, 'reasoning': {'enabled': False}}
    def exchange(number):
        request = root/f'request-{number}.json'; request.write_text(json.dumps(body))
        journal = root/f'exchange-{number}.jsonl'
        result = subprocess.run(command + ['inference-probe', '--request', str(request), '--journal', str(journal),
                    '--key-file', str(args.key_file)], capture_output=True, text=True, timeout=140)
        if result.returncode: raise RuntimeError(result.stderr.strip())
        # Fresh process: no object graph or in-memory SDK state survives the exchange.
        projection = json.loads(subprocess.check_output(command + ['inference-replay', '--journal', str(journal)], text=True))
        assert projection['completed'] and projection['projection_supported'], projection
        return projection
    first = exchange(1)
    assert first['finish_reason'] == 'tool_calls'
    assistant = first['assistant']
    assert len(assistant['tool_calls']) == 1
    call = assistant['tool_calls'][0]
    assert call['function']['name'] == 'probe_echo'
    assert json.loads(call['function']['arguments']) == {'text': marker}
    # Deterministic local harness result, not an arbitrary model-chosen command or daemon tool.
    body['messages'] += [assistant, {'role': 'tool', 'tool_call_id': call['id'], 'content': marker}]
    body['tool_choice'] = 'none'
    second = exchange(2)
    assert second['finish_reason'] == 'stop' and marker in second['assistant']['content']
    usage = [first['usage'], second['usage']]
    print(json.dumps({'ok': True, 'binary': str(binary), 'model': args.model,
            'exchanges': 2, 'fresh_process_replay': True, 'tool_result_roundtrip': True,
            'total_tokens': sum(u.get('total_tokens', 0) for u in usage),
            'reported_cost': sum(u.get('cost', 0) for u in usage)}))
