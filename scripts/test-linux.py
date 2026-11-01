#!/usr/bin/env python3
"""Run the shipped APE regression suite on an SSH-accessible Linux host.

Uses a private temporary directory; leaves installed daemons, identities and
credentials alone. Requires Python 3, Bash, tar and OpenSSL on the test host.
"""
import argparse
import datetime
import hashlib
import json
import os
import pathlib
import platform
import re
import shlex
import signal
import socket
import subprocess
import sys
import tarfile
import tempfile
import time

SUITES = ["platform", "smoke", "workload", "recovery", "resident", "mailbox",
          "logging", "inference-runtime", "media", "inference-probe"]


def worker(binary):
    """Runs on Linux from the uploaded test bundle, without project/toolchain setup."""
    assert platform.system() == "Linux", "test host is not Linux"
    environment = {"os": platform.system(), "architecture": platform.machine(),
                   "kernel": platform.release(), "python": platform.python_version(),
                   "release": pathlib.Path('/etc/os-release').read_text(),
                   "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest()}
    print(json.dumps({"environment": environment}), flush=True)
    results = []
    for suite in SUITES:
        script = "smoke.py" if suite == "smoke" else suite + "-smoke.py"
        start = time.monotonic()
        process = subprocess.Popen([sys.executable, "scripts/" + script, str(binary)],
                                   start_new_session=True)
        try:
            status = process.wait(timeout=180)
        except (subprocess.TimeoutExpired, KeyboardInterrupt):
            os.killpg(process.pid, signal.SIGKILL)
            process.wait()
            status = 124
        result = {"suite": suite, "exit_code": status,
                  "seconds": round(time.monotonic() - start, 3)}
        results.append(result)
        print(json.dumps({"suite_result": result}), flush=True)
        if status:
            break
    report = {"environment": environment, "suites": results,
              "ok": len(results) == len(SUITES) and all(r['exit_code'] == 0 for r in results)}
    pathlib.Path("result.json").write_text(json.dumps(report, indent=2) + "\n")
    return 0 if report['ok'] else 1


def port():
    with socket.socket() as sock:
        sock.bind(('127.0.0.1', 0))
        return sock.getsockname()[1]


def cross_host_server(binary, key, peer, codex):
    command = ['/bin/bash', str(binary), '--data-dir', str(pathlib.Path('lan-data').resolve())]
    cli = lambda *args: subprocess.check_output(command + list(args), text=True, timeout=15).strip()
    public = cli('identity')
    cli('trust', key, 'Rotation client')
    ui = port()
    while ui == peer:
        ui = port()
    daemon = subprocess.Popen(command + ['serve', '--listen', f'0.0.0.0:{peer}', '--ui', f'127.0.0.1:{ui}',
                                        '--codex', codex or str(pathlib.Path('tests/fake-codex.py').resolve())],
                              stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        lines = [daemon.stdout.readline() for _ in range(3)]
        assert 'Local controls:' in lines[-1], 'Linux LAN daemon did not start'
        # Only public connection information leaves this process; no control token.
        print(json.dumps({'key': public, 'port': peer, 'cwd': str(pathlib.Path.cwd())}), flush=True)
        for line in sys.stdin:
            request = json.loads(line)
            print(json.dumps(json.loads(cli('ping', request['address'], key, request['message']))), flush=True)
    finally:
        daemon.terminate()
        daemon.wait(timeout=10)
    return 0


def cross_host_check(ssh, remote, binary, peer_port, codex, model):
    """Direct authenticated LAN traffic; SSH only bootstraps and controls test processes."""
    host_ip = socket.gethostbyname(ssh[-1].rsplit('@', 1)[-1])
    with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as route:
        route.connect((host_ip, peer_port))
        local_ip = route.getsockname()[0]
    with tempfile.TemporaryDirectory(prefix='boxen-linux-lan-') as directory:
        command = ['/bin/bash', str(binary), '--data-dir', directory]
        cli = lambda *args: subprocess.check_output(command + list(args), text=True, timeout=30).strip()
        key = cli('identity')
        remote_command = 'cd ' + shlex.quote(remote) + ' && ' + shlex.join([
            'python3', 'scripts/test-linux.py', '--cross-host-server', '--binary', './boxen.com', '--trust-key', key, '--peer-port', str(peer_port)] + (['--codex', codex] if codex else []))
        server = subprocess.Popen(ssh + [remote_command], stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                  stderr=subprocess.PIPE, text=True)
        daemon = None
        try:
            peer = json.loads(server.stdout.readline())
            cli('trust', peer['key'], 'Linux rotation')
            local_peer, ui = port(), port()
            daemon = subprocess.Popen(command + ['serve', '--listen', f'{local_ip}:{local_peer}', '--ui', f'127.0.0.1:{ui}'],
                                      stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            lines = [daemon.stdout.readline() for _ in range(3)]
            assert 'Local controls:' in lines[-1], 'Local LAN daemon did not start'
            address = f"{host_ip}:{peer['port']}"
            marker = 'linux-rotation-' + os.urandom(8).hex()
            assert json.loads(cli('ping', address, peer['key'], marker))['message'] == marker
            server.stdin.write(json.dumps({'address': f'{local_ip}:{local_peer}', 'message': marker}) + '\n')
            server.stdin.flush()
            assert json.loads(server.stdout.readline())['message'] == marker
            prompt = (f'Please run the shell command `printf {marker}; sleep 1; printf {marker} > proof.txt`, then reply with the exact marker.' if codex else 'SLOW')
            run = json.loads(cli('start', address, peer['key'], peer['cwd'], model if codex else 'fixture', prompt))
            ident = run['record']['spec']['id']
            watch = subprocess.Popen(command + ['watch', address, peer['key'], ident, '--once'],
                                     stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            events, first_output, first_command = [], None, None
            try:
                for line in watch.stdout:
                    event = json.loads(line)
                    events.append(event)
                    if first_command is None and event['data'].get('method') == 'item/started' and event['data'].get('params',{}).get('item',{}).get('type')=='commandExecution':
                        first_command = time.monotonic()
                    if first_output is None and event['data'].get('method') == 'item/commandExecution/outputDelta':
                        first_output = time.monotonic()
                assert watch.wait(timeout=10) == 0, watch.stderr.read()
                terminal = [e['data'] for e in events if e['kind']=='status']
                assert terminal and terminal[-1]['status']=='completed', terminal
                observed = first_command if codex else first_output
                assert observed is not None and time.monotonic() - observed > .5, 'LAN tool activity was not observed before completion'
            finally:
                if watch.poll() is None:
                    watch.terminate()
                    watch.wait(timeout=10)
            if not codex:
                assert any(e['data'].get('method') == 'item/commandExecution/outputDelta' for e in events)
            assert any(e['kind'] == 'status' and e['data']['status'] == 'completed' for e in events)
            assert [e['seq'] for e in events] == list(range(1, len(events) + 1))
            replay = [json.loads(line) for line in cli('watch', address, peer['key'], ident, '--once').splitlines()]
            assert replay == events
            if codex:
                effect = subprocess.check_output(ssh + ['cat ' + shlex.quote(remote + '/proof.txt')], text=True, timeout=15)
                assert effect == marker
                assert marker in json.loads(cli('thread-result', address, peer['key'], ident))['response']
                follow = json.loads(cli('start', address, peer['key'], peer['cwd'], model,
                    'What exact marker did the earlier command write? Please answer from the conversation without tools.', '--thread', run['record']['spec']['thread_id']))
                more = [json.loads(line) for line in cli('watch', address, peer['key'], follow['record']['spec']['id'], '--once').splitlines()]
                assert marker in json.loads(cli('thread-result', address, peer['key'], follow['record']['spec']['id']))['response']
                original = next(e['data']['id'] for e in events if e['kind']=='backend_thread')
                resumed = next(e['data']['id'] for e in more if e['kind']=='backend_thread')
                assert original == resumed and not any(e['kind']=='backend_session_rebuilt' for e in more)
            return {'ok': True, 'transport': 'direct_tcp', 'direct_lan_verified': True, 'linux_address': address, 'local_address': f'{local_ip}:{local_peer}', 'bidirectional_ping': True, 'linux_workload': 'real Codex app-server' if codex else 'controlled Codex fixture', 'real_shell_effect_and_resume': bool(codex),
                    'events': len(events), 'streamed_before_completion': True, 'command_output_deltas': first_output is not None, 'gap_free_replay': True}
        finally:
            server.stdin.close()
            try:
                assert server.wait(timeout=15) == 0, server.stderr.read()
            finally:
                if daemon:
                    daemon.terminate()
                    daemon.wait(timeout=10)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('host', nargs='?', default='chad@resident-host.lan')
    parser.add_argument('--binary', type=pathlib.Path, default=pathlib.Path('target/cosmo/release/boxen.com'))
    parser.add_argument('--codex', help='Optional existing Linux Codex executable for a real authenticated LAN workload')
    parser.add_argument('--codex-model', default='-', help='Model for optional live Codex check (default runtime choice)')
    parser.add_argument('--peer-port', type=int, default=42426, help='Linux direct test listener port (default 42426)')
    parser.add_argument('--output', type=pathlib.Path, help='Local report directory (default work/linux/TIMESTAMP)')
    parser.add_argument('--cross-host-server', action='store_true', help=argparse.SUPPRESS)
    parser.add_argument('--trust-key', help=argparse.SUPPRESS)
    parser.add_argument('--worker', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if not 1024 <= args.peer_port <= 65535:
        parser.error('--peer-port is in 1024–65535')
    if args.cross_host_server:
        return cross_host_server(args.binary.resolve(), args.trust_key, args.peer_port, args.codex)
    if args.worker:
        return worker(args.binary.resolve())
    root = pathlib.Path(__file__).resolve().parent.parent
    binary = args.binary.resolve()
    if not binary.is_file() or binary.suffix != '.com':
        parser.error('--binary is a built portable .com executable')
    # Probe tests are part of this rotation rather than silently skipped.
    help_text = subprocess.check_output(['/bin/bash', str(binary), '--help'], text=True, timeout=10)
    if 'inference-probe' not in help_text:
        parser.error('this rotation needs an APE built with --features ape,inference-probe')
    if args.host.startswith('-'):
        parser.error('host cannot start with an option prefix')
    output = args.output or root / 'work/linux' / datetime.datetime.now(datetime.timezone.utc).strftime('%Y%m%dT%H%M%SZ')
    output.mkdir(parents=True, exist_ok=True)
    ssh = ['ssh', '-o', 'BatchMode=yes', '-o', 'ConnectTimeout=10', args.host]
    remote = subprocess.check_output(ssh + ['umask 077; mktemp -d /tmp/boxen-linux-XXXXXXXX'], text=True, timeout=20).strip()
    if not re.fullmatch(r'/tmp/boxen-linux-[A-Za-z0-9]+', remote):
        raise RuntimeError('unexpected remote temporary directory')
    digest = hashlib.sha256(binary.read_bytes()).hexdigest()
    report = {"host": args.host, "binary_sha256": digest, "ok": False}
    try:
        with tempfile.TemporaryFile() as bundle:
            with tarfile.open(fileobj=bundle, mode='w') as archive:
                archive.add(binary, arcname='boxen.com')
                archive.add(pathlib.Path(__file__).resolve(), arcname='scripts/test-linux.py')
                for suite in SUITES:
                    name = 'smoke.py' if suite == 'smoke' else suite + '-smoke.py'
                    archive.add(root / 'scripts' / name, arcname='scripts/' + name)
                archive.add(root / 'tests/fake-codex.py', arcname='tests/fake-codex.py')
            bundle.seek(0)
            subprocess.run(ssh + ['tar -xf - -C ' + shlex.quote(remote)], stdin=bundle, check=True, timeout=60)
        cmd = 'cd ' + shlex.quote(remote) + ' && python3 scripts/test-linux.py --worker --binary ./boxen.com'
        with (output / 'suites.log').open('w') as log:
            process = subprocess.Popen(ssh + [cmd], stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
            for line in process.stdout:
                print(line, end='', flush=True)
                log.write(line)
                log.flush()
            status = process.wait()
        raw = subprocess.check_output(ssh + ['cat ' + shlex.quote(remote + '/result.json')], text=True, timeout=20)
        report.update(json.loads(raw))
        if report['ok'] and status == 0:
            report['cross_host'] = cross_host_check(ssh, remote, binary, args.peer_port, args.codex, args.codex_model)
            print(json.dumps({'cross_host_result': report['cross_host']}), flush=True)
        report['ssh_exit_code'] = status
        report['ok'] = report['ok'] and status == 0 and report['environment']['binary_sha256'] == digest and report.get('cross_host', {}).get('ok', False)
    except BaseException as error:
        report['ok'] = False
        report['error'] = str(error)
        raise
    finally:
        cleanup = subprocess.run(ssh + ['rm -rf -- ' + shlex.quote(remote)], timeout=20)
        report['remote_cleanup_ok'] = cleanup.returncode == 0
        report['ok'] = report['ok'] and report['remote_cleanup_ok']
        (output / 'result.json').write_text(json.dumps(report, indent=2) + '\n')
        print('Linux report: ' + str(output / 'result.json'), flush=True)
    return 0 if report['ok'] else 1


if __name__ == '__main__':
    sys.exit(main())
