"""Host-native launch commands for controlled smoke-test fixtures."""
import os
import pathlib
import queue
import shlex
import subprocess
import sys
import tempfile
import threading

_wrappers = tempfile.TemporaryDirectory(prefix='boxen-smoke-launchers-')


def fake_codex():
    fixture = pathlib.Path(__file__).resolve().parent.parent / 'tests/fake-codex.py'
    if os.name != 'nt':
        fixture.chmod(0o755)
        return fixture
    wrapper = pathlib.Path(_wrappers.name) / 'fake-codex.cmd'
    wrapper.write_text(f'@echo off\n"{sys.executable}" "{fixture}" %*\n')
    return wrapper


def python_shell(source):
    """A Python effect executed through the daemon's actual native shell."""
    arguments = [sys.executable, '-c', source]
    return subprocess.list2cmdline(arguments) if os.name == 'nt' else shlex.join(arguments)


class PipeLines:
    """Bounded waits for subprocess lines; Windows select only accepts sockets."""
    def __init__(self, pipe):
        self.lines = queue.Queue()
        def read():
            while True:
                line = pipe.readline()
                if not line:
                    break
                self.lines.put(line)
            self.lines.put(None)
        threading.Thread(target=read, daemon=True).start()

    def get(self, timeout=10):
        try:
            line = self.lines.get(timeout=timeout)
        except queue.Empty as error:
            raise AssertionError('subprocess stopped producing lines') from error
        assert line is not None, 'subprocess stream ended early'
        return line


def windows_cpu(process):
    """Kernel plus user CPU seconds for an owned live subprocess."""
    import ctypes
    from ctypes import wintypes
    times = [wintypes.FILETIME() for _ in range(4)]
    get_times = ctypes.windll.kernel32.GetProcessTimes
    get_times.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(wintypes.FILETIME)] * 4
    get_times.restype = wintypes.BOOL
    if not get_times(wintypes.HANDLE(int(process._handle)), *[ctypes.byref(t) for t in times]):
        raise ctypes.WinError()
    return sum((t.dwHighDateTime << 32) | t.dwLowDateTime for t in times[2:]) / 10000000
