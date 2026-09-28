"""Memory and time guard for every process the CAD comparison starts.

Two runaway geometry jobs (25 GB and 71 GB) have already filled the
owner's swap, so nothing here runs unwatched: a thread polls the process
tree of each child every half second and kills the whole tree when any
process passes the limit. RSS alone hides memory the kernel has
compressed or swapped, so any process above a quarter of the limit is
also measured with `footprint` (macOS's physical footprint, compressed
pages included) every few seconds.
"""

import os
import re
import signal
import subprocess
import threading
import time

DEFAULT_LIMIT_MB = 2048


def _tree(root):
    """{pid: (ppid, rss_kb, command)} for root and its descendants."""
    try:
        out = subprocess.run(["ps", "-A", "-o", "pid=,ppid=,rss=,comm="], capture_output=True,
                             text=True, timeout=10).stdout
    except (OSError, subprocess.TimeoutExpired):
        return {}
    procs = {}
    for line in out.splitlines():
        parts = line.split(None, 3)
        if len(parts) >= 3 and parts[0].isdigit():
            procs[int(parts[0])] = (int(parts[1]), int(parts[2]), parts[3] if len(parts) > 3 else "")
    keep = {root}
    changed = True
    while changed:
        changed = False
        for pid, (ppid, _, _) in procs.items():
            if ppid in keep and pid not in keep:
                keep.add(pid)
                changed = True
    return {p: procs[p] for p in keep if p in procs}


def footprint_mb(pid):
    try:
        out = subprocess.run(["footprint", "-p", str(pid)], capture_output=True, text=True, timeout=20).stdout
    except (OSError, subprocess.TimeoutExpired):
        return None
    m = re.search(r"Footprint:\s*([\d.]+)\s*([KMG]?B)", out)
    if not m:
        return None
    scale = {"B": 1 / 2**20, "KB": 1 / 1024, "MB": 1, "GB": 1024}[m.group(2)]
    return float(m.group(1)) * scale


class Guard:
    """Watches the tree under `pid`. `exempt` names commands (by substring
    of the executable) held to `exempt_limit_mb` instead, e.g. Claude Code's
    own node process, which is not a geometry job."""

    def __init__(self, pid, limit_mb=DEFAULT_LIMIT_MB, exempt=(), exempt_limit_mb=4096, log=None):
        self.pid, self.limit, self.exempt, self.exempt_limit = pid, limit_mb, exempt, exempt_limit_mb
        self.peak_mb = {}
        self.killed = None
        self.log = log
        self._stop = threading.Event()
        self._fp_at = {}
        self.thread = threading.Thread(target=self._run, daemon=True)
        self.thread.start()

    def _limit_for(self, comm):
        return self.exempt_limit if any(e in comm for e in self.exempt) else self.limit

    def _run(self):
        while not self._stop.is_set():
            now = time.monotonic()
            for pid, (_, rss_kb, comm) in _tree(self.pid).items():
                mb = rss_kb / 1024
                lim = self._limit_for(comm)
                if mb > lim / 4 and now - self._fp_at.get(pid, 0) > 3:
                    self._fp_at[pid] = now
                    fp = footprint_mb(pid)
                    if fp:
                        mb = max(mb, fp)
                name = os.path.basename(comm) or str(pid)
                self.peak_mb[name] = max(self.peak_mb.get(name, 0), round(mb))
                if mb > lim:
                    self.killed = {"pid": pid, "command": comm, "mb": round(mb), "limit_mb": lim}
                    if self.log:
                        self.log(f"memory guard: {comm} ({pid}) at {mb:.0f} MB > {lim} MB; killing the run")
                    self.kill_tree()
                    return
            self._stop.wait(0.5)

    def kill_tree(self):
        for pid in sorted(_tree(self.pid), reverse=True):
            try:
                os.kill(pid, signal.SIGKILL)
            except OSError:
                pass
        try:
            os.killpg(self.pid, signal.SIGKILL)
        except OSError:
            pass

    def stop(self):
        self._stop.set()
        self.thread.join(timeout=5)


def run(cmd, cwd=None, timeout=300, limit_mb=DEFAULT_LIMIT_MB, env=None):
    """subprocess.run under the guard. Returns (returncode, stdout, stderr,
    seconds, guard_record). returncode is None on a timeout or a kill."""
    t0 = time.monotonic()
    p = subprocess.Popen(cmd, cwd=cwd, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                         start_new_session=True, env=env, stdin=subprocess.DEVNULL)
    g = Guard(p.pid, limit_mb)
    try:
        out, err = p.communicate(timeout=timeout)
        rc = p.returncode
    except subprocess.TimeoutExpired:
        g.kill_tree()
        out, err = p.communicate()
        rc = None
        err = (err or "") + f"\n[guard] timed out after {timeout} s"
    dt = time.monotonic() - t0
    g.stop()
    if g.killed:
        rc = None
        err = (err or "") + f"\n[guard] killed: {g.killed}"
    return rc, out, err, dt, {"peak_mb": g.peak_mb, "killed": g.killed}
