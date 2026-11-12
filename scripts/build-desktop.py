#!/usr/bin/env python3
"""Build the native client separately from the portable daemon; bundle on macOS."""
import argparse
import pathlib
import plistlib
import shutil
import subprocess
import sys
from build_environment import build_executable

root = pathlib.Path(__file__).resolve().parent.parent
parser = argparse.ArgumentParser(description=__doc__)
if sys.platform == 'darwin':
    parser.add_argument('--bundle', type=pathlib.Path, default=root / 'work/Boxen.app')
args = parser.parse_args()
binary = build_executable(root, root / 'desktop', 'boxen-desktop')
if sys.platform == 'darwin':
    app = args.bundle
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
