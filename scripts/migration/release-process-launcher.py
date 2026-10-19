#!/usr/bin/python3
"""Exec-only private-session launcher; optional outer subreaper owns orphans.

No application code or package is modified. All configuration is private to a run.
"""
import ctypes
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import time


def supervise(arguments):
    # The outer helper is excluded from app measurements. It lets normal app
    # shutdown reap Chromium/WebKit orphans instead of leaving zombies at PID 1.
    if ctypes.CDLL(None, use_errno=True).prctl(36, 1, 0, 0, 0) != 0:
        raise OSError(ctypes.get_errno(), "PR_SET_CHILD_SUBREAPER failed")
    began = time.monotonic()
    evidence = {"supervisorPid": os.getpid(), "subreaper": True,
                "reaped": [], "killedAdoptedChildren": [], "complete": False}
    env = dict(os.environ, FREESHOW_COMPARISON_SUPERVISED="1")
    process = subprocess.Popen(arguments, env=env)
    interrupted = False

    def forward(sig, _frame):
        nonlocal interrupted, interrupted_at
        interrupted = True
        interrupted_at = interrupted_at or time.monotonic()
        try:
            process.send_signal(sig)
        except ProcessLookupError:
            pass

    signal.signal(signal.SIGINT, forward)
    signal.signal(signal.SIGTERM, forward)
    result = None
    deadline = None
    final_deadline = None
    interrupted_at = None
    while True:
        if interrupted_at and time.monotonic() - interrupted_at > 5 and result is None:
            process.kill()
        if final_deadline and time.monotonic() > final_deadline:
            print("Comparison supervisor: adopted children did not exit within cleanup bound", file=sys.stderr)
            result = 124
            break
        try:
            pid, status = os.waitpid(-1, os.WNOHANG)
        except ChildProcessError:
            evidence["complete"] = True
            break
        if pid:
            evidence["reaped"].append({"pid": pid, "exitCode": os.waitstatus_to_exitcode(status)})
        if pid == process.pid:
            result = os.waitstatus_to_exitcode(status)
            process.returncode = result
            deadline = time.monotonic() + 1
            final_deadline = time.monotonic() + 10
        if pid == 0:
            if deadline is not None and time.monotonic() > deadline:
                # Do not kill unrelated user processes. Remaining adopted
                # children are necessarily this comparison's descendants.
                children = Path(f"/proc/{os.getpid()}/task/{os.getpid()}/children")
                for child in children.read_text().split():
                    try:
                        os.kill(int(child), signal.SIGKILL)
                        evidence["killedAdoptedChildren"].append(int(child))
                    except ProcessLookupError:
                        pass
                deadline = time.monotonic() + 2
            time.sleep(0.025)
    result = (128 + signal.SIGINT) if interrupted and result is None else (result or 0)
    if result < 0:
        result = 128 - result
    evidence.update({"exitCode": result, "elapsedSeconds": time.monotonic() - began})
    children = Path(f"/proc/{os.getpid()}/task/{os.getpid()}/children")
    evidence["remainingAdoptedChildren"] = [int(pid) for pid in children.read_text().split()]
    receipt = os.environ.get("FREESHOW_COMPARISON_SUPERVISOR_RECEIPT")
    if receipt and Path(receipt).parent.is_dir():
        Path(receipt).write_text(json.dumps(evidence, indent=2) + "\n")
    return result


def launch():
    config = json.loads(Path(os.environ["FREESHOW_COMPARISON_LAUNCH_CONFIG"]).read_text())
    # Playwright already launches detached; WebKitWebDriver normally does not.
    if os.getsid(0) != os.getpid():
        os.setsid()
    if config.get("cgroup"):
        Path(config["cgroup"], "cgroup.procs").write_text(str(os.getpid()))
    stat = Path("/proc/self/stat").read_text()
    fields = stat[stat.rfind(")") + 1:].split()
    receipt = {"pid": os.getpid(), "pgid": os.getpgrp(), "sid": os.getsid(0),
               "startTicks": int(fields[19]), "executable": config["binary"]}
    receipt["identity"] = f'{receipt["pid"]}:{receipt["startTicks"]}'
    location = Path(config["receipt"])
    temporary = location.with_suffix(".tmp")
    temporary.write_text(json.dumps(receipt))
    temporary.replace(location)
    env = dict(os.environ, **config["env"])
    env["FREESHOW_COMPARISON_RUN_ID"] = config["runId"]
    os.execve(config["binary"], [config["binary"], *sys.argv[1:]], env)


if __name__ == "__main__":
    if len(sys.argv) > 1 and sys.argv[1] == "--supervise":
        sys.exit(supervise(sys.argv[2:]))
    launch()
