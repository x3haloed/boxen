#!/usr/bin/env python3
"""Exercise the guest correspondence controls used by the Pub/Sub runner."""
import base64
import concurrent.futures
import hashlib
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

from smoke_helpers import PipeLines, fake_codex

ROOT = pathlib.Path(__file__).resolve().parent.parent
BINARY = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else ROOT / 'target/release/boxen').resolve()
COMMAND = (['/bin/bash'] if BINARY.suffix == '.com' else []) + [str(BINARY)]


def port():
    with socket.socket() as listener:
        listener.bind(('127.0.0.1', 0))
        return listener.getsockname()[1]


def request(url, payload, headers, status):
    req = urllib.request.Request(
        url,
        headers={'Content-Type': 'application/json', **(headers or {})},
        data=json.dumps(payload).encode(),
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
    peer, ui = port(), port()
    control = f'http://127.0.0.1:{ui}'
    children = []

    def start_daemon():
        process = subprocess.Popen(
            cli + ['serve', '--listen', f'127.0.0.1:{peer}', '--ui', f'127.0.0.1:{ui}',
                   '--codex', str(fake_codex())], stdout=subprocess.PIPE, text=True,
        )
        children.append(process)
        stdout = PipeLines(process.stdout)
        lines = [stdout.get() for _ in range(3)]
        assert 'Local controls:' in lines[-1], lines
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

    def chat(action, status=200, connector=None, **fields):
        connector = connector or instance
        return api('messaging', runner=connector, status=status,
                   args={'action': 'guest_chat_' + action, 'instance': connector, **fields})

    def page(after=0, status=200):
        return chat('read', status=status, after=after)

    def send(body, ident=None, status=200, attachments=None):
        fields = {'id': ident or secrets.token_hex(32), 'body': body}
        if attachments is not None:
            fields['attachments'] = attachments
        return chat('send', status=status, **fields)

    def upload(name, data):
        file = {'name': name, 'size': len(data), 'digest': hashlib.sha256(data).hexdigest()}
        current = chat('upload', transfer={'action': 'status', 'file': file})
        while not current['complete']:
            offset = current['next']
            chunk = data[offset:offset + 192 * 1024]
            current = chat('upload', transfer={'action': 'write', 'file': file, 'offset': offset,
                                               'data': base64.b64encode(chunk).decode()})
            assert current['next'] == offset + len(chunk)
        return file

    def assert_original(message_id, index, expected):
        file = chat('attachment', message_id=message_id, index=index)
        assert file['size'] == len(expected)
        assert file['digest'] == hashlib.sha256(expected).hexdigest()
        assert (home / 'attachments' / file['digest']).read_bytes() == expected

    def inbox(target):
        return messaging('inbox', conversation='resident:' + target)['messages']

    try:
        daemon = start_daemon()
        agent = resident('Guest assistant')
        other = resident('Other assistant')
        instance = create(agent)
        other_instance = create(agent)
        assert page()['resident'] == 'Guest assistant' and page()['status'] == 'ready'
        assert page()['messages'] == []
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
        assert set(tail['messages'][-1]) == {'id', 'sender', 'body'}
        assert messaging('status', id=tail['messages'][-1]['id'])['delivery']['status'] == 'accepted'
        assert messaging('outbox', instance=instance)['messages'] == []
        assert len([m for m in inbox(agent) if m['body'] == body]) == 1

        png = base64.b64decode('iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+jZ1sAAAAASUVORK5CYII=')
        reply_file = pathlib.Path(directory) / 'resident.png'
        reply_file.write_bytes(png)
        image = upload('guest image.png', png)
        blob = b'<html>arbitrary bytes</html>' * 20500
        document = upload('example.html', blob)
        empty = upload('empty.txt', b'')
        send('', status=422, attachments=[{**image, 'digest': secrets.token_hex(32)}])
        ident = secrets.token_hex(32)
        receipt = send('', ident, attachments=[image, document, empty])
        assert send('', ident, attachments=[image, document, empty]) == receipt
        send('', ident, status=422, attachments=[image])
        received = until(lambda: next((m for m in inbox(agent) if m['id'] == receipt['id'] and m['presented_in']), None))
        assert [item['name'] for item in received['attachments']] == ['guest image.png', 'example.html', 'empty.txt']
        assert_original(receipt['id'], 0, png)
        assert_original(receipt['id'], 1, blob)
        assert_original(receipt['id'], 2, b'')
        api('messaging', runner=other_instance, status=422, args={
            'action': 'guest_chat_attachment', 'instance': other_instance, 'message_id': receipt['id'], 'index': 0,
        })
        files_in_reply = {
            'action': 'call', 'to': reply['to'], 'operation': 'send_files',
            'arguments': {'paths': [reply_file.name], 'in_reply_to': receipt['id']},
        }
        send('MESSAGING:' + json.dumps(files_in_reply))
        outgoing = until(lambda: next((m for m in page()['messages'] if m['sender'] == 'resident' and m.get('attachments')), None))
        assert outgoing['attachments'] == [{'size': len(png)}]
        assert_original(outgoing['id'], 0, png)
        until(lambda: page()['status'] == 'ready')
        api('resident_update', id=agent, patch={'expected_revision': 1, 'auto_activate': False})
        assert page()['status'] == 'offline'
        before_restart = page()['messages']
        reply_file.unlink()

        stop(daemon)
        daemon = start_daemon()
        assert page()['messages'] == before_restart
        assert_original(outgoing['id'], 0, png)
        assert send(body, submission) == one
        messaging('update', instance=instance, expected_revision=1, enabled=False)
        page(status=422)
        send('After disable', status=422)
        messaging('update', instance=instance, expected_revision=2, enabled=True)
        assert page()['messages'] == before_restart
        print('Guest runner controls passed presence, attachments, isolation, exact retries, restart, and disable checks')
    finally:
        for child in reversed(children):
            stop(child)
