#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
"""Runs a command that may use a lot of memory, and kills it before the machine runs out.

  scripts/mem-capped.py [--gb N] -- command args...     N is at most CAP_GB, the default
  scripts/mem-capped.py self-test

Counted and killed together: the command, everything it starts, what those start, and every process of a
session one of them leads: so every process group, and also one that has left the group or the session, or
whose parent has gone. Counted is each process's memory footprint, which includes compressed and swapped-out
pages. The run is killed when the total passes the cap, when macOS reports memory pressure at warning or
above, when memory cannot be read, and when this script or its watcher dies. Both of them check, every POLL
seconds, so one that is stopped or killed leaves the other. One capped run at a time on the machine: a
second is refused.

Exit code: the command's own; 86 killed, for any of those reasons; 87 not started. A `mem-capped:` line says
why. A script that is killed itself has the exit code of that; its watcher's line says why the command went.

What can still get past it:
  - memory allocated between two looks, before the kill takes effect;
  - a daemon in a session of its own whose parent was started and ended between two looks (on Linux, where a
    process does not keep its first parent's identity, any that lost its parent between two looks);
  - work another process does for the command: apps and test hosts launchd starts (xcodebuild's among
    them), XPC services;
  - a compile server the command calls itself (`sccache --start-server`, or a wrapper it sets again): cargo's
    rustc wrapper is off in a capped run, so a plain cargo build has none. Such a server's rustc is not counted;
    one the run started detaches, and keeps the lock until it ends unless the end of the run killed it;
  - a process of another user (setuid): it cannot be read or killed;
  - memory no process footprint includes: the file cache, written pages of mapped files, kernel memory;
  - this script and its watcher both killed with SIGKILL: the command goes on with no cap. It holds the lock,
    so no second run joins it, unless it closed the descriptors it inherited or the lock file was deleted.
Only memory is capped, not processor time, disk or the GPU; and only this run, not the rest of the machine,
which is what the pressure reading is for. Linux has no pressure reading here.
"""
import ctypes
import errno
import fcntl
import mmap
import os
import select
import signal
import sys
import tempfile
import time

CAP_GB = 4
KILLED, REFUSED = 86, 87
LOCK = "/tmp/openlab-mem-capped.lock"
POLL = 0.05
PRESSURE_WARNING = 2  # kern.memorystatus_vm_pressure_level: 1 normal, 2 warning, 4 critical
START_LIMIT = 10  # seconds for a helper to answer before the command starts
STOP_LIMIT = 1  # seconds in which processes are stopped before any is killed
KILL_LIMIT = 5  # seconds to go on killing a process that does not die
# Cargo starts rustc itself, whatever its config file or the caller names as a wrapper: a shared sccache server's
# rustc would not be counted, and a server the run started would be killed with it or keep its lock.
COMMAND_ENV = {"RUSTC_WRAPPER": "", "CARGO_BUILD_RUSTC_WRAPPER": ""}
STOP_SIGNALS = (signal.SIGTERM, signal.SIGINT, signal.SIGHUP, signal.SIGQUIT)


class Refused(Exception):
    pass


# --- What the system says about its processes. processes() lists the living ones (no zombies, not 0 and 1) as
# {pid: (identity, parent's identity, session id)}; an identity is never used again, a pid is. footprint() is
# a process's bytes; it raises ProcessLookupError for one that has gone, another OSError if it may not be read.

if sys.platform == "darwin":
    libc = ctypes.CDLL(None, use_errno=True)
    PROC_ALL_PIDS, PROC_PIDUNIQIDENTIFIERINFO, RUSAGE_INFO_V0 = 1, 17, 0

    class UniqueIds(ctypes.Structure):  # the parent's stays the first parent's when launchd takes the process over
        _fields_ = [("uuid", ctypes.c_uint8 * 16), ("own", ctypes.c_uint64), ("parent", ctypes.c_uint64),
                    ("versions", ctypes.c_int32 * 2), ("reserved", ctypes.c_uint64 * 2)]

    class RusageInfo(ctypes.Structure):
        _fields_ = [("uuid", ctypes.c_uint8 * 16)]
        _fields_ += [(n, ctypes.c_uint64) for n in (
            "user_time", "system_time", "pkg_idle_wkups", "interrupt_wkups", "pageins", "wired_size",
            "resident_size", "phys_footprint", "proc_start_abstime", "proc_exit_abstime")]

    def failed(what):
        e = ctypes.get_errno()
        return OSError(e, f"{what}: {os.strerror(e)}")  # ESRCH makes it a ProcessLookupError

    def processes():
        size = libc.proc_listpids(PROC_ALL_PIDS, 0, None, 0)
        while True:
            pids = (ctypes.c_int * (size // ctypes.sizeof(ctypes.c_int) + 64))()
            got = libc.proc_listpids(PROC_ALL_PIDS, 0, pids, ctypes.sizeof(pids))
            if got <= 0:
                raise failed("proc_listpids")
            if got < ctypes.sizeof(pids):
                break
            size = got * 2
        table, ids = {}, UniqueIds()
        for pid in pids[:got // ctypes.sizeof(ctypes.c_int)]:
            if pid <= 1:
                continue
            size = libc.proc_pidinfo(pid, PROC_PIDUNIQIDENTIFIERINFO, ctypes.c_uint64(0), ctypes.byref(ids),
                                     ctypes.sizeof(ids))
            if size != ctypes.sizeof(ids):
                continue  # gone, or a zombie
            try:
                table[pid] = (ids.own, ids.parent, os.getsid(pid))
            except OSError:
                pass
        if os.getpid() not in table:  # the call has changed: an empty list must not read as an empty run
            raise OSError("the list of processes cannot be read")
        return table

    def rusage(pid):
        info = RusageInfo()
        if libc.proc_pid_rusage(pid, RUSAGE_INFO_V0, ctypes.byref(info)) != 0:
            raise failed(f"proc_pid_rusage of {pid}")
        return info

    def footprint(pid):
        """ri_phys_footprint: counts compressed and swapped pages, which resident size does not."""
        return rusage(pid).phys_footprint

    def pressure():
        level, size = ctypes.c_int(), ctypes.c_size_t(ctypes.sizeof(ctypes.c_int))
        if libc.sysctlbyname(b"kern.memorystatus_vm_pressure_level", ctypes.byref(level), ctypes.byref(size),
                             None, ctypes.c_size_t(0)) != 0:
            raise failed("kern.memorystatus_vm_pressure_level")
        return level.value

else:
    def processes():
        found = {}
        for name in os.listdir("/proc"):
            if name.isdigit() and int(name) > 1:
                try:
                    with open(f"/proc/{name}/stat", "rb") as f:
                        state, parent, _, session, *rest = f.read().rpartition(b")")[2].split()
                except OSError:
                    continue
                if state not in (b"Z", b"X"):
                    found[int(name)] = (int(parent), int(session), rest[15])  # its start time
        return {pid: ((pid, start), (parent, found[parent][2]) if parent in found else None, session)
                for pid, (parent, session, start) in found.items()}

    def footprint(pid):
        try:
            with open(f"/proc/{pid}/status", "rb") as f:
                rows = [line.split() for line in f]
        except FileNotFoundError:
            raise ProcessLookupError(errno.ESRCH, "gone") from None
        return 1024 * sum(int(r[1]) for r in rows if r and r[0] in (b"RssAnon:", b"RssShmem:", b"VmSwap:"))

    def pressure():
        return 1


def machine_trouble():
    if pressure() >= PRESSURE_WARNING:
        return "macOS reports memory pressure, which may come from anything on this machine"
    return None


# --- The run: which processes it is, what they use, and ending them.

class Run:
    def __init__(self, seed):
        table = processes()
        if seed not in table:
            raise OSError("the command's process has gone")
        self.seen = {table[seed][0]}  # the identity of every process of the run so far
        self.sessions = {seed}  # the command leads a session of its own
        self.pids = {seed}
        # This script and what it was started from are never the run's, whatever the table says later.
        self.never, rows = set(), {row[0]: row for row in table.values()}
        row = table.get(os.getpid())
        while row and row[0] not in self.never:
            self.never.add(row[0])
            row = rows.get(row[1])

    def scan(self):
        """The pids of the run's living processes."""
        table, pids, added = processes(), set(), True

        def led_by_the_run(session):
            return session in self.sessions and (session not in table or table[session][0] in self.seen)
        while added:
            added = False
            for pid, (own, parent, session) in table.items():
                if pid in pids or own in self.never:
                    continue
                if own in self.seen or parent in self.seen or led_by_the_run(session):
                    pids.add(pid)
                    self.seen.add(own)
                    if session == pid:
                        self.sessions.add(pid)
                    added = True
        # Only a session one of them led, never the one this script was started in. A session's number is
        # not used again while a process is in it; one with none left is dropped.
        self.sessions &= {table[pid][2] for pid in pids}
        self.pids = pids
        return pids

    def sizes(self):
        """What each uses, None for one that may not be read (another user's)."""
        sizes = []
        for pid in self.scan():
            try:
                sizes.append(footprint(pid))
            except ProcessLookupError:
                pass  # it has gone
            except OSError:
                sizes.append(None)
        return sizes

    def end(self):
        """Kills every process of the run; the pids of those not seen to end within KILL_LIMIT."""
        begun, stopped, blind = time.monotonic(), set(), False
        while True:
            try:
                live, listed = self.scan(), True
            except Exception:  # what was last known of the run goes, once: a pid may be another's by then
                live, listed = self.pids, False
            if listed and not live:
                return []
            took = time.monotonic() - begun
            # Stopped first, until none is new, so that none starts another that outlives its parent unseen. Not
            # for long: a stopped process keeps its memory, and looking may be slow when there is little left.
            new = live - stopped
            if listed and new and took < STOP_LIMIT:
                for pid in new:
                    send(pid, signal.SIGSTOP)
                stopped |= new
                continue
            if listed or not blind:
                for pid in live:
                    send(pid, signal.SIGKILL)
            blind = blind or not listed
            if took >= KILL_LIMIT:
                return sorted(live)
            time.sleep(0.01)


def send(pid, number):
    if pid <= 1:  # launchd, or what kill() takes as a group or as everything
        return
    try:
        os.kill(pid, number)
    except (ProcessLookupError, PermissionError):
        pass


def trouble(run, cap):
    """Why the run has to be killed, or None."""
    try:
        sizes = run.sizes()
        if sizes and all(size is None for size in sizes):
            return "the memory of none of its processes can be read"
        used = sum(size or 0 for size in sizes)
        if used > cap:
            return f"it used {used / 2**30:.2f} GB and the cap is {cap / 2**30:.2f} GB"
        return machine_trouble()
    except Exception as e:  # a run that cannot be watched does not go on
        return f"memory could not be read ({e})"


# --- The watcher and the command.

def say(text):
    try:
        print(f"mem-capped: {text}", file=sys.stderr, flush=True)
    except OSError:
        pass


def say_survivors(pids):
    if pids:
        say(f"sent SIGKILL, not seen to end and no longer watched: {' '.join(map(str, pids))}")


def watch(cap, pid_fd, note_fd, script):
    """The watcher: a process of its own, in a session of its own, so it outlives the script being killed."""
    os.setsid()
    for s in STOP_SIGNALS:
        signal.signal(s, signal.SIG_DFL)
    line = os.read(pid_fd, 20)
    if not line:
        return  # the script gave up before it had a command
    run = Run(int(line))
    run.scan()
    os.write(note_fd, b"+")
    while True:
        why = trouble(run, cap)
        if os.getppid() != script:
            why = why or "the script running it died"
        if why:
            told = False
            try:  # the reason first, into a pipe with room for it: the script must find it once the command is dead
                told = bool(os.write(note_fd, why.encode()[:400]))
            except OSError:
                pass  # the script is dead, and cannot say it
            left = run.end()
            if not told:
                say(f"killed: {why}")
            say_survivors(left)
            return
        time.sleep(POLL)


def forked(work):
    """Forks a child that does `work` and nothing else of this program."""
    pid = os.fork()
    if pid == 0:
        try:
            work()
        finally:
            os._exit(REFUSED)
    return pid


def exec_command(command, gate, ready):
    """The command's process, until it is the command: waits at the gate until the watcher watches it."""
    try:
        os.setsid()
        for s in STOP_SIGNALS:
            if signal.getsignal(s) != signal.SIG_IGN:
                signal.signal(s, signal.SIG_DFL)
        for s in (signal.SIGPIPE, signal.SIGXFSZ):  # Python ignores them
            signal.signal(s, signal.SIG_DFL)
        os.write(ready, b"+")
        if os.read(gate, 1):  # nothing: the script gave up or died
            os.environ.update(COMMAND_ENV)
            os.execvp(command[0], command)
    except Exception as e:
        os.write(ready, str(e).encode()[:400])


def wait_for(fd, what):
    if not select.select([fd], [], [], START_LIMIT)[0] or os.read(fd, 1) != b"+":
        raise Refused(f"{what} did not start")


def run_capped(command, cap, lock_path):
    stops = []
    for s in STOP_SIGNALS:  # before anything is started; a signal the caller ignores stays ignored
        if signal.getsignal(s) != signal.SIG_IGN:
            signal.signal(s, lambda number, _: stops.append(number))
    signal.signal(signal.SIGCHLD, signal.SIG_DFL)  # ignored, it would lose the exit codes
    watcher = child = 0
    try:
        # The command and the watcher hold the lock too: it stays taken until all of them have gone.
        lock = os.open(lock_path, os.O_RDWR | os.O_CREAT, 0o666)
        os.set_inheritable(lock, True)
        try:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            raise Refused(f"another capped run, or something one left running, holds {lock_path}") from None
        why = machine_trouble()
        if why:
            raise Refused(why)
        me = os.getpid()
        pid_r, pid_w = os.pipe()
        note_r, note_w = os.pipe()

        def be_watcher():
            os.close(pid_w)
            os.close(note_r)
            watch(cap, pid_r, note_w, me)
        watcher = forked(be_watcher)
        os.close(pid_r)
        os.close(note_w)
        gate_r, gate_w = os.pipe()
        ready_r, ready_w = os.pipe()

        def be_command():
            os.close(gate_w)
            os.close(pid_w)
            exec_command(command, gate_r, ready_w)
        child = forked(be_command)
        os.close(gate_r)
        os.close(ready_w)
        wait_for(ready_r, "the command's process")  # it leads its own session from here
        run = Run(child)
        run.scan()
        os.write(pid_w, str(child).encode())
        wait_for(note_r, "the watcher")
        if not stops:
            try:
                os.write(gate_w, b"+")
            except BrokenPipeError:
                pass  # the watcher has killed it already, and says why below
    except Exception as e:
        for pid in (child, watcher):
            if pid:
                send(pid, signal.SIGKILL)
                os.waitpid(pid, 0)
        say(f"not started: {e}")
        return 128 + stops[0] if stops else REFUSED

    reason = code = None
    while reason is None and code is None:
        if stops:
            code = 128 + stops[0]
            break
        done, status = os.waitpid(child, os.WNOHANG)
        if done:
            code = os.waitstatus_to_exitcode(status)
            code = code if code >= 0 else 128 - code
            break
        if os.waitpid(watcher, os.WNOHANG)[0]:
            watcher, reason = 0, "its watcher died"
            break
        reason = trouble(run, cap)
        if reason is None:
            time.sleep(POLL)
    # What the command left running goes too (but see the header for what is not known). Nothing is printed before the kill: printing can block.
    left = run.end()
    if watcher:
        send(watcher, signal.SIGKILL)
        os.waitpid(watcher, 0)
    reason = os.read(note_r, 1000).decode(errors="replace") or reason
    say_survivors(left)
    if reason:
        say(f"killed (exit {KILLED}): {reason}. The command did not fail by itself.")
        return KILLED
    if not stops:
        why = os.read(ready_r, 1000).decode(errors="replace")  # what exec said; the command's own process has gone
        if why:
            say(f"not started: {why}")
            return REFUSED
    return code


def main(argv):
    if argv == ["self-test"]:
        return self_test()
    cap, command = CAP_GB, argv
    if argv[:1] == ["--gb"]:
        try:
            cap = float(argv[1])
        except (IndexError, ValueError):
            cap = 0
        command = argv[2:]
    if not 0 < cap <= CAP_GB:
        say(f"not started: the cap is a number over 0 and at most {CAP_GB} (GB)")
        return REFUSED
    if command[:1] != ["--"] or len(command) < 2:
        say("not started: usage: mem-capped.py [--gb N] -- command args...")
        return REFUSED
    return run_capped(command[1:], int(cap * 2**30), LOCK)


# --- Self-test: real runs of small Python commands under a lock file of its own. Nothing waits a fixed time:
# a run and everything it started hold the write end of a pipe, whose end of file says all of them have gone.

MB = 2**20
real_pressure = pressure
PROLOGUE = "import os, sys\nup, go = map(int, sys.argv[1:3])\ndef say(): os.write(up, b'%d\\n' % os.getpid())\n"
WAIT = "say(); os.read(go, 1)"
HOG = "say(); os.read(go, 1); x = b'x' * (64 << 20); os.read(go, 1)"
SETSID_HOG = "if os.fork() == 0:\n os.setsid(); say(); x = b'x' * (64 << 20)\nos.read(go, 1)"
SETSID_WAIT = "if os.fork() == 0:\n os.setsid()\nsay(); os.read(go, 1)"
HOG_CAP = 48 * MB
HANG = 5  # seconds before a self-test is given up as hung; a trial takes a fraction of one


def check(ok, what):
    if not ok:
        raise AssertionError(what)


class Trial:
    """run_capped in a process of its own, with the command's pipes and what it printed."""
    runs = []

    def __init__(self, lock, code, cap=400 * MB, patch=None, command=None, gb=None):
        self.up, up = os.pipe()
        go, self.go = os.pipe()
        self.err, err = os.pipe()
        self.pid = os.fork()
        if self.pid == 0:
            result = 1
            try:
                signal.signal(signal.SIGALRM, signal.SIG_DFL)
                signal.alarm(HANG)  # the watcher then ends the rest
                os.dup2(err, 1)  # nor may one left behind hold the self-test's own output open
                os.dup2(err, 2)
                for fd in (self.up, self.go, self.err, err):
                    os.close(fd)
                for fd in (up, go):
                    os.set_inheritable(fd, True)
                if patch:
                    patch()
                command = command or [sys.executable, "-I", "-S", "-c", PROLOGUE + code, str(up), str(go)]
                if gb:
                    globals()["LOCK"] = lock
                    result = main(["--gb", gb, "--"] + command)
                else:
                    result = run_capped(command, cap, lock)
            finally:
                os._exit(result)
        for fd in (up, go, err):
            os.close(fd)
        try:
            Trial.runs.append(Run(self.pid))  # only for ending what a failed test leaves
        except OSError:
            pass  # it is over already

    def ups(self, count=1):
        """The pids the command's processes wrote once they ran."""
        text = b""
        while text.count(b"\n") < count:
            check(select.select([self.up], [], [], HANG)[0], "the command did not say it runs")
            got = os.read(self.up, 100)
            check(got, f"the command ended early: {text!r}")
            text += got
        Trial.runs[-1].scan()
        return [int(p) for p in text.split()]

    def let_go(self):
        os.write(self.go, b"g")

    def watcher(self, command_pid):
        table = processes()
        pids = [p for p in table if table[p][1] == table[self.pid][0] and p != command_pid]
        check(len(pids) == 1, f"the script's other child is the watcher: {pids}")
        return pids[0]

    def gone(self):
        """Waits until no process of the run is left; what they wrote that was not read yet."""
        text = b""
        while True:
            check(select.select([self.up], [], [], HANG)[0], "a process of the run is still there")
            got = os.read(self.up, 100)
            if not got:
                return text
            text += got

    def result(self):
        """(exit code, stderr) once everything has gone."""
        status = os.waitpid(self.pid, 0)[1]
        self.wrote = self.gone()
        text = b""
        while True:
            got = os.read(self.err, 4096)
            if not got:
                break
            text += got
        for fd in (self.up, self.go, self.err):
            os.close(fd)
        return os.waitstatus_to_exitcode(status), text.decode()


def self_test():
    tests = [f for name, f in sorted(globals().items()) if name.startswith("test_")]
    globals()["pressure"] = lambda: 1  # the machine's own pressure must not fail these

    def hung(*_):
        raise AssertionError("it hangs")
    signal.signal(signal.SIGALRM, hung)
    with tempfile.TemporaryDirectory() as folder:
        for test in tests:
            try:
                signal.alarm(3 * HANG)
                test(os.path.join(folder, test.__name__))
            except Exception as e:
                for run in Trial.runs:
                    run.end()
                print(f"mem-capped: self-test FAILED: {test.__name__}: {type(e).__name__}: {e}")
                return 1
    print(f"mem-capped: self-test ok, {len(tests)} tests")
    if sys.platform != "darwin":
        print("mem-capped: SKIPPED: the macOS footprint, process list and pressure readings (Linux ones were tested)")
    return 0


def expect(trial, code, *words):
    got, text = trial.result()
    check(got == code and all(w in text for w in words), f"wanted exit {code} and {words}, got {got} and {text!r}")


def test_exit_code_kept(lock):
    trial = Trial(lock, "sys.exit(0)")
    code, text = trial.result()
    check((code, text) == (0, ""), f"a run under the cap passes and says nothing: {code} {text!r}")
    expect(Trial(lock, "sys.exit(7)"), 7)
    expect(Trial(lock, "os.kill(os.getpid(), 15)"), 143)


def test_no_compile_server(lock):
    code = "sys.exit(7 if [os.environ.get(n) for n in ('RUSTC_WRAPPER', 'CARGO_BUILD_RUSTC_WRAPPER')] == ['', ''] else 8)"
    expect(Trial(lock, code, patch=lambda: os.environ.update(RUSTC_WRAPPER="sccache")), 7)


def test_over_the_cap(lock):
    trial = Trial(lock, HOG, cap=HOG_CAP)
    trial.let_go()
    expect(trial, KILLED, "killed", "the cap is 0.05 GB")
    trial = Trial(lock, HOG, gb="0.047")  # as a user gives it
    trial.let_go()
    expect(trial, KILLED, "killed", "the cap is 0.05 GB")


def test_setsid_child_is_counted_and_killed(lock):
    expect(Trial(lock, SETSID_HOG, cap=HOG_CAP), KILLED, "the cap is 0.05 GB")


def test_what_the_command_leaves_running_is_killed(lock):
    # The parent exits as soon as both have said they run. Linux keeps no trace of a parent that has gone, so
    # there the child leaves only the group.
    leave = "setsid()" if sys.platform == "darwin" else "setpgid(0, 0)"
    trial = Trial(lock, f"r, w = os.pipe()\nif os.fork() == 0:\n os.{leave}; say(); os.write(w, b'+'); os.read(go, 1)\n"
                        "os.read(r, 1); say()")
    trial.ups(2)
    expect(trial, 0)
    check(not trial.wrote, "only the two wrote")


def test_second_run_refused(lock):
    first = Trial(lock, WAIT)
    first.ups()
    second = Trial(lock, WAIT)
    expect(second, REFUSED, "not started", lock)
    check(not second.wrote, "a refused command does not run")
    first.let_go()
    expect(first, 0)
    expect(Trial(lock, "sys.exit(3)"), 3)  # the lock is free again


def test_killed_script_takes_its_command_down(lock):
    trial = Trial(lock, SETSID_WAIT)
    trial.ups(2)
    os.kill(trial.pid, signal.SIGKILL)
    expect(trial, -signal.SIGKILL, "the script running it died")


def test_dead_watcher_kills_the_command(lock):
    trial = Trial(lock, SETSID_WAIT)
    command = trial.ups(2)
    table = processes()
    command = [p for p in command if table[p][1] == table[trial.pid][0]][0]
    os.kill(trial.watcher(command), signal.SIGKILL)
    expect(trial, KILLED, "its watcher died")


def test_either_alone_enforces_the_cap(lock):
    trial = Trial(lock, HOG, cap=HOG_CAP)
    os.kill(trial.watcher(trial.ups()[0]), signal.SIGSTOP)
    trial.let_go()
    expect(trial, KILLED, "the cap is 0.05 GB")
    trial = Trial(lock, HOG, cap=HOG_CAP)
    watcher = trial.watcher(trial.ups()[0])
    os.kill(trial.pid, signal.SIGSTOP)
    trial.let_go()
    # The watcher goes once it has killed the command; the stopped script has not looked.
    limit = time.monotonic() + HANG
    while watcher in processes():
        check(time.monotonic() < limit, "the watcher goes once it has killed the command")
        time.sleep(0.001)
    os.kill(trial.pid, signal.SIGCONT)
    expect(trial, KILLED, "the cap is 0.05 GB")


def test_both_killed_still_refuses_a_second(lock):
    trial = Trial(lock, WAIT)
    command = trial.ups()[0]
    watcher = trial.watcher(command)
    os.kill(watcher, signal.SIGSTOP)  # so it cannot act on the script's death
    os.kill(trial.pid, signal.SIGKILL)
    os.kill(watcher, signal.SIGKILL)
    expect(Trial(lock, WAIT), REFUSED, lock)
    check(command in processes(), "the command outlives both: the case the lock is for")
    os.kill(command, signal.SIGKILL)
    expect(trial, -signal.SIGKILL)
    expect(Trial(lock, "sys.exit(3)"), 3)


def test_signals(lock):
    trial = Trial(lock, SETSID_WAIT)
    trial.ups(2)
    os.kill(trial.pid, signal.SIGTERM)
    expect(trial, 128 + signal.SIGTERM)

    def patch():  # arrives while it is starting: the command never runs
        script, real = os.getpid(), machine_trouble

        def fake():
            if os.getpid() == script:
                os.kill(script, signal.SIGTERM)
            return real()
        globals()["machine_trouble"] = fake
    trial = Trial(lock, WAIT, patch=patch)
    expect(trial, 128 + signal.SIGTERM)
    check(not trial.wrote, "a run told to stop while starting does not run the command")


def test_fork_failure_leaves_nothing(lock):
    for failing in (1, 2):
        def patch():
            real, calls = os.fork, []

            def fake():
                calls.append(1)
                if len(calls) == failing:
                    raise OSError(errno.EAGAIN, "no more processes")
                return real()
            os.fork = fake
        trial = Trial(lock, WAIT, patch=patch)
        expect(trial, REFUSED, "no more processes")
        check(not trial.wrote, "nothing ran")


def test_failures_before_the_start(lock):
    expect(Trial(lock, "", command=["/nonexistent/command"]), REFUSED, "not started", "No such file")
    expect(Trial(lock, "", command=["true", "a\0b"]), REFUSED, "not started", "null byte")
    expect(Trial(os.path.join(lock + ".no-such-folder", "lock"), WAIT), REFUSED, "not started", "No such file")

    def unreadable():
        raise OSError("no list")
    trial = Trial(lock, WAIT, patch=lambda: globals().update(processes=unreadable))
    expect(trial, REFUSED, "no list")
    check(not trial.wrote, "nothing ran")


def test_pressure_and_failed_reads(lock):
    expect(Trial(lock, WAIT, patch=lambda: globals().update(pressure=lambda: 2)),
           REFUSED, "not started", "memory pressure")

    def watcher_only():  # its first look, before or after the gate opens: the reason is not lost
        script = os.getpid()
        globals()["pressure"] = lambda: 1 if os.getpid() == script else 2
    expect(Trial(lock, WAIT, patch=watcher_only), KILLED, "killed", "memory pressure")

    def failing(number):
        raise OSError("no answer")
    flag = lock + ".flag"
    code = f"say(); open({flag!r}, 'w').close(); os.read(go, 1)"

    def later(name, then):
        def patch():
            real = globals()[name]
            globals()[name] = lambda *a: then(*a) if os.path.exists(flag) else real(*a)
        return patch
    for patch, words in ((later("pressure", lambda: 2), "anything on this machine"),
                         (later("footprint", failing), "none of its processes")):
        expect(Trial(lock, code, patch=patch), KILLED, "killed", words)
        os.remove(flag)
    # The list cannot be read at the end either: what was known of the run is killed all the same.
    lost = later("processes", lambda: failing(0))
    trial = Trial(lock, code, patch=lambda: (lost(), globals().update(KILL_LIMIT=0.05)))
    expect(trial, KILLED, "no answer", "not seen to end")


def test_command_line(_):
    for argv in ([], ["--"], ["sleep", "0"], ["--gb"], ["--gb", "x", "--", "true"], ["--gb", "nan", "--", "true"],
                 ["--gb", "inf", "--", "true"], ["--gb", "0", "--", "true"], ["--gb", str(CAP_GB + 0.1), "--", "true"]):
        saved, said = sys.stderr, []
        sys.stderr = type("Err", (), {"write": lambda _, t: said.append(t), "flush": lambda _: None})()
        try:
            code = main(argv)
        finally:
            sys.stderr = saved
        check(code == REFUSED and "not started" in "".join(said), f"{argv} is refused: {code} {said}")
    saved = run_capped
    globals()["run_capped"] = lambda *given: given
    try:
        check(main(["--", "a", "--gb", "9"]) == (["a", "--gb", "9"], CAP_GB * 1024 * MB, LOCK), "the cap when none is given")
        check(main(["--gb", "0.5", "--", "a"]) == (["a"], 512 * MB, LOCK), "a lower one")
    finally:
        globals()["run_capped"] = saved


def test_which_processes_are_the_run(_):
    """Parents, sessions, processes whose parent has gone, and pids used again, on made-up tables."""
    saved, table = processes, {}
    globals()["processes"] = lambda: dict(table)
    try:
        # pid: (identity, parent's, session). 5 is the script's shell; 20 is the command. Children come first, so
        # a single pass would miss them.
        table.update({31: (6, 5, 30), 30: (5, 4, 30), 40: (7, 3, 40), 21: (4, 3, 20), 20: (3, 2, 20),
                      os.getpid(): (2, 1, 5), 5: (1, 0, 5)})
        run = Run(20)
        check(run.scan() == {20, 21, 30, 31, 40}, "children, theirs, and sessions they lead")
        # 20, 21 and 30 have ended. 32 lost a parent nobody saw but is in 30's session; 41 is a daemon's child.
        for pid in (20, 21, 30):
            del table[pid]
        table.update({32: (9, 8, 30), 41: (10, 7, 40)})
        check(run.scan() == {31, 32, 40, 41}, "kept after their parents went, and their new ones with them")
        # A daemon that lost its parent, never seen itself: its parent's identity says whose it was.
        table[50] = (11, 4, 50)
        check(run.scan() == {31, 32, 40, 41, 50}, "a daemon of a process that has gone")
        # 30's number is in use again while 31 is still in the session 30 led: those of the new 30 are not the run's.
        table.update({30: (20, 1, 30), 33: (21, 20, 30)})
        check(run.scan() == {31, 32, 40, 41, 50}, "a session led by another process")
        del table[30], table[33]
        # The script and what it was started from are never the run's, whatever the table says of them.
        table.update({os.getpid(): (2, 3, 20), 5: (1, 3, 20)})
        check(run.scan() == {31, 32, 40, 41, 50}, "its own ancestors")
        table.update({os.getpid(): (2, 1, 5), 5: (1, 0, 5)})
        # Pids used again by other processes, one as a session (its leader gone) that the run's 30 once led.
        for pid in (31, 32, 50):
            del table[pid]
        run.scan()
        table.update({33: (13, 12, 30), 50: (14, 1, 5)})
        check(run.scan() == {40, 41}, "a pid used again is another process")
    finally:
        globals()["processes"] = saved


def modelled(table, test):
    """Runs the test with a made-up table, clock and kill; `kill` in it is called for each signal."""
    saved, kill, monotonic = processes, send, time.monotonic
    now = [0.0]
    time.monotonic = lambda: now[0]

    def listed():
        now[0] += table.get("takes", 0.01)
        for pid in table.get("bomb", ()):  # it starts another for every look
            table[pid + 1 + len(table)] = (1000 + len(table), 3, 20)
        return {pid: row for pid, row in table.items() if isinstance(pid, int)}
    globals()["processes"] = listed
    try:
        table.update({20: (3, 2, 20), os.getpid(): (2, 1, 5), 5: (1, 0, 5)})
        test(Run(20))
    finally:
        time.monotonic = monotonic
        globals().update(processes=saved, send=kill)


def test_stopped_before_killed(_):
    """One killed while it runs may be starting a daemon through a process nobody saw; a stopped one is not."""
    table, stopped = {21: (4, 3, 20)}, set()

    def signal_it(pid, number):
        if number == signal.SIGSTOP:
            if pid == 20 and not stopped:
                table[22] = (7, 3, 20)  # started as the signal was on its way
            stopped.add(pid)
        elif pid in table:
            if pid not in stopped:
                table[100 + pid] = (50 + pid, 90 + pid, 100 + pid)
            del table[pid]

    def test(run):
        globals()["send"] = signal_it
        check(run.end() == [] and set(table) == {5, os.getpid()}, f"nothing is left: {table}")
    modelled(table, test)


def test_never_left_stopped(_):
    """Slow lists, and a run that starts another for every look: all of it is killed, not only stopped."""
    for table in ({"takes": 5.5}, {"takes": 0.4, "bomb": [30]}):
        last = {}

        def signal_it(pid, number):
            check(time.monotonic() < 60, "it ends")
            last[pid] = number

        def test(run):
            globals()["send"] = signal_it
            left = run.end()
            check(20 in left and all(last[pid] == signal.SIGKILL for pid in left), f"every one is killed: {last}")
        modelled(table, test)
    told = []
    globals()["send"], saved = (lambda *a: told.append(a)), send
    saved_kill = os.kill
    os.kill = lambda *a: told.append(a)
    try:
        for pid in (-1, 0, 1):
            saved(pid, signal.SIGKILL)
        check(not told, "no signal to launchd, to a group or to everything")
    finally:
        os.kill = saved_kill
        globals()["send"] = saved


def test_slow_list(lock):
    """Listing the processes takes over the kill's limit: the command is killed and not left stopped."""
    def patch():
        real, clock, late = processes, time.monotonic, [0.0]

        def slow():
            late[0] += 5.5
            return real()
        globals()["processes"] = slow
        time.monotonic = lambda: clock() + late[0]
    trial = Trial(lock, HOG, cap=HOG_CAP, patch=patch)
    trial.let_go()
    expect(trial, KILLED, "the cap is 0.05 GB")


def test_what_cannot_be_read(_):
    """One that has gone is left out, one that may not be read is unknown, and neither ends the watch."""
    saved = footprint
    child = os.fork()
    if child == 0:
        signal.signal(signal.SIGALRM, signal.SIG_DFL)
        signal.alarm(HANG)
        signal.pause()
    run = Run(child)

    def answer(number):
        def fake(_):
            raise OSError(number, "no")
        globals()["footprint"] = fake
    try:
        answer(errno.ESRCH)
        check(run.sizes() == [] and trouble(run, 1) is None, "gone")
        answer(errno.EPERM)
        check(run.sizes() == [None] and "none of its processes" in trouble(run, 1), "another user's")
        run.sizes = lambda: [10, None]
        check(trouble(run, 10) is None and "cap" in trouble(run, 9), "the ones that can be read count")
    finally:
        globals()["footprint"] = saved
        os.kill(child, signal.SIGKILL)
        os.waitpid(child, 0)


def test_the_readings(lock):
    me, table = os.getpid(), processes()
    check(not {0, 1} & set(table), "neither the kernel nor launchd is ever a process to look at")
    check(table[me][2] == os.getsid(0), "listed with its session")
    if os.getppid() in table:
        check(table[me][1] == table[os.getppid()][0], "and its parent")
    child = os.fork()
    if child == 0:
        os._exit(0)
    for _ in range(2000):  # until it has exited; nobody has reaped it
        if child not in processes():
            break
        time.sleep(0.001)
    else:
        check(False, "a zombie is not a living process")
    os.waitpid(child, 0)
    try:
        footprint(child)
        check(False, "a process that has gone has no footprint")
    except ProcessLookupError:
        pass
    before = footprint(me)
    block = b"x" * (32 * MB)
    after = footprint(me)
    check(after - before >= len(block) * 0.9, f"32 MB written add to the footprint: {before} {after}")
    del block
    if sys.platform == "darwin":  # resident size and footprint are told apart by pages that are only a file's
        with open(lock, "w+b") as f:
            f.truncate(32 * MB)
            with mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ) as pages:
                before = rusage(me)
                check(sum(pages[at] for at in range(0, len(pages), 4096)) == 0, "read")
                after = rusage(me)
        grown = [(getattr(after, n) - getattr(before, n)) // MB for n in ("resident_size", "phys_footprint")]
        check(grown[0] >= 28 and grown[1] <= 4, f"a file's clean pages are resident and not footprint: {grown} MB")
        check(footprint(me) == rusage(me).phys_footprint, "and the footprint is what counts")
    check(real_pressure() in (1, 2, 4), "the pressure level is one of macOS's")


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
