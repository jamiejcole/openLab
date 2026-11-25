#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Runs a command that may use a lot of memory, and kills it before the machine runs out.

It counts resident memory of the command's process group only: a process that leaves the group is
not counted or killed, and compressed pages are seen only through the machine-wide readings.

    scripts/mem-capped.py [--gb N] -- command args...

The command and everything in its process group are killed when together they pass N GB (4), when
macOS reports critical memory pressure or little memory left, when memory cannot be read, or when
this script dies. One capped run at a time on the machine: a second is refused, not queued.
Exit codes: the command's own, 86 killed for memory, 87 refused.
"""
import argparse, fcntl, os, signal, subprocess, sys, time

KILLED, REFUSED = 86, 87
LOCK = "/tmp/openlab-mem-capped.lock"
MIN_FREE_PERCENT = 15  # kern.memorystatus_level, below which a run is refused or killed


def out(*cmd):
    return subprocess.run(cmd, capture_output=True, text=True, timeout=5, check=True).stdout


def machine_trouble():
    """Why the machine is short of memory, or None. Other systems have no such reading."""
    if sys.platform != "darwin":
        return None
    if int(out("sysctl", "-n", "kern.memorystatus_vm_pressure_level")) >= 4:
        return "macOS reports critical memory pressure"
    free = int(out("sysctl", "-n", "kern.memorystatus_level"))
    return f"only {free}% of memory is free" if free < MIN_FREE_PERCENT else None


def group_kb(pgid):
    rows = (line.split() for line in out("ps", "-axo", "pgid=,rss=").splitlines())
    return sum(int(rss) for g, rss in rows if int(g) == pgid)


def kill_group(pgid):
    try:
        os.killpg(pgid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass


def watch(pgid, cap_kb, parent, note_fd):
    """The watcher, a process of its own: it outlives this script being killed."""
    while True:
        try:
            why = machine_trouble()
            used = group_kb(pgid)
            if why is None and used > cap_kb:
                why = f"it used {used / 2**20:.1f} GB (the cap is {cap_kb / 2**20:.1f})"
        except Exception as e:  # a run that cannot be watched does not go on
            why = f"memory could not be read ({e})"
        if os.getppid() != parent:
            why = "the script running it died"
        if why:
            kill_group(pgid)
            try:
                os.write(note_fd, why.encode())
            except BrokenPipeError:
                pass  # the script died: nobody is left to tell
            return
        time.sleep(0.25)


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--gb", type=float, default=4)
    p.add_argument("command", nargs=argparse.REMAINDER)
    a = p.parse_args()
    command = a.command[1:] if a.command[:1] == ["--"] else a.command
    if not command or not 0 < a.gb <= 6:
        p.error("give a command, and a cap over 0 and at most 6 GB")
    lock = open(LOCK, "a")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        print("mem-capped: another capped run is going; not starting a second", file=sys.stderr)
        return REFUSED
    why = machine_trouble()
    if why:
        print(f"mem-capped: not started: {why}", file=sys.stderr)
        return REFUSED
    # The watcher holds the lock too, so it stays taken until the command is gone.
    child = subprocess.Popen(command, start_new_session=True)
    read_fd, write_fd = os.pipe()
    me = os.getpid()
    watcher = os.fork()
    if watcher == 0:
        os.setsid()
        os.close(read_fd)
        watch(child.pid, int(a.gb * 2**20), me, write_fd)
        os._exit(0)
    os.close(write_fd)
    for s in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):
        signal.signal(s, lambda n, _: (kill_group(child.pid), sys.exit(128 + n)))
    try:
        code = child.wait()
    finally:
        kill_group(child.pid)  # nothing it started in its group stays behind
        os.kill(watcher, signal.SIGKILL)
        os.waitpid(watcher, 0)
    note = os.read(read_fd, 400).decode()
    if note:
        print(f"mem-capped: killed: {note}", file=sys.stderr)
        return KILLED
    return code if code >= 0 else 128 - code


if __name__ == "__main__":
    sys.exit(main())
