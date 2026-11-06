#!/usr/bin/env python3
"""Build the optional work-item service with source/dependency paths remapped."""
import pathlib
import subprocess
from build_environment import environment

root = pathlib.Path(__file__).resolve().parent.parent
subprocess.run(['cargo', 'build', '--locked', '--release', '--manifest-path', str(root/'addons/work-items/Cargo.toml')], env=environment(root), check=True)
