#!/usr/bin/env python3
"""Exercise the opt-in Rust transport and fresh-process replay; no external key needed."""
import http.server
import json
import pathlib
import subprocess
import ssl
import sys
import tempfile
import threading
import time

binary = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else 'target/release/boxen').resolve()
command = (['/bin/bash'] if binary.suffix == '.com' else []) + [str(binary)]
requests = []
replay_details = [{'index': 0, 'id': 'opaque-1', 'type': 'reasoning.encrypted',
                   'format': 'fixture-v1', 'data': 'opaque-payload', 'future': {'retained': True}}]

def frame(delta=None, finish=None, **extra):
    return {'id': 'response-1', 'choices': [{'index': 0, 'delta': delta or {}, 'finish_reason': finish}], **extra}

def encode(value):
    # Multiple data lines, CRLF and UTF-8 are deliberately split by the transport below.
    return ('data: ' + json.dumps(value, ensure_ascii=False, indent=1).replace('\n', '\r\ndata: ') + '\r\n\r\n').encode()

class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_): pass
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        requests.append(body)
        if self.path == '/redirect':
            self.send_response(302); self.send_header('Location', '/good'); self.end_headers(); return
        self.send_response(200); self.send_header('Content-Type', 'text/event-stream'); self.end_headers()
        try:
            if self.path == '/slow': time.sleep(2); return
            if self.path == '/error':
                self.wfile.write(encode(frame({'content': 'partial'})) + encode({'error': {'message': 'fixture failure'}})); return
            if self.path == '/large': self.wfile.write(b'data: ' + b'x' * (1024 * 1024 + 1)); return
            if self.path == '/reply':
                assert body['messages'][-2]['reasoning_details'] == replay_details
                assert body['messages'][-1]['tool_call_id'] == 'call-1'
                wire = encode(frame({'content': 'Result received.'})) + encode(frame(finish='stop'))
            else:
                wire = b': keepalive\r\n\r\n'
                wire += encode(frame({'role': 'assistant', 'content': 'hé'}))
                wire += encode(frame({'content': 'llo', 'future_field': {'kept': True},
                                      'reasoning_details': [{**replay_details[0], 'data': 'opaque-'}],
                                      'tool_calls': [{'index': 0, 'id': 'call-1', 'type': 'function',
                                                      'function': {'name': 'probe_echo', 'arguments': '{"text":'}}]}))
                wire += encode(frame({'reasoning_details': [{'index': 0, 'data': 'payload'}],
                                      'tool_calls': [{'index': 0, 'function': {'arguments': '"hello"}'}}]}))
                if self.path == '/cut': self.wfile.write(wire); return
                wire += encode(frame(finish='length' if self.path == '/length' else 'tool_calls'))
                # OpenRouter-style repeated finish in a late accounting frame.
                wire += encode(frame(finish='length' if self.path == '/length' else 'tool_calls', usage={'total_tokens': 17}))
            for offset in range(0, len(wire), 7):
                self.wfile.write(wire[offset:offset + 7]); self.wfile.flush()
            time.sleep(.15)  # consumer must see retained frames before completion
            self.wfile.write(b'data: [DONE]\r\n\r\n'); self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError): pass

server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
threading.Thread(target=server.serve_forever, daemon=True).start()
endpoint = f'http://127.0.0.1:{server.server_port}'
with tempfile.TemporaryDirectory(prefix='boxen-inference-probe-') as temp:
    root = pathlib.Path(temp)
    request = root / 'request.json'; request.write_text(json.dumps({'model': 'fixture', 'messages': []}))
    def probe(case):
        journal = root / (case + '.jsonl')
        result = subprocess.run(command + ['inference-probe', '--endpoint', endpoint + '/' + case,
                '--request', str(request), '--journal', str(journal), '--timeout-seconds', '1'],
                capture_output=True, text=True, timeout=8)
        projection = subprocess.run(command + ['inference-replay', '--journal', str(journal)],
                                    capture_output=True, text=True, timeout=8)
        assert projection.returncode == 0, projection.stderr
        return result, json.loads(projection.stdout), journal
    # A separate process can read the first retained frame while the request is still open.
    journal = root / 'stream.jsonl'
    proc = subprocess.Popen(command + ['inference-probe', '--endpoint', endpoint + '/good',
              '--request', str(request), '--journal', str(journal)], stdout=subprocess.PIPE, text=True)
    saw_frame = False
    for line in proc.stdout:
        event = json.loads(line)
        if event['type'] == 'provider_frame' and not saw_frame:
            assert proc.poll() is None
            assert json.loads(journal.read_text().splitlines()[-1])['seq'] >= event['seq']
            saw_frame = True
    assert proc.wait(timeout=5) == 0 and saw_frame
    projection = json.loads(subprocess.check_output(command + ['inference-replay', '--journal', str(journal)], text=True))
    assert projection['completed'] and projection['projection_supported']
    assistant = projection['assistant']
    assert assistant['content'] == 'héllo'
    assert assistant['future_field'] == {'kept': True}
    assert assistant['reasoning_details'] == replay_details
    call = assistant['tool_calls'][0]
    assert call['id'] == 'call-1' and call['function']['arguments'] == '{"text":"hello"}'
    assert projection['usage']['total_tokens'] == 17
    # The harness supplies a deterministic tool result, then a new Rust process resumes inference.
    request.write_text(json.dumps({'model': 'fixture', 'messages': [assistant,
        {'role': 'tool', 'tool_call_id': call['id'], 'content': 'hello'}]}))
    result, resumed, _ = probe('reply')
    assert result.returncode == 0 and resumed['assistant']['content'] == 'Result received.'
    for case in ['cut', 'error', 'redirect', 'slow', 'large']:
        result, projection, _ = probe(case)
        assert result.returncode != 0, case
        assert not projection['completed'] and projection['assistant'] is None, case
    result, projection, _ = probe('length')
    assert result.returncode == 0 and projection['finish_reason'] == 'length'
    # Consumers cannot mistake a partial journal tail for a completed exchange.
    torn = root / 'torn.jsonl'; torn.write_bytes(b'{"seq":1,"type":"exchange_started"}\n{"seq":2')
    torn_result = json.loads(subprocess.check_output(command + ['inference-replay', '--journal', str(torn)], text=True))
    assert not torn_result['completed'] and torn_result['assistant'] is None
    # An unknown changing field is retained raw but cannot silently enter accepted context.
    unknown = root/'unknown.jsonl'
    records = [{'type': 'exchange_started'},
        {'type': 'provider_frame', 'frame': frame({'future': 'one'})},
        {'type': 'provider_frame', 'frame': frame({'future': 'two'}, finish='stop')},
        {'type': 'exchange_completed'}]
    unknown.write_text(''.join(json.dumps({**record, 'seq': i+1})+'\n' for i, record in enumerate(records)))
    unsupported = json.loads(subprocess.check_output(command + ['inference-replay', '--journal', str(unknown)], text=True))
    assert unsupported['completed'] and not unsupported['projection_supported'] and unsupported['assistant'] is None
    broken = root/'broken.jsonl'; broken.write_text('{"seq":2,"type":"exchange_started"}\n')
    assert subprocess.run(command + ['inference-replay', '--journal', str(broken)], capture_output=True).returncode != 0
    # Untrusted certificates fail through the actual Rust TLS stack, without disabling verification.
    certificate = root/'cert.pem'; private_key = root/'cert.key'
    subprocess.run(['openssl', 'req', '-x509', '-newkey', 'rsa:2048', '-nodes',
        '-keyout', str(private_key), '-out', str(certificate), '-days', '1', '-subj', '/CN=127.0.0.1'],
        check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    tls_server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Handler)
    tls_context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls_context.load_cert_chain(certificate, private_key)
    tls_server.socket = tls_context.wrap_socket(tls_server.socket, server_side=True)
    threading.Thread(target=tls_server.serve_forever, daemon=True).start()
    untrusted = subprocess.run(command + ['inference-probe', '--endpoint',
        f'https://127.0.0.1:{tls_server.server_port}/good', '--request', str(request),
        '--journal', str(root/'untrusted.jsonl')], capture_output=True, timeout=8)
    assert untrusted.returncode != 0
    tls_server.shutdown()
    # Existing journals never get overwritten, and a fixture cannot obtain credentials.
    repeat = subprocess.run(command + ['inference-probe', '--endpoint', endpoint+'/good', '--request', str(request), '--journal', str(journal)], capture_output=True)
    assert repeat.returncode != 0
    fake_key = root/'key'; fake_key.write_text('fixture-secret-never-sent')
    before = len(requests)
    blocked = subprocess.run(command + ['inference-probe', '--endpoint', endpoint+'/good', '--key-file', str(fake_key), '--request', str(request), '--journal', str(root/'blocked')], capture_output=True)
    assert blocked.returncode != 0 and len(requests) == before
server.shutdown()
print(json.dumps({'ok': True, 'binary': str(binary), 'cases': ['stream-before-completion', 'durable-before-output',
    'fresh-process-replay', 'opaque-state-roundtrip', 'unknown-field', 'fragmented-unicode-SSE', 'tool-result-followup',
    'late-usage-duplicate-finish', 'cut-stream', 'provider-error', 'redirect', 'timeout', 'size-bound',
    'length-stop', 'unsupported-projection', 'journal-sequence-validation', 'untrusted-certificate', 'torn-tail', 'no-overwrite', 'credential-origin-boundary']}))
