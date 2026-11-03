#!/usr/bin/env python3
"""Real paired daemons, controlled provider: policies, exact recall and recovery.

The provider is deterministic test evidence, not a claim about model recall.
"""
import http.server
from smoke_helpers import python_shell
import json
import pathlib
import secrets
import socket
import subprocess
import sys
import tempfile
import threading
import time
import urllib.error
import urllib.request

ROOT = pathlib.Path(__file__).resolve().parent.parent
BINARY = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else ROOT / 'target/release/boxen').resolve()
COMMAND = (['/bin/bash'] if BINARY.suffix == '.com' else []) + [str(BINARY)]
observed = []


class Provider(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        assert 'Authorization' not in self.headers
        observed.append(body)
        helper = body['messages'][0]['content'].startswith('This is a context-summary helper')
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()

        def emit(delta, finish=None):
            frame = {'choices': [{'index': 0, 'delta': delta, 'finish_reason': finish}]}
            self.wfile.write(('data: ' + json.dumps(frame) + '\n\n').encode())
            self.wfile.flush()

        try:
            if helper:
                if body['model'] == 'slow-summary':
                    time.sleep(10)
                if body['model'] == 'disconnect-summary':
                    emit({'content': 'UNACCEPTED_DISCONNECTED_SUMMARY'})
                    return
                if body['model'] == 'call-summary':
                    emit({'tool_calls': [{'index': 0, 'id': 'forbidden-helper', 'type': 'function', 'function': {'name': 'boxen_shell', 'arguments': json.dumps({'command': 'touch forbidden-summary.txt'})}}]}, 'tool_calls')
                elif body['model'] == 'empty-summary':
                    emit({'content': ''}, 'stop')
                elif body['model'] == 'broken-summary':
                    emit({'content': 'UNACCEPTABLE_PARTIAL_SUMMARY'}, 'length')
                else:
                    emit({'content': 'HELPER_INTERPRETATION: report to Jen, Friday relaxed; unfinished question remains open.'}, 'stop')
            else:
                prompt = next(m['content'] for m in reversed(body['messages']) if m['role'] == 'user')
                if isinstance(prompt, str) and prompt.startswith(('Mailbox activation.','Message activation.')):
                    text = 'PRIVATE_ACTIVITY'
                elif prompt == 'EFFECT' and not any(m['role'] == 'tool' for m in body['messages']):
                    emit({'tool_calls': [{'index': 0, 'id': 'effect-call', 'type': 'function', 'function': {
                        'name': 'boxen_shell', 'arguments': json.dumps({'command': python_shell("open('effect.txt','a').write('once'); print('build-observation-' * 4000)")})}}]}, 'tool_calls')
                    self.wfile.write(b'data: [DONE]\n\n')
                    self.wfile.flush()
                    return
                elif prompt == 'RECALL':
                    after = body['messages'][body['messages'].index(next(m for m in reversed(body['messages']) if m['role'] == 'user')) + 1:]
                    if not any(m['role'] == 'tool' for m in after):
                        emit({'tool_calls': [{'index': 0, 'id': 'recall-call', 'type': 'function', 'function': {
                            'name': 'boxen_history', 'arguments': json.dumps({'action': 'search', 'query': "report to Jen"})}}]}, 'tool_calls')
                        self.wfile.write(b'data: [DONE]\n\n')
                        self.wfile.flush()
                        return
                    recall=json.loads(next(m['content'] for m in after if m['role']=='tool'))
                    assert any('report to Jen' in json.dumps(e) for e in recall.get('entries',[])), recall
                    text = 'EXACT_RECALL_OBSERVED'
                else:
                    text = 'WORKLOAD_ANSWER:' + ('ordinary-filler ' * 350)
                emit({'content': text[:20]})
                time.sleep(.02)
                emit({'content': text[20:]}, 'stop')
            self.wfile.write(b'data: [DONE]\n\n')
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass


def port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


provider = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Provider)
threading.Thread(target=provider.serve_forever, daemon=True).start()
with tempfile.TemporaryDirectory(prefix='boxen-compaction-') as temporary:
    directory = pathlib.Path(temporary)
    data = [directory / 'a', directory / 'b']
    peers = [port(), port()]
    uis = [port(), port()]
    processes = []

    def cli(i, *args):
        return subprocess.check_output(COMMAND + ['--data-dir', str(data[i]), *args], text=True, timeout=10).strip()

    keys = [cli(i, 'identity') for i in range(2)]
    for i in range(2):
        cli(i, 'trust', keys[1 - i], 'Compaction peer')

    def daemon(i):
        p = subprocess.Popen(COMMAND + ['--data-dir', str(data[i]), 'serve', '--listen', f'127.0.0.1:{peers[i]}',
                                       '--ui', f'127.0.0.1:{uis[i]}', '--openrouter-endpoint', f'http://127.0.0.1:{provider.server_port}/chat'],
                             stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        processes.append(p)
        lines = [p.stdout.readline() for _ in range(3)]
        assert 'Local controls:' in lines[-1], lines
        return lines[-1].strip().split('#')[1]

    tokens = [daemon(i) for i in range(2)]

    def api(request, watch=False):
        req = urllib.request.Request(f'http://127.0.0.1:{uis[0]}/api/' + ('watch' if watch else 'operation'),
                                     headers={'Authorization': 'Bearer ' + tokens[0], 'Content-Type': 'application/json'},
                                     data=json.dumps({'peer': keys[1], 'address': f'127.0.0.1:{peers[1]}', 'request': request}).encode())
        response = urllib.request.urlopen(req, timeout=20)
        return response if watch else json.load(response)

    def start(prompt, thread=None, mode=None, model='fixture'):
        ident = secrets.token_hex(32)
        spec = {'id': ident, 'thread_id': thread or ident, 'backend': 'openrouter', 'cwd': temporary, 'model': model, 'prompt': prompt}
        if mode:
            spec['context'] = {'mode': mode, 'budget_bytes': 32768, 'pin_quota_bytes': 8192, 'mask_after': 1}
        return api({'op': 'start', 'spec': spec})

    def collect(run, until=None):
        events = []
        with api({'id': run['record']['spec']['id'], 'after': 0}, True) as stream:
            for line in stream:
                batch = json.loads(line)
                assert 'error' not in batch, batch
                events.extend(batch['events'])
                if until and any(e['kind'] == until for e in events):
                    return events, batch['snapshot']
                if batch['done']:
                    return events, batch['snapshot']
        raise AssertionError('watch ended without completion')

    def ctx(thread, action, **fields):
        return api({'op': 'context', 'thread_id': thread, 'args': {'action': action, **fields}})

    def finish(run):
        events, done = collect(run)
        assert done['status'] == 'completed', (done, events[-5:])
        return events

    try:
        cli_run=json.loads(cli(0,'start',f'127.0.0.1:{peers[1]}',keys[1],temporary,'fixture','CLI policy proof','--backend','openrouter','--context','{"mode":"verbatim"}'))
        finish(cli_run)
        cli_status=json.loads(cli(0,'context',f'127.0.0.1:{peers[1]}',keys[1],cli_run['record']['spec']['thread_id'],'{"action":"status"}'))
        assert cli_status['boundary']['settings']['mode']=='verbatim'
        default_run=start('default policy proof')
        finish(default_run)
        assert ctx(default_run['record']['spec']['thread_id'],'status')['boundary']['settings']['mode']=='summary'
        initial = start("I'll have the report to Jen by Friday. Jen later said no rush on Friday. which makes me wonder whether—", mode='verbatim')
        thread = initial['record']['spec']['thread_id']
        finish(initial)
        original = ctx(thread, 'read', limit=1)['entries'][0]
        unauthorized=urllib.request.Request(f'http://127.0.0.1:{uis[1]}/api/operation',headers={'Authorization':'Bearer '+tokens[1],'Content-Type':'application/json'},data=json.dumps({'peer':keys[1],'address':f'127.0.0.1:{peers[1]}','request':{'op':'context','thread_id':thread,'args':{'action':'read'}}}).encode())
        try:
            urllib.request.urlopen(unauthorized,timeout=10)
            raise AssertionError('another owner read this thread history')
        except urllib.error.HTTPError as error:
            assert error.code==422

        pin = secrets.token_hex(32)
        ctx(thread, 'pin', id=pin, entries=[original['id']])
        note = secrets.token_hex(32)
        ctx(thread, 'draft', id=note, text='HELPER_DRAFT_NOT_YET_APPROVED', entries=[original['id']])
        for n in range(26):
            finish(start(f'FILL {n} ' + 'work material ' * 400, thread))
        status = ctx(thread, 'status')
        boundary = status['boundary']
        assert original['id'] in boundary['selected']
        assert not boundary['summary']
        assert not boundary['notes'], 'unreviewed helper prose leaked into continuity'
        assert not any(b['messages'][0]['content'].startswith('This is a context-summary helper') for b in observed)
        coverage=[]
        offset=0
        while True:
            page=ctx(thread,'coverage',offset=offset)
            coverage.extend(page['entries'])
            if page['next_offset'] is None:break
            offset=page['next_offset']
        assert len(coverage) == boundary['source_count']
        assert any(e['state'] == 'omitted' for e in coverage)
        assert ctx(thread, 'read', id=original['id'], limit=1)['entries'][0] == original
        assert not any('HELPER_DRAFT_NOT_YET_APPROVED' in json.dumps(body['messages']) for body in observed)
        try:
            ctx(thread, 'approve', id=note, expected_revision=99)
            raise AssertionError('stale review unexpectedly succeeded')
        except urllib.error.HTTPError as error:
            assert error.code == 422
        bulk=ctx(thread, 'search', query='FILL 0', limit=1)['entries'][0]
        bulk_neighbor=ctx(thread, 'read', id=bulk['id'], limit=2)['entries']
        oversized_pin=secrets.token_hex(32)
        ctx(thread, 'pin', id=oversized_pin, entries=[e['id'] for e in bulk_neighbor])
        finish(start('evaluate oversized declared bundle', thread))
        assert any(b['id']==oversized_pin for b in ctx(thread,'status')['boundary']['excluded_bundles'])
        # Large complete maps stay out of bounded previews; exact records paginate.
        for _ in range(5):
            ctx(thread,'pin',id=secrets.token_hex(32),entries=[e['id'] for e in coverage][:64])
        finish(start('evaluate paged exclusion records',thread))
        state=ctx(thread,'status')
        assert len(json.dumps(state).encode())<49152
        full='';offset=0
        while True:
            page=ctx(thread,'record',id=state['boundary']['id'],byte_offset=offset)
            assert 'serialized_record_excerpt' in page, 'fixture did not exercise record pagination'
            full+=page['serialized_record_excerpt']
            if page['next_byte_offset'] is None:break
            offset=page['next_byte_offset']
        exact_boundary=json.loads(full)['data']
        assert len(exact_boundary['excluded_bundles'])==state['boundary']['excluded_bundles_total']
        ctx(thread, 'approve', id=note, expected_revision=1)
        finish(start('RECALL', thread))
        assert ctx(thread, 'status')['boundary']['notes'][0]['author']['kind'] == 'helper'
        print('PASS repeated verbatim cuts, exact recall, declared pin, draft isolation and revision-specific review', flush=True)

        effect = start('EFFECT', mode='verbatim')
        effect_events = finish(effect)
        effect_thread = effect['record']['spec']['thread_id']
        for n in range(3):
            finish(start('filler ' * 900, effect_thread))
        assert any(e['kind'] == 'context_boundary' and e['data']['masked'] for e in effect_events)
        assert (directory / 'effect.txt').read_text() == 'once'
        processes[1].kill()
        processes[1].wait(timeout=5)
        tokens[1] = daemon(1)
        assert ctx(thread, 'read', id=original['id'], limit=1)['entries'][0] == original
        finish(start('after restart', effect_thread))
        assert (directory / 'effect.txt').read_text() == 'once'
        print('PASS masked historical observation, restart replay and no completed-effect reexecution', flush=True)

        summary = start('summary seed ' + 'source ' * 800, mode='summary')
        finish(summary)
        summary_thread = summary['record']['spec']['thread_id']
        summary_events = []
        for n in range(4):
            summary_events += finish(start('summary fill ' + 'source ' * 800, summary_thread))
        b = ctx(summary_thread, 'status')['boundary']
        assert b['summary']['author']['kind'] == 'summary_helper'
        assert 'HELPER_INTERPRETATION' in b['summary']['text']
        assert any(e['kind'] == 'context_delta' for e in summary_events)
        assert not any(e['kind'] == 'assistant_message' and 'HELPER_INTERPRETATION' in e['data']['text'] for e in summary_events)
        switched = finish(start('policy changes rebuild originals', summary_thread, mode='verbatim'))
        assert not ctx(summary_thread, 'status')['boundary']['summary']
        assert not any(e['kind'] == 'context_summary_started' for e in switched)
        print('PASS summary inference, attribution, separate streams and summary-free policy switch', flush=True)

        for model in ('broken-summary', 'empty-summary', 'call-summary', 'disconnect-summary', 'slow-summary'):
            run = start('seed ' + 'source ' * 800, mode='summary', model=model)
            finish(run)
            ident = run['record']['spec']['thread_id']
            finish(start('fill ' + 'source ' * 800, ident, model=model))
            run = start('fill ' + 'source ' * 800, ident, model=model)
            if model == 'slow-summary':
                events, _ = collect(run, 'context_summary_started')
                api({'op': 'control', 'id': run['record']['spec']['id'], 'action': {'type': 'cancel'}})
                assert collect(run)[1]['status'] == 'cancelled'
            else:
                events = finish(run)
                assert any(e['kind'] == 'context_summary_failed' for e in events)
                b = ctx(ident, 'status')['boundary']
                assert b['settings']['mode'] == 'summary' and b['fallback_reason']
                assert not b['summary']
        assert not (directory / 'forbidden-summary.txt').exists()
        print('PASS empty/truncated/disconnected/call-bearing summary rejection, explicit degraded continuation and helper cancellation', flush=True)
    finally:
        for p in processes:
            if p.poll() is None:
                p.terminate()
                try:
                    p.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    p.kill()
                    p.wait()
        provider.shutdown()
