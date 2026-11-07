"""Run an unchanged evaluator and reap its complete child tree on exit (Linux)."""
import ctypes
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time

# Linux PR_SET_CHILD_SUBREAPER: orphaned grandchildren return to this process.
libc = ctypes.CDLL(None, use_errno=True)
if libc.prctl(36, 1, 0, 0, 0) != 0:
    raise OSError(ctypes.get_errno(), "cannot supervise evaluator descendants")

stopping = False


def request_stop(_signal, _frame):
    global stopping
    stopping = True


signal.signal(signal.SIGTERM, request_stop)
child = subprocess.Popen([sys.executable, *sys.argv[2:]])
while child.poll() is None and not stopping:
    time.sleep(0.005)


def descendants(parent):
    try:
        children = Path(f"/proc/{parent}/task/{parent}/children").read_text().split()
    except (FileNotFoundError, ProcessLookupError):
        return []
    result = []
    for raw in children:
        pid = int(raw)
        result.extend(descendants(pid))
        result.append(pid)
    return result


def kill_descendants():
    for pid in descendants(os.getpid()):
        if pid > 1:
            try:
                os.kill(pid, signal.SIGKILL)
            except ProcessLookupError:
                pass


kill_descendants()
returncode = child.wait()
# Adopted grandchildren may be reparented while the direct child is reaped.
while True:
    kill_descendants()
    try:
        pid, _status = os.waitpid(-1, os.WNOHANG)
    except ChildProcessError:
        break
    if pid == 0:
        time.sleep(0.005)
Path(sys.argv[1]).write_text(json.dumps({"returncode": returncode}))
sys.exit(returncode if returncode >= 0 else 128 - returncode)
