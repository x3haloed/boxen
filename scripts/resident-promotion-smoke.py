#!/usr/bin/env python3
"""Controlled native-preserving promotion through real daemons and a scoped MCP bridge."""
import os
import pathlib
import runpy
os.environ['BOXEN_PROMOTION_PROBE']='1'
runpy.run_path(str(pathlib.Path(__file__).with_name('thread-import-smoke.py')),run_name='__main__')
