#!/usr/bin/env python3
"""Exercise a guest browser origin against real Boxen processes and a controlled Codex."""
import base64
import concurrent.futures
import json
import pathlib
import secrets
import socket
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

from smoke_helpers import fake_codex

ROOT = pathlib.Path(__file__).resolve().parent.parent
BINARY = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else ROOT / 'target/release/boxen').resolve()
COMMAND = (['/bin/bash'] if BINARY.suffix == '.com' else []) + [str(BINARY)]


def port():
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        return listener.getsockname()[1]


def request(url, payload=None, headers=None, status=200):
    req = urllib.request.Request(
        url,
        headers={'Content-Type': 'application/json', **(headers or {})},
        data=payload if isinstance(payload, bytes) else json.dumps(payload).encode() if payload is not None else None,
    )
    try:
        response = urllib.request.urlopen(req, timeout=6)
    except urllib.error.HTTPError as error:
        response = error
    with response:
        assert response.code == status, (response.code, status, response.read())
        return json.load(response) if response.headers.get_content_type() == 'application/json' else response.read()


def until(check):
    deadline = time.monotonic() + 25
    while time.monotonic() < deadline:
        result = check()
        if result:
            return result
        time.sleep(.08)
    raise AssertionError('guest chat did not reach the expected state')


with tempfile.TemporaryDirectory(prefix='boxen-guest-chat-') as directory:
    home = pathlib.Path(directory) / 'daemon'
    cli = COMMAND + ['--data-dir', str(home)]
    machine = subprocess.check_output(cli + ['identity'], text=True).strip()
    peer, ui, chat_port = port(), port(), port()
    control = f'http://127.0.0.1:{ui}'
    origin = f'http://127.0.0.1:{chat_port}'
    headers = {'X-Boxen-Chat': '1'}
    children = []

    def start_daemon():
        process = subprocess.Popen(
            cli + ['serve', '--listen', f'127.0.0.1:{peer}', '--ui', f'127.0.0.1:{ui}',
                   '--codex', str(fake_codex())], stdout=subprocess.PIPE, text=True,
        )
        children.append(process)
        lines = [process.stdout.readline() for _ in range(3)]
        assert 'Local controls:' in lines[-1], lines
        return process

    def start_chat(instance):
        process = subprocess.Popen(
            cli + ['guest-chat', '--instance', instance, '--url', control, '--listen', f'127.0.0.1:{chat_port}'],
            stdout=subprocess.PIPE, text=True,
        )
        children.append(process)
        assert process.stdout.readline().strip() == f'Guest chat origin: {origin}'
        return process

    def stop(process):
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait(timeout=5)
        process.stdout.close()

    def api(op, *, status=200, runner=None, **fields):
        envelope = {'peer': machine, 'request': {'op': op, **fields}}
        if runner:
            envelope['connector_instance'] = runner
        return request(control + '/api/operation', envelope, {
            'Authorization': 'Bearer ' + (home / 'control-token').read_text().strip(),
        }, status)

    def messaging(action, **fields):
        return api('messaging', args={'action': action, **fields})

    def resident(name):
        return api('resident_create', spec={'id': secrets.token_hex(32), 'name': name, 'cwd': directory})['id']

    def create(resident_id):
        return json.loads(subprocess.check_output(
            cli + ['guest-chat-create', resident_id, '--url', control], text=True, timeout=10,
        ))['definition']['id']

    def page(after=0, status=200):
        return request(origin + f'/messages?after={after}', headers=headers, status=status)

    def send(body, ident=None, status=200, attachments=None):
        payload = {'id': ident or secrets.token_hex(32), 'body': body}
        if attachments is not None:
            payload['attachments'] = attachments
        return request(origin + '/messages', payload, headers, status)

    def upload(name, data, status=200, extra_headers=None):
        return request(origin + '/uploads?name=' + urllib.parse.quote(name), data,
                       {**headers, 'Content-Type': 'application/octet-stream', **(extra_headers or {})}, status)

    def inbox(target):
        return messaging('inbox', conversation='resident:' + target)['messages']

    try:
        daemon = start_daemon()
        agent = resident('Guest assistant')
        other = resident('Other assistant')
        instance = create(agent)
        other_instance = create(agent)
        runner = start_chat(instance)
        assert b'<textarea' in request(origin + '/')
        assert page()['resident'] == 'Guest assistant' and page()['status'] == 'ready'
        assert page()['messages'] == []
        request(origin + '/messages', status=403)
        request(origin + '/messages', {'body': 'cross-site'}, {**headers, 'Sec-Fetch-Site': 'cross-site'}, 403)
        request(origin + '/', headers={'Host': 'untrusted.example'}, status=403)
        request(origin + '/api/operation', {'op': 'resident_list'}, headers, 404)
        request(origin + '/messages', {'body': 'forged routing', 'id': secrets.token_hex(32), 'resident': other}, headers, 400)
        send('x' * 8193, status=422)
        api('messaging', runner=instance, status=422, args={'action': 'guest_chat_read', 'instance': other_instance})
        api('messaging', runner=instance, status=422, args={'action': 'ingest', 'instance': instance})
        api('messaging', runner=instance, status=422, args={'action': 'outbox', 'instance': instance})
        api('resident_list', runner=instance, status=422)

        api('messaging', runner=other_instance, args={
            'action': 'guest_chat_send', 'instance': other_instance, 'id': secrets.token_hex(32), 'body': 'Other guest secret',
        })
        messaging('send', id=secrets.token_hex(32), client_id=secrets.token_hex(32),
                  to={'instance': 'boxen', 'conversation': 'resident:' + agent},
                  subject='Private', body='Private mailbox secret')
        silent_id = secrets.token_hex(32)
        send('No explicit reply <script>plain text</script> SLOW', silent_id)
        until(lambda: page()['status'] == 'working')
        until(lambda: any(m['body'].startswith('No explicit') and m['presented_in'] for m in inbox(agent)))
        first = page()
        assert [(m['sender'], m['body']) for m in first['messages']] == [
            ('guest', 'No explicit reply <script>plain text</script> SLOW'),
        ]
        assert not inbox(other)

        reply = {'action': 'send', 'to': {'instance': instance, 'conversation': 'chat'}, 'body': 'Explicit guest reply'}
        body = 'MESSAGING:' + json.dumps(reply)
        submission = secrets.token_hex(32)
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            one, two = list(pool.map(lambda _: send(body, submission), range(2)))
        assert one == two and one['accepted']
        send('Changed retry', submission, status=422)
        tail = until(lambda: (p if len((p := page(first['next_after']))['messages']) == 2 else None))
        assert [m['sender'] for m in tail['messages']] == ['guest', 'resident']
        assert tail['messages'][-1]['body'] == 'Explicit guest reply'
        assert set(tail['messages'][-1]) == {'id', 'sender', 'body', 'at'}
        assert messaging('status', id=tail['messages'][-1]['id'])['delivery']['status'] == 'accepted'
        assert messaging('outbox', instance=instance)['messages'] == []
        assert len([m for m in inbox(agent) if m['body'] == body]) == 1

        png = base64.b64decode('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jZ1sAAAAASUVORK5CYII=')
        reply_file = pathlib.Path(directory) / 'resident.png'
        reply_file.write_bytes(png)
        upload('cross-site', b'denied', status=403, extra_headers={'Sec-Fetch-Site': 'cross-site'})
        image = upload('guest image.png', png)
        blob = b'<html>arbitrary bytes</html>' * 20500
        document = upload('example.html', blob)
        empty = upload('empty.txt', b'')
        send('', status=422, attachments=[{**image, 'proof': secrets.token_hex(32)}])
        with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
            overlapping = b'concurrent' * 62000
            uploads = list(pool.map(lambda n: upload(f'parallel-{n}.bin', overlapping), range(2)))
        assert uploads[0]['file']['digest'] == uploads[1]['file']['digest']
        ident = secrets.token_hex(32)
        receipt = send('', ident, attachments=[image, document, empty])
        assert send('', ident, attachments=[image, document, empty]) == receipt
        send('', ident, status=422, attachments=[image])
        received = until(lambda: next((m for m in inbox(agent) if m['id'] == receipt['id'] and m['presented_in']), None))
        assert [item['name'] for item in received['attachments']] == ['guest image.png', 'example.html', 'empty.txt']
        asset = origin + '/attachments/' + receipt['id']
        assert request(asset + '/0', headers=headers) == png
        assert request(asset + '/1?download', headers=headers) == blob
        assert request(asset + '/2?download', headers=headers) == b''
        request(asset + '/0', status=403)
        request(asset + '/0', headers={**headers, 'Sec-Fetch-Site': 'cross-site'}, status=403)
        api('messaging', runner=other_instance, status=422, args={
            'action': 'guest_chat_attachment', 'instance': other_instance, 'message_id': receipt['id'], 'index': 0,
        })
        files_in_reply = {
            'action': 'call', 'to': reply['to'], 'operation': 'send_files',
            'arguments': {'paths': [reply_file.name], 'in_reply_to': receipt['id']},
        }
        send('MESSAGING:' + json.dumps(files_in_reply))
        outgoing = until(lambda: next((m for m in page()['messages'] if m['sender'] == 'resident' and m.get('attachments')), None))
        assert outgoing['attachments'] == [{'name': reply_file.name, 'size': len(png), 'image': True}]
        assert request(origin + f'/attachments/{outgoing["id"]}/0', headers=headers) == png
        with urllib.request.urlopen(urllib.request.Request(asset + '/1', headers=headers), timeout=6) as response:
            assert response.headers.get_content_type() == 'application/octet-stream'
            assert response.headers['Content-Disposition'].startswith('attachment;')
        until(lambda: page()['status'] == 'ready')
        api('resident_update', id=agent, patch={'expected_revision': 1, 'auto_activate': False})
        assert page()['status'] == 'offline'
        before_restart = page()['messages']
        reply_file.unlink()

        stop(runner)
        stop(daemon)
        daemon = start_daemon()
        runner = start_chat(instance)
        assert page()['messages'] == before_restart
        assert request(origin + f'/attachments/{outgoing["id"]}/0', headers=headers) == png
        assert send(body, submission) == one
        messaging('update', instance=instance, expected_revision=1, enabled=False)
        page(status=422)
        send('After disable', status=422)
        messaging('update', instance=instance, expected_revision=2, enabled=True)
        assert page()['messages'] == before_restart
        print('Guest chat passed status, attachment round trips, message isolation, exact retry, restart, and disable checks')
    finally:
        for child in reversed(children):
            stop(child)
