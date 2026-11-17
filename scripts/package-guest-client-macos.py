#!/usr/bin/env python3
"""Build an ad hoc signed Apple Silicon Guest Chat disk image, without credentials."""
import argparse
import hashlib
import json
import os
import pathlib
import platform
import plistlib
import shutil
import subprocess
import sys
import tempfile

from build_environment import build_executable


def run(*args):
    return subprocess.check_output(args, text=True).strip()


def main():
    root = pathlib.Path(__file__).resolve().parent.parent
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=pathlib.Path, default=root / 'work/guest-client-release')
    args = parser.parse_args()
    if sys.platform != 'darwin' or platform.machine() != 'arm64':
        parser.error('This package requires an Apple Silicon macOS build host.')
    package = root / 'guest-client'
    metadata = json.loads(run('cargo', 'metadata', '--locked', '--format-version=1',
                              '--filter-platform', 'aarch64-apple-darwin',
                              '--manifest-path', str(package / 'Cargo.toml')))
    version = next(item['version'] for item in metadata['packages']
                   if item['name'] == 'boxen-guest-client')
    revision = run('git', '-C', str(root), 'rev-parse', '--short=12', 'HEAD')
    if run('git', '-C', str(root), 'status', '--porcelain', '--', 'guest-client',
           'scripts/build_environment.py', 'scripts/package-guest-client-macos.py'):
        revision += '-dirty'
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    name = f'Boxen-Guest-Chat-{version}-{revision}-macOS-arm64'
    image = output / f'{name}.dmg'
    if image.exists():
        parser.error(f'Package already exists: {image}')
    os.environ['MACOSX_DEPLOYMENT_TARGET'] = '12.0'
    binary = build_executable(root, package, 'boxen-guest-client')
    if run('lipo', '-archs', str(binary)) != 'arm64':
        raise RuntimeError('The Mac package requires an arm64 executable.')
    with tempfile.TemporaryDirectory(prefix='guest-package-', dir=output) as directory:
        stage = pathlib.Path(directory)
        contents = stage / 'Boxen Guest Chat.app/Contents'
        (contents / 'MacOS').mkdir(parents=True)
        licenses = contents / 'Resources/Licenses'
        licenses.mkdir(parents=True)
        executable = contents / 'MacOS/boxen-guest-client'
        shutil.copyfile(binary, executable)
        executable.chmod(0o755)
        (contents / 'Info.plist').write_bytes(plistlib.dumps({
            'CFBundleName': 'Boxen Guest Chat',
            'CFBundleDisplayName': 'Boxen Guest Chat',
            'CFBundleIdentifier': 'org.boxen.guest-client',
            'CFBundleExecutable': 'boxen-guest-client',
            'CFBundlePackageType': 'APPL',
            'CFBundleShortVersionString': version,
            'CFBundleVersion': version,
            'BoxenSourceRevision': revision,
            'LSMinimumSystemVersion': '12.0',
            'LSArchitecturePriority': ['arm64'],
            'NSHighResolutionCapable': True,
            'NSPrincipalClass': 'NSApplication',
        }))
        for source, name_in_bundle in [
            (root / 'LICENSE-MIT', 'Boxen-MIT.txt'),
            (root / 'LICENSE-APACHE', 'Boxen-Apache.txt'),
            (package / 'vendor/softer_gui/LICENSE', 'softer_gui-MIT.txt'),
            (package / 'assets/DM-Sans-OFL.txt', 'DM-Sans-OFL.txt'),
            (package / 'assets/Courier-Prime-OFL.txt', 'Courier-Prime-OFL.txt'),
        ]:
            shutil.copyfile(source, licenses / name_in_bundle)
        notices = []
        resolved = {node['id'] for node in metadata['resolve']['nodes']}
        for dependency in metadata['packages']:
            if dependency['id'] not in resolved or dependency['source'] is None:
                continue
            source = pathlib.Path(dependency['manifest_path']).parent
            label = f"{dependency['name']}-{dependency['version']}"
            destination = licenses / 'Dependencies' / label
            copied = []
            for entry in source.iterdir():
                if entry.name.upper().startswith(('LICENSE', 'LICENCE', 'COPYING', 'NOTICE')):
                    destination.mkdir(parents=True, exist_ok=True)
                    if entry.is_dir():
                        shutil.copytree(entry, destination / entry.name)
                    else:
                        shutil.copyfile(entry, destination / entry.name)
                    copied.append(entry.name)
            if not copied:
                raise RuntimeError(f'No dependency license notice found for {label}')
            notices.append({'name': dependency['name'], 'version': dependency['version'],
                            'license': dependency['license'], 'notices': sorted(copied)})
        (licenses / 'Dependencies.json').write_text(json.dumps(notices, indent=2) + '\n')
        shutil.copyfile(package / 'INSTALL-macOS.txt', stage / 'INSTALL.txt')
        (stage / 'Applications').symlink_to('/Applications')
        app = contents.parent
        run('codesign', '--force', '--sign', '-', str(app))
        run('codesign', '--verify', '--deep', '--strict', '--verbose=2', str(app))
        # Scan the final signed executable and resources, not just compiler flags.
        for file in app.rglob('*'):
            if file.is_file():
                data = file.read_bytes()
                for private in (str(pathlib.Path.home()), str(root), '/Users/', '/home/'):
                    if private.encode() in data:
                        raise RuntimeError(f'Private build path retained in {file.relative_to(app)}')
        libraries = run('otool', '-L', str(executable))
        for line in libraries.splitlines()[1:]:
            dependency = line.strip().split(' (', 1)[0]
            if not dependency.startswith(('/usr/lib/', '/System/Library/')):
                raise RuntimeError(f'Non-system library dependency: {dependency}')
        run('hdiutil', 'create', '-volname', 'Boxen Guest Chat', '-srcfolder', str(stage),
            '-format', 'UDZO', str(image))
    digest = hashlib.sha256(image.read_bytes()).hexdigest()
    image.with_suffix('.dmg.sha256').write_text(f'{digest}  {image.name}\n')
    report = {'artifact': image.name, 'sha256': digest, 'source_revision': revision,
              'version': version, 'architecture': 'arm64', 'minimum_macos': '12.0',
              'signing': 'ad-hoc', 'notarized': False, 'private_path_scan': 'passed',
              'dependencies': 'system libraries only'}
    image.with_suffix('.json').write_text(json.dumps(report, indent=2) + '\n')
    print(image)


if __name__ == '__main__':
    main()
