"""Build and replace the local Boxen executables while the operator has them stopped."""
import argparse
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import uuid

from build_environment import build_executable, environment


ROOT = Path(__file__).resolve().parent.parent


def destinations(platform):
    if platform == 'darwin':
        return Path('/usr/local/libexec/boxen/boxen'), Path('/Applications/Boxen.app')
    if platform == 'win32':
        program_files = os.environ.get('ProgramW6432') or os.environ['ProgramFiles']
        directory = Path(program_files) / 'Boxen'
        return directory / 'boxen.exe', directory / 'boxen-desktop.exe'
    directory = Path.home() / '.local/bin'
    return directory / 'boxen.com', directory / 'boxen-desktop'


def build(platform, output, desktop_name):
    if platform == 'linux':
        subprocess.run(['bash', 'scripts/build-ape.sh'], cwd=ROOT,
                       env={**environment(ROOT), 'CARGO_TARGET_DIR': str(ROOT / 'target')}, check=True)
        daemon = ROOT / 'target/cosmo/release/boxen.com'
        subprocess.run(['bash', str(daemon), 'platform-check'], check=True, timeout=10)
    else:
        daemon = build_executable(ROOT, ROOT, 'boxen', cwd=ROOT)
        subprocess.run([str(daemon), 'platform-check'], check=True, timeout=10)

    command = [sys.executable, str(ROOT / 'scripts/build-desktop.py')]
    if platform == 'darwin':
        desktop = output / desktop_name
        subprocess.run(command + ['--bundle', str(desktop)], cwd=ROOT, check=True)
        subprocess.run(['/usr/bin/codesign', '--verify', '--deep', '--strict', str(desktop)], check=True)
    else:
        desktop = Path(subprocess.check_output(
            command, cwd=ROOT, encoding='utf-8',
            env={**os.environ, 'PYTHONIOENCODING': 'utf-8'},
        ).rstrip('\r\n'))
    return daemon, desktop


class Replacement:
    def __init__(self, destination, elevated):
        self.destination = destination
        self.elevated = elevated
        if elevated:
            self.stage = Path(subprocess.check_output(
                ['sudo', '/usr/bin/mktemp', '-d', str(destination.parent / '.boxen-upgrade.XXXXXXXX')],
                text=True).strip())
        elif sys.platform == 'win32':
            # mkdtemp uses mode 0700, which gives the directory a private ACL on
            # modern Python. A same-volume rename retains the staged file's ACL.
            self.stage = destination.parent / f'.boxen-upgrade.{uuid.uuid4().hex}'
            self.stage.mkdir()
        else:
            self.stage = Path(tempfile.mkdtemp(prefix='.boxen-upgrade.', dir=destination.parent))
        self.next = self.stage / destination.name
        self.previous = self.stage / f'previous-{destination.name}'
        self.backed_up = False
        self.installed = False

    def copy(self, source):
        if self.elevated and source.is_dir():
            subprocess.run(['sudo', '/usr/bin/ditto', str(source), str(self.next)], check=True)
        elif self.elevated:
            subprocess.run(['sudo', '/usr/bin/install', '-m', '0755', str(source), str(self.next)], check=True)
        else:
            shutil.copy2(source, self.next)

    def move(self, source, destination):
        if self.elevated:
            subprocess.run(['sudo', '/bin/mv', str(source), str(destination)], check=True)
        else:
            source.rename(destination)

    def install(self):
        if self.destination.exists() or self.destination.is_symlink():
            self.move(self.destination, self.previous)
            self.backed_up = True
        self.move(self.next, self.destination)
        self.installed = True

    def restore(self):
        if self.installed:
            self.move(self.destination, self.next)
        if self.backed_up:
            self.move(self.previous, self.destination)

    def clean(self):
        if self.elevated:
            subprocess.run(['sudo', '/bin/rm', '-rf', str(self.stage)], check=True)
        else:
            shutil.rmtree(self.stage)


def replace_all(sources, targets, elevated=False):
    replacements = []
    retained = set()
    try:
        for source, destination in zip(sources, targets):
            replacement = Replacement(destination, elevated)
            replacements.append(replacement)
            replacement.copy(source)
        for replacement in replacements:
            replacement.install()
    except KeyboardInterrupt:
        # An interrupted external rename may have succeeded before returning.
        # Preserve both staged and backup files for the operator to inspect.
        for replacement in replacements:
            retained.add(replacement)
            print(f'Check {replacement.destination}. Recovery files are in {replacement.stage}.',
                  file=sys.stderr)
        raise
    except Exception:
        for replacement in reversed(replacements):
            try:
                replacement.restore()
            except (OSError, subprocess.SubprocessError) as error:
                retained.add(replacement)
                print(f'Could not restore {replacement.destination}: {error}. '
                      f'Recovery files are in {replacement.stage}.', file=sys.stderr)
        raise
    finally:
        for replacement in replacements:
            if replacement not in retained:
                try:
                    replacement.clean()
                except (OSError, subprocess.SubprocessError) as error:
                    print(f'Could not clean {replacement.stage}: {error}', file=sys.stderr)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('platform', choices=['darwin', 'linux', 'win32'], help=argparse.SUPPRESS)
    platform = parser.parse_args().platform
    if platform != sys.platform:
        parser.error(f'This upgrade script requires {platform}; current platform is {sys.platform}.')
    targets = destinations(platform)
    for target in targets:
        if not target.parent.is_dir():
            parser.error(f'Expected install directory {target.parent}. See docs/local-upgrades.md.')
        if target.exists() and (target.is_dir() != (target.suffix == '.app')):
            parser.error(f'Unexpected install target type at {target}. See docs/local-upgrades.md.')
    # Lock the installation across builds as well as replacement. The operator
    # owns service and desktop lifecycle and can remove an abandoned empty lock.
    lock = targets[0].parent / '.boxen-upgrade-lock'
    elevated = platform == 'darwin'
    if elevated:
        subprocess.run(['sudo', '/bin/mkdir', str(lock)], check=True)
    else:
        lock.mkdir()
    try:
        with tempfile.TemporaryDirectory(prefix='boxen-build-') as directory:
            sources = build(platform, Path(directory), targets[1].name)
            replace_all(sources, targets, elevated)
    finally:
        if elevated:
            subprocess.run(['sudo', '/bin/rmdir', str(lock)], check=True)
        else:
            lock.rmdir()
    for target in targets:
        print(f'Updated {target}')


if __name__ == '__main__':
    try:
        main()
    except (OSError, subprocess.SubprocessError, KeyboardInterrupt) as error:
        sys.exit(f'Boxen upgrade failed: {error or "interrupted"}')
