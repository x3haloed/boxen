"""Source/dependency path remapping for distributable native client artifacts."""
import os
import pathlib
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
