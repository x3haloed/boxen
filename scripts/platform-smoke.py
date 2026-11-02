#!/usr/bin/env python3
"""Check actual blocking primitives and idle CPU in the shipped executable."""
import json
import pathlib
import platform
import os
if os.name!='nt':
    import resource
import subprocess
import sys
import time

binary = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else "target/release/boxen").resolve()
command = (["/bin/bash"] if binary.suffix == ".com" else []) + [str(binary)]
before = resource.getrusage(resource.RUSAGE_CHILDREN) if os.name!='nt' else None
start = time.monotonic()
if os.name=='nt':
    import ctypes
    from ctypes import wintypes
    process=subprocess.Popen(command+["platform-check"],stdout=subprocess.PIPE,stderr=subprocess.PIPE,text=True)
    stdout,stderr=process.communicate(timeout=10)
    if process.returncode:raise subprocess.CalledProcessError(process.returncode,process.args,stdout,stderr)
    result=subprocess.CompletedProcess(process.args,process.returncode,stdout,stderr)
    times=[wintypes.FILETIME() for _ in range(4)]
    get_times=ctypes.windll.kernel32.GetProcessTimes
    get_times.argtypes=[wintypes.HANDLE]+[ctypes.POINTER(wintypes.FILETIME)]*4
    get_times.restype=wintypes.BOOL
    if not get_times(wintypes.HANDLE(int(process._handle)),*[ctypes.byref(t) for t in times]):raise ctypes.WinError()
    cpu=sum((t.dwHighDateTime<<32)|t.dwLowDateTime for t in times[2:])/10000000
else:
    result = subprocess.run(command + ["platform-check"], capture_output=True, text=True, check=True, timeout=10)
    after = resource.getrusage(resource.RUSAGE_CHILDREN)
    cpu = after.ru_utime + after.ru_stime - before.ru_utime - before.ru_stime
wall = time.monotonic() - start
record = json.loads(result.stdout)
assert record["ok"]
assert record["os"] == {"Darwin":"macos","Linux":"linux","Windows":"windows"}.get(platform.system(),platform.system().lower())
assert record["condition_variable_ms"] >= 150
assert record["channel_timeout_ms"] >= 150
assert record["condition_variable_wakes"] < 100
assert record["notification_wakes"] < 100
assert cpu < .1, f"blocking checks consumed {cpu:.3f}s CPU during {wall:.3f}s wall time"
print(json.dumps({**record, "cpu_seconds": round(cpu, 4), "wall_seconds": round(wall, 4)}))
