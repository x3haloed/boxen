"""Build native executables with source and dependency path remapping."""
import json
import os
import pathlib
import subprocess
import sys


def environment(root):
    env = os.environ.copy()
    flags = env.get('CARGO_ENCODED_RUSTFLAGS', '').split('\x1f') if env.get('CARGO_ENCODED_RUSTFLAGS') else env.get('RUSTFLAGS', '').split()
    roots = [(pathlib.Path.home(), '/user'),
             (pathlib.Path(env.get('CARGO_HOME', pathlib.Path.home()/'.cargo')), '/cargo'),
             (root.resolve(), '/boxen')]
    for source, replacement in roots:
        for spelling in dict.fromkeys([str(source), source.as_posix()]):
            flags.append(f'--remap-path-prefix={spelling}={replacement}')
    if sys.platform == 'win32':
        flags.append('-Clink-arg=/PDBALTPATH:boxen.pdb')
    env.pop('RUSTFLAGS', None)
    env['CARGO_ENCODED_RUSTFLAGS'] = '\x1f'.join(flags)
    return env


def build_executable(root, package, binary, *, cwd=None):
    manifest = package / 'Cargo.toml'
    output = subprocess.check_output([
        'cargo', 'build', '--locked', '--release', '--bin', binary,
        '--target-dir', str(package / 'target'), '--manifest-path', str(manifest),
        '--message-format=json,json-render-diagnostics',
    ], cwd=cwd, env=environment(root), encoding='utf-8')
    executables = set()
    for line in output.splitlines():
        if not line.startswith('{'):
            continue
        message = json.loads(line)
        if (message.get('reason') == 'compiler-artifact' and message.get('executable')
                and message['target']['name'] == binary
                and pathlib.Path(message['manifest_path']).resolve() == manifest.resolve()):
            executables.add(pathlib.Path(message['executable']))
    if len(executables) != 1:
        raise OSError(f'Expected one Cargo executable for {binary}, found {len(executables)}.')
    return executables.pop()
