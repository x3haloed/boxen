#!/usr/bin/env python3
"""Check actual blocking primitives and idle CPU in the shipped executable."""
import json
import pathlib
import platform
import resource
import subprocess
import sys
import time

binary = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/boxen").resolve()
command = (["/bin/bash"] if binary.suffix == ".com" else []) + [str(binary)]
before = resource.getrusage(resource.RUSAGE_CHILDREN)
start = time.monotonic()
result = subprocess.run(command + ["platform-check"], capture_output=True, text=True, check=True, timeout=10)
wall = time.monotonic() - start
after = resource.getrusage(resource.RUSAGE_CHILDREN)
cpu = after.ru_utime + after.ru_stime - before.ru_utime - before.ru_stime
record = json.loads(result.stdout)
assert record["ok"]
assert record["os"] == {"Darwin":"macos","Linux":"linux","Windows":"windows"}.get(platform.system(),platform.system().lower())
assert record["condition_variable_ms"] >= 150
assert record["channel_timeout_ms"] >= 150
assert record["condition_variable_wakes"] < 100
assert record["notification_wakes"] < 100
assert cpu < .1, f"blocking checks consumed {cpu:.3f}s CPU during {wall:.3f}s wall time"
print(json.dumps({**record, "cpu_seconds": round(cpu, 4), "wall_seconds": round(wall, 4)}))
