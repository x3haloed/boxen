#!/usr/bin/env python3
"""Build the native client separately from the portable daemon; bundle on macOS."""
import pathlib
import plistlib
import shutil
import subprocess
import sys
from build_environment import environment

root = pathlib.Path(__file__).resolve().parent.parent
subprocess.run(['cargo', 'build', '--locked', '--release', '--manifest-path', str(root / 'desktop/Cargo.toml')], env=environment(root), check=True)
binary = root / 'desktop/target/release' / ('boxen-desktop.exe' if sys.platform == 'win32' else 'boxen-desktop')
if sys.platform == 'darwin':
    app = root / 'work/Boxen.app'
    contents = app / 'Contents'
    (contents / 'MacOS').mkdir(parents=True, exist_ok=True)
    shutil.copy2(binary, contents / 'MacOS/boxen-desktop')
    (contents / 'Info.plist').write_bytes(plistlib.dumps({
        'CFBundleName': 'Boxen', 'CFBundleDisplayName': 'Boxen',
        'CFBundleIdentifier': 'dev.boxen.desktop', 'CFBundleExecutable': 'boxen-desktop',
        'CFBundlePackageType': 'APPL', 'CFBundleShortVersionString': '0.1.0',
        'LSMinimumSystemVersion': '12.0', 'NSHighResolutionCapable': True,
    }))
    subprocess.run(['codesign', '--force', '--deep', '--sign', '-', str(app)], check=True)
    print(app)
else:
    print(binary)
