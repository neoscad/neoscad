#!/usr/bin/env python3
"""Agent CAD comparison: ModelRift's CadQuery-vs-OpenSCAD benchmark with
NeoSCAD as a third condition (docs/agent-eval.md, "CAD comparison").

    run_cad.py --neoscad PATH [--model 'claude-opus-5[1m]'] [--tasks T1,T2,T3]
               [--conditions openscad,cadquery,neoscad] [--n 1]
    run_cad.py --tasks T0 --model haiku ...      # plumbing check, cheap
    run_cad.py --print-commands ...              # show the claude commands only
    run_cad.py --regrade cad-<ts>                # grade saved STLs again

Each run is headless `claude -p` in a fresh temporary directory, one at a
time. Claude Code runs `--restricted` (file tools confined to the run
directory, no user or project settings, hooks or MCP servers) with its Bash
sandbox on: writes only inside the run directory (and the system temp
dirs), no network, and the other conditions' tools unreadable. Every
process is under guard.py (2 GB per geometry process, 45 min per run).

Results: progress/agent-eval/cad-<ts>.json (a new file per eval, rewritten
after each run so a crash keeps what finished), transcripts, sources, STL
versions and grades under progress/agent-eval/cad-<ts>/, one summary line
in progress/agent-eval/cad-index.jsonl. summarize.py turns a record into
the post's markdown tables.
"""

import argparse
import datetime
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
sys.path.insert(0, str(HERE))
import guard  # noqa: E402

SPEC = json.loads((HERE / "tasks.json").read_text())
OPENSCAD = "/Applications/OpenSCAD.app/Contents/MacOS/OpenSCAD"
VENV = ROOT / ".cache" / "agent-eval" / "cadquery-venv"
MCP_TOOLS = ["evaluate", "render", "snapshot", "check", "measure", "test", "format", "docs"]
BASE_TOOLS = ["Bash", "Read", "Write", "Edit"]
MAX_KEEP_BYTES = 200 * 2**20  # never copy a file bigger than this out of a run


def log(msg):
    print(msg, flush=True)


def git(*args):
    return subprocess.run(["git", "-C", str(ROOT), *args], capture_output=True, text=True).stdout.strip()


def tool_versions(args):
    v = {}
    try:
        p = subprocess.run([args.openscad, "--version"], capture_output=True, text=True, timeout=30)
        v["openscad"] = (p.stdout + p.stderr).strip()
    except (OSError, subprocess.TimeoutExpired) as e:
        v["openscad"] = f"unavailable: {e}"
    try:
        p = subprocess.run([args.neoscad, "--version"], capture_output=True, text=True, timeout=30)
        v["neoscad"] = (p.stdout + p.stderr).strip()
    except (OSError, subprocess.TimeoutExpired) as e:
        v["neoscad"] = f"unavailable: {e}"
    try:
        probe = "import cadquery, sys; print(cadquery.__version__, sys.version.split()[0])"
        p = subprocess.run([str(args.python), "-c", probe], capture_output=True, text=True, timeout=120)
        v["cadquery_python"] = p.stdout.strip() or p.stderr.strip()[-200:]
    except (OSError, subprocess.TimeoutExpired) as e:
        v["cadquery_python"] = f"unavailable: {e}"
    v["claude"] = subprocess.run([args.claude, "--version"], capture_output=True, text=True).stdout.strip()
    return v


_VIEWER = None


def viewer_path():
    """A copy of view_stl.py outside the repository. The sandbox denies
    reading the repository, so an agent that opened the script to learn its
    options got a permission error and wasted calls on it. The copy is
    readable; running it is exempted from the sandbox as before."""
    global _VIEWER
    if _VIEWER is None:
        d = Path(tempfile.mkdtemp(prefix="cad-tools-")).resolve()
        _VIEWER = d / "view_stl.py"
        shutil.copy2(HERE / "view_stl.py", _VIEWER)
    return _VIEWER


def prompt_for(task, cond, args):
    t, c = SPEC["tasks"][task], SPEC["conditions"][cond]
    parts = t["parts"]
    return SPEC["prompt"].format(
        spec=t["spec"],
        sources=", ".join(f"{p}.{c['ext']}" for p in parts),
        how_export=c["how_export"],
        stls=", ".join(f"out/{p}.stl" for p in parts),
    ), c["briefing"].format(openscad=args.openscad, python=args.python,
                            viewer=f"{args.python} {viewer_path()}")


def sandbox_settings(cond, args, writable=()):
    """Claude Code's Bash sandbox: writes only in the run directory, no
    network, and each condition blind to the other conditions' tools (so
    the CadQuery agent cannot fall back on OpenSCAD, and none can read the
    NeoSCAD repository). The MCP server runs outside this sandbox; it has
    its own roots (docs/mcp.md, "Safety")."""
    deny = [str(ROOT), str(Path(args.neoscad).resolve().parent)]
    if cond != "openscad":
        deny.append(str(Path(args.openscad).resolve().parents[2]))  # the .app bundle
    fs = {"denyRead": [f"//{p.lstrip('/')}" for p in deny]}
    if writable:
        fs["allowWrite"] = [f"//{str(p).lstrip('/')}" for p in writable]
    if cond == "cadquery":
        # The venv may sit inside the (denied) repository; its interpreter
        # is a link into uv's Python, which is outside it.
        fs["allowRead"] = [f"//{str(args.venv.resolve()).lstrip('/')}"]
    sandbox_extra = {}
    if cond == "openscad":
        # Inside the sandbox the nightly aborts at startup ("Incompatible
        # processor. This Qt build requires the following features: neon"):
        # Qt's CPU check reads hw.optional.neon, a sysctl the sandbox
        # denies. So OpenSCAD itself runs outside it, still under the
        # harness's memory guard and time limit. Claude Code exempts a
        # compound command only when every part matches, so the briefing
        # tells the agent to run OpenSCAD as a command of its own.
        sandbox_extra["excludedCommands"] = [args.openscad, args.openscad + ":*"]
    elif cond == "cadquery":
        # CadQuery's VTK viewer gets no OpenGL context in the sandbox, so
        # the one image path is view_stl.py (which refuses paths outside
        # the working directory), exempted the same way.
        viewer = f"{args.python} {viewer_path()}"
        sandbox_extra["excludedCommands"] = [viewer, viewer + ":*"]
    return {
        "sandbox": {
            "enabled": True,
            "failIfUnavailable": True,
            "allowUnsandboxedCommands": False,
            "autoAllowBashIfSandboxed": True,
            "network": {"allowedDomains": [], "strictAllowlist": True},
            "filesystem": fs,
            **sandbox_extra,
        },
        "permissions": {"defaultMode": "acceptEdits"},
    }


def command(task, cond, workdir, args, keep, aux=None):
    """The claude command line, and the environment it runs in. `aux` is a
    per-run scratch directory outside the run directory: CadQuery's numba
    wants a writable cache and the venv is read-only in the sandbox
    ("cannot cache function '_preprocess': no locator available" on
    importing cadquery.vis), so NUMBA_CACHE_DIR points there."""
    prompt, briefing = prompt_for(task, cond, args)
    tools = list(BASE_TOOLS)
    allowed = list(BASE_TOOLS)
    env = dict(os.environ)
    writable = []
    if cond == "cadquery" and aux is not None:
        env["NUMBA_CACHE_DIR"] = str(aux / "numba")
        writable.append(aux)
    settings = keep / "settings.json"
    settings.write_text(json.dumps(sandbox_settings(cond, args, writable), indent=1))
    cmd = [
        args.claude, "-p", prompt,
        "--model", args.model,
        "--output-format", "stream-json", "--verbose",
        "--append-system-prompt", briefing,
        "--restricted",
        "--settings", str(settings),
        "--strict-mcp-config",
        "--no-session-persistence",
        "--permission-prompts", "none",
        "--max-turns", str(args.max_turns),
        "--max-budget-usd", str(args.max_budget_usd),
    ]
    if cond == "neoscad":
        mcp = keep / "mcp.json"
        mcp.write_text(json.dumps({"mcpServers": {"neoscad": {
            "command": str(args.neoscad), "args": ["mcp", "--root", str(workdir)]}}}))
        cmd += ["--mcp-config", str(mcp)]
        allowed += [f"mcp__neoscad__{t}" for t in MCP_TOOLS]
    cmd += ["--tools", ",".join(tools), "--allowedTools", ",".join(allowed)]
    return cmd, env


class Watcher:
    """Snapshots every STL written in the run directory, with the time it
    appeared, so versions can be counted and graded after the run."""

    def __init__(self, workdir, history, t0):
        self.workdir, self.history, self.t0 = workdir, history, t0
        self.events = []
        self.seen = {}
        self.pending = {}
        self._stop = threading.Event()
        self.thread = threading.Thread(target=self._run, daemon=True)
        self.thread.start()

    def _scan(self):
        now = time.monotonic() - self.t0
        for p in self.workdir.rglob("*.stl"):
            try:
                st = p.stat()
            except OSError:
                continue
            key = (st.st_mtime_ns, st.st_size)
            rel = str(p.relative_to(self.workdir))
            if self.seen.get(rel) == key:
                continue
            # Copy only once the file has stopped changing for one poll.
            if self.pending.get(rel, (None,))[0] != key:
                self.pending[rel] = (key, now)
                continue
            first_seen = self.pending.pop(rel)[1]
            self.seen[rel] = key
            if st.st_size > MAX_KEEP_BYTES:
                self.events.append({"t": round(first_seen, 2), "path": rel, "size": st.st_size, "skipped": "too big"})
                continue
            data = p.read_bytes()
            h = hashlib.sha1(data).hexdigest()[:16]
            dest = self.history / f"{len(self.events):03d}-{rel.replace('/', '__')}"
            dest.write_bytes(data)
            self.events.append({"t": round(first_seen, 2), "path": rel, "size": st.st_size, "sha1": h,
                                "copy": dest.name})

    def _run(self):
        while not self._stop.is_set():
            try:
                self._scan()
            except OSError:
                pass
            self._stop.wait(0.5)

    def stop(self):
        self._stop.set()
        self.thread.join(timeout=10)
        self._scan()
        self._scan()


# ---------------------------------------------------------------------------
# Transcript accounting


ERROR_PATTERNS = {
    "openscad": re.compile(r"^ERROR:|^\s*ERROR:", re.M),
    "cadquery": re.compile(r"Traceback \(most recent call last\)|^\w*Error: ", re.M),
}


def tool_call_is_cad(cond, name, inp, args):
    if cond == "neoscad":
        return name.startswith("mcp__neoscad__") and name.split("__")[-1] in ("evaluate", "render", "snapshot",
                                                                              "check", "measure", "test")
    if name != "Bash":
        return False
    cmd = inp.get("command", "")
    if cond == "openscad":
        return "openscad" in cmd.lower()
    return str(args.python) in cmd or "cadquery-venv" in cmd


def result_text(c):
    content = c.get("content")
    if isinstance(content, list):
        return "\n".join(x.get("text", "") for x in content if isinstance(x, dict))
    return content if isinstance(content, str) else json.dumps(content)


def is_tool_error(cond, c, text):
    if cond == "neoscad":
        if c.get("is_error"):
            return True
        try:
            j = json.loads(text)
        except (json.JSONDecodeError, TypeError):
            return "error" in text[:200].lower()
        if not isinstance(j, dict):
            return False
        if j.get("failed") or j.get("error"):
            return True
        return any(d.get("severity") == "error" for d in j.get("diagnostics", []) if isinstance(d, dict))
    return bool(ERROR_PATTERNS[cond].search(text)) or (bool(c.get("is_error")) and "Exit code" in text)


def account(lines, cond, args):
    """Tool calls with arrival times, CAD-tool errors, tokens and cost."""
    calls = {}
    order = []
    results = []
    for t, line in lines:
        try:
            m = json.loads(line)
        except json.JSONDecodeError:
            continue
        if m.get("type") == "assistant":
            for c in m["message"].get("content", []):
                if c.get("type") == "tool_use":
                    calls[c["id"]] = {"name": c["name"], "input": c.get("input", {}), "start": t, "end": None}
                    order.append(c["id"])
        elif m.get("type") == "user":
            content = m.get("message", {}).get("content", [])
            for c in content if isinstance(content, list) else []:
                if isinstance(c, dict) and c.get("type") == "tool_result" and c.get("tool_use_id") in calls:
                    call = calls[c["tool_use_id"]]
                    call["end"] = t
                    text = result_text(c)
                    call["is_error"] = bool(c.get("is_error"))
                    call["cad"] = tool_call_is_cad(cond, call["name"], call["input"], args)
                    call["cad_error"] = call["cad"] and is_tool_error(cond, c, text)
                    if call["cad_error"]:
                        call["error_text"] = text[:2000]
        elif m.get("type") == "result":
            results.append(m)
    tools = {}
    for i in order:
        n = calls[i]["name"].removeprefix("mcp__neoscad__")
        tools[n] = tools.get(n, 0) + 1
    cad_calls = [calls[i] for i in order if calls[i].get("cad")]
    return {
        "calls": [dict(calls[i], id=i) for i in order],
        "summary": {
            "tool_calls": len(order),
            "tools": tools,
            "cad_tool_calls": len(cad_calls),
            "cad_tool_errors": sum(1 for c in cad_calls if c.get("cad_error")),
            "all_error_results": sum(1 for i in order if calls[i].get("is_error")),
            **result_summary(results),
        },
    }


def result_summary(results):
    """Turns, tokens, cost and the final message from the stream's result
    events. A session can emit more than one: when a background command
    the agent started finishes after its report, Claude Code wakes it for
    a short follow-up turn with a result of its own (`origin`
    task-notification). That result's `num_turns` counts only the
    follow-up, while its `modelUsage`, `total_cost_usd` and
    `duration_api_ms` are the session's running totals. Taking the last
    result's figures as the run's once recorded a long run as the
    follow-up's few turns and its follow-up note as the final message,
    so turns are summed, totals come from the last
    result, and the report is the last result that answered the prompt."""
    last = results[-1] if results else {}
    usage = {"input": 0, "cache_read": 0, "cache_write": 0, "output": 0}
    for u in (last.get("modelUsage") or {}).values():
        usage["input"] += u.get("inputTokens", 0)
        usage["cache_read"] += u.get("cacheReadInputTokens", 0)
        usage["cache_write"] += u.get("cacheCreationInputTokens", 0)
        usage["output"] += u.get("outputTokens", 0)
    answers = [r for r in results if not r.get("origin")] or results
    report = answers[-1] if answers else {}
    return {
        "turns": sum(r.get("num_turns") or 0 for r in results) if results else None,
        "tokens": usage,
        "tokens_total": sum(usage.values()),
        "tokens_excl_cache_reads": usage["input"] + usage["cache_write"] + usage["output"],
        "cost_usd": last.get("total_cost_usd"),
        "api_ms": last.get("duration_api_ms"),
        "stop": last.get("subtype"),
        "is_error": last.get("is_error"),
        "final_message": report.get("result") or "",
        "result_events": len(results),
        "followup_messages": [r.get("result") or "" for r in results if r is not report],
    }


def versions_from(events, calls, parts):
    """Group STL writes to out/ into versions: one per tool call that wrote
    new bytes to out/<part>.stl (writes outside a call are grouped when
    within 2 s). Returns [{call, t, state: {part: copy}, changed: [...]}]."""
    wanted = {f"out/{p}.stl": p for p in parts}
    state, last_hash = {}, {}
    versions = []
    for e in sorted(events, key=lambda e: e["t"]):
        part = wanted.get(e["path"])
        if part is None or "sha1" not in e or last_hash.get(part) == e["sha1"]:
            continue
        last_hash[part] = e["sha1"]
        state[part] = e["copy"]
        owner = None
        for c in calls:
            if c["start"] <= e["t"] <= (c["end"] if c["end"] is not None else float("inf")) + 1.5:
                owner = c["id"]
        if versions and ((owner and versions[-1]["call"] == owner)
                         or (not owner and not versions[-1]["call"] and e["t"] - versions[-1]["t"] < 2)):
            versions[-1]["state"] = dict(state)
            versions[-1]["changed"].append(part)
        else:
            versions.append({"call": owner, "t": e["t"], "state": dict(state), "changed": [part]})
    return versions


def grade_files(task, files, keep):
    """grade.py in its own guarded process (it is stdlib Python, but a huge
    STL is still memory)."""
    rc, out, err, dt, g = guard.run([sys.executable, str(HERE / "grade.py"), "--task", task,
                                     *[f"{k}={v}" for k, v in files.items()]], timeout=900)
    if rc != 0:
        return {"pass": False, "clean": False, "grader_failed": (err or "")[-1000:], "guard": g}
    r = json.loads(out)
    r["grade_s"] = round(dt, 2)
    return r


def line_count(workdir, task, cond):
    """Non-blank lines of the part sources and the local files they
    include, use or import."""
    ext = SPEC["conditions"][cond]["ext"]
    todo = [workdir / f"{p}.{ext}" for p in SPEC["tasks"][task]["parts"]]
    seen, total = set(), 0
    while todo:
        f = todo.pop()
        if f in seen or not f.exists():
            continue
        seen.add(f)
        text = f.read_text(errors="replace")
        total += sum(1 for l in text.splitlines() if l.strip())
        if ext == "scad":
            for m in re.finditer(r"(?:include|use)\s*<([^>]+)>", text):
                todo.append((f.parent / m.group(1)).resolve())
        else:
            for m in re.finditer(r"^\s*(?:from|import)\s+([\w.]+)", text, re.M):
                todo.append(workdir / (m.group(1).split(".")[0] + ".py"))
    return {"lines": total, "files": sorted(str(p.relative_to(workdir)) for p in seen if p.is_relative_to(workdir))}


def recompute(task, cond, workdir, args, repeats=3):
    """Re-time the final model's export outside the agent: copy the
    sources to a fresh directory and export every part `repeats` times
    with the condition's tool from the command line (NeoSCAD's CLI, a
    cold process, not the MCP server's warm cache). Reports the median of
    the per-repeat totals; the first repeat pays any cold-cache cost."""
    ext = SPEC["conditions"][cond]["ext"]
    parts = SPEC["tasks"][task]["parts"]
    d = Path(tempfile.mkdtemp(prefix="cad-recompute-")).resolve()
    try:
        for f in workdir.iterdir():
            if f.is_file() and f.suffix in (".scad", ".py", ".json", ".dxf", ".svg", ".txt") and \
                    f.stat().st_size < 10 * 2**20:
                shutil.copy(f, d / f.name)
        (d / "out").mkdir()
        totals, errors, peaks = [], [], {}
        for _ in range(repeats):
            total = 0.0
            for p in parts:
                if cond == "cadquery":
                    cmd = [str(args.python), f"{p}.py"]
                elif cond == "openscad":
                    cmd = [args.openscad, "--backend=manifold", "-o", f"out/{p}.stl", f"{p}.scad"]
                else:
                    cmd = [str(args.neoscad), "-o", f"out/{p}.stl", f"{p}.scad"]
                if not (d / f"{p}.{ext}").exists():
                    errors.append(f"missing {p}.{ext}")
                    continue
                rc, _, err, dt, g = guard.run(cmd, cwd=d, timeout=600)
                total += dt
                for k, v in g["peak_mb"].items():
                    peaks[k] = max(peaks.get(k, 0), v)
                if rc != 0:
                    errors.append(f"{p}: exit {rc}: {(err or '')[-500:]}")
            totals.append(total)
            if errors:
                break
        stls = {p: str(d / "out" / f"{p}.stl") for p in parts if (d / "out" / f"{p}.stl").exists()}
        same = None
        if stls and not errors:
            same = all(hashlib.sha1(Path(s).read_bytes()).hexdigest() ==
                       hashlib.sha1((workdir / "out" / f"{p}.stl").read_bytes()).hexdigest()
                       for p, s in stls.items() if (workdir / "out" / f"{p}.stl").exists())
        return {"seconds": round(sorted(totals)[len(totals) // 2], 3) if totals else None,
                "all_s": [round(t, 3) for t in totals], "errors": errors, "peak_mb": peaks,
                "identical_to_agent_stl": same}
    finally:
        shutil.rmtree(d, ignore_errors=True)


def copy_tree(src, dst):
    for f in src.rglob("*"):
        if f.is_file() and f.stat().st_size <= MAX_KEEP_BYTES:
            t = dst / f.relative_to(src)
            t.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(f, t)


def run_one(task, cond, rep, args, rundir):
    keep = rundir / f"{task}-{cond}-{rep}"
    (keep / "history").mkdir(parents=True, exist_ok=True)
    workdir = Path(tempfile.mkdtemp(prefix=f"cad-{task}-{cond}-")).resolve()
    (workdir / "out").mkdir()
    aux = Path(tempfile.mkdtemp(prefix=f"cad-aux-{task}-{cond}-")).resolve()
    cmd, env = command(task, cond, workdir, args, keep, aux)
    (keep / "command.json").write_text(json.dumps(cmd, indent=1))
    log(f"{task} {cond} #{rep} in {workdir} ...")
    t0 = time.monotonic()
    lines = []
    transcript = open(keep / "transcript.jsonl", "w")
    stderr = open(keep / "stderr.txt", "w")
    p = subprocess.Popen(cmd, cwd=workdir, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=stderr,
                         text=True, start_new_session=True, env=env)
    g = guard.Guard(p.pid, args.mem_limit_mb, exempt=(os.path.realpath(args.claude),), exempt_limit_mb=4096, log=log)
    w = Watcher(workdir, keep / "history", t0)

    def read():
        for line in p.stdout:
            lines.append((round(time.monotonic() - t0, 2), line))
            transcript.write(line)
            transcript.flush()

    reader = threading.Thread(target=read, daemon=True)
    reader.start()
    timed_out = False
    try:
        p.wait(timeout=args.timeout_min * 60)
    except subprocess.TimeoutExpired:
        timed_out = True
        log(f"  timed out after {args.timeout_min} min; stopping the run")
        g.kill_tree()
        p.wait()
    wall = time.monotonic() - t0
    reader.join(timeout=10)
    g.stop()
    w.stop()
    transcript.close()
    stderr.close()
    (keep / "arrivals.json").write_text(json.dumps([t for t, _ in lines]))

    acc = account(lines, cond, args)
    parts = SPEC["tasks"][task]["parts"]
    vers = versions_from(w.events, acc["calls"], parts)
    by_id = {c["id"]: c for c in acc["calls"]}
    for i, v in enumerate(vers, 1):
        v["n"] = i
        gr = grade_files(task, {p: str(keep / "history" / c) for p, c in v["state"].items()}, keep)
        call = by_id.get(v["call"]) or {}
        v["tool_error"] = bool(call.get("cad_error"))
        v["clean"] = gr.get("clean")
        present_ok = all(gr.get("parts", {}).get(p, {}).get("clean") for p in v["state"])
        complete = len(v["state"]) == len(parts)
        v["pass"] = gr.get("pass")
        v["wrong"] = (not present_ok) or (complete and not gr.get("pass"))
        v["failed_gates"] = gr.get("failed_gates")
        (keep / f"grade-v{i:02d}.json").write_text(json.dumps(gr, indent=1))
    silent = [v["n"] for v in vers if v["wrong"] and not v["tool_error"]]

    final_files = {p: str(workdir / "out" / f"{p}.stl") for p in parts}
    final = grade_files(task, final_files, keep)
    (keep / "grade-final.json").write_text(json.dumps(final, indent=1))
    rec = recompute(task, cond, workdir, args) if any(Path(f).exists() for f in final_files.values()) else None
    loc = line_count(workdir, task, cond)
    copy_tree(workdir, keep / "work")
    if not args.keep_workdir:
        shutil.rmtree(workdir, ignore_errors=True)
    shutil.rmtree(aux, ignore_errors=True)
    s = acc["summary"]
    (keep / "calls.json").write_text(json.dumps(acc["calls"], indent=1))
    run = {
        "task": task, "condition": cond, "rep": rep,
        "pass": final.get("pass"), "clean": final.get("clean"),
        "gates": f"{final.get('gates_passed')}/{final.get('gates_total')}",
        "failed_gates": final.get("failed_gates"),
        "versions": len(vers),
        "versions_over_cap": len(vers) > 12,
        "tool_errors": s["cad_tool_errors"],
        "silent_wrong_versions": len(silent),
        "silent_wrong_version_numbers": silent,
        "final_failed_checks": len(final.get("failed_gates") or []) + (0 if final.get("clean") else 1),
        "recompute_s": rec and rec["seconds"], "recompute": rec,
        "wall_s": round(wall, 1), "timed_out": timed_out,
        "guard": {"peak_mb": g.peak_mb, "killed": g.killed},
        "lines_of_code": loc["lines"], "sources": loc["files"],
        "stray_stls": sorted({e["path"] for e in w.events if not e["path"].startswith("out/")}),
        "version_detail": [{k: v[k] for k in ("n", "t", "changed", "state", "tool_error", "clean", "pass",
                                                "wrong", "failed_gates")} for v in vers],
        **{k: s[k] for k in ("tool_calls", "tools", "cad_tool_calls", "all_error_results", "turns", "tokens",
                             "tokens_total", "tokens_excl_cache_reads", "cost_usd", "api_ms", "stop", "is_error",
                             "result_events")},
        "final_message": s["final_message"][:4000],
        "followup_messages": [m[:2000] for m in s["followup_messages"]],
        "workdir": str(workdir),
    }
    log(f"  {'PASS' if run['pass'] else 'FAIL'} clean={run['clean']} gates {run['gates']} "
        f"versions {run['versions']} tool errors {run['tool_errors']} silent {run['silent_wrong_versions']} "
        f"recompute {run['recompute_s']}s wall {run['wall_s']}s tokens {run['tokens_total']} ${run['cost_usd']}")
    if run["failed_gates"]:
        log(f"  failed: {run['failed_gates']}")
    return run


def history_states(keep, detail, parts):
    """Each version's {part: history copy}, rebuilt for a record written
    before version_detail kept `state`: the copies of out/<part>.stl in
    arrival order, repeats of the same bytes dropped (as versions_from
    does), are handed to the versions in order, one per part each version
    changed. None when the counts disagree."""
    wanted = {f"out__{p}.stl": p for p in parts}
    arrivals, last = [], {}
    for f in sorted((keep / "history").iterdir()):
        part = wanted.get(f.name.split("-", 1)[-1])
        if part is None:
            continue
        h = hashlib.sha1(f.read_bytes()).hexdigest()
        if last.get(part) != h:
            last[part] = h
            arrivals.append((part, f.name))
    if len(arrivals) != sum(len(v["changed"]) for v in detail):
        return None
    states, state, it = [], {}, iter(arrivals)
    for v in detail:
        for want in v["changed"]:
            part, copy = next(it)
            if part != want:
                return None
            state[part] = copy
        states.append(dict(state))
    return states


def reaccount(r, keep):
    """Re-derives a run's result-event figures (turns, tokens, cost, final
    message) from its saved transcript with the current result_summary,
    so a regrade also carries accounting fixes. Tool calls and tool errors
    are left as recorded; the old figures go to r["account_at_run"]."""
    path = keep / "transcript.jsonl"
    if not path.exists():
        return
    results = []
    for line in path.read_text().splitlines():
        try:
            m = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(m, dict) and m.get("type") == "result":
            results.append(m)
    s = result_summary(results)
    keys = ("turns", "tokens", "tokens_total", "tokens_excl_cache_reads", "cost_usd", "api_ms", "stop",
            "is_error", "result_events")
    changed = {k: r[k] for k in keys + ("final_message",)
               if k in r and r[k] != (s[k][:4000] if k == "final_message" else s[k])}
    if changed:
        r["account_at_run"] = changed
    r.update({k: s[k] for k in keys})
    r["final_message"] = s["final_message"][:4000]
    r["followup_messages"] = [m[:2000] for m in s["followup_messages"]]


def regrade(name, args):
    """Grades a record's saved STLs again (every version and the final
    parts) with the current grader. The original record and its grade
    files are left as they were: the new record goes alongside as
    <name>-regrade-<ts>.json, with each run's grades in
    <name>/<run>/regrade-<ts>/, so a grader fix can be checked against
    what was published."""
    path = args.out / f"{name}.json"
    record = json.loads(path.read_text())
    ts = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    for r in record["runs"]:
        keep = args.out / name / f"{r['task']}-{r['condition']}-{r['rep']}"
        out = keep / f"regrade-{ts}"
        out.mkdir()
        parts = SPEC["tasks"][r["task"]]["parts"]
        detail = r.get("version_detail", [])
        states = ([v["state"] for v in detail] if all("state" in v for v in detail)
                  else history_states(keep, detail, parts))
        r["grade_at_run"] = {k: r.get(k) for k in ("pass", "clean", "failed_gates", "gates",
                                                    "silent_wrong_versions", "final_failed_checks")}
        r["grade_at_run"]["version_detail"] = [dict(v) for v in detail]
        reaccount(r, keep)
        if states is None:
            log(f"{r['task']} {r['condition']} #{r['rep']}: history does not match the versions; "
                "version grades kept as recorded")
            r["versions_regraded"] = False
        else:
            for i, (v, st) in enumerate(zip(detail, states), 1):
                gr = grade_files(r["task"], {p: str(keep / "history" / c) for p, c in st.items()}, keep)
                present_ok = all(gr.get("parts", {}).get(p, {}).get("clean") for p in st)
                v.update(state=st, clean=gr.get("clean"), failed_gates=gr.get("failed_gates"),
                         wrong=(not present_ok) or (len(st) == len(parts) and not gr.get("pass")))
                v["pass"] = gr.get("pass")
                (out / f"grade-v{i:02d}.json").write_text(json.dumps(gr, indent=1))
            r["silent_wrong_versions"] = sum(1 for v in detail if v["wrong"] and not v["tool_error"])
            r["versions_regraded"] = True
        g = grade_files(r["task"], {p: str(keep / "work" / "out" / f"{p}.stl") for p in parts}, keep)
        if g.get("pass") != r["pass"]:
            log(f"{r['task']} {r['condition']} #{r['rep']}: {r['pass']} -> {g.get('pass')}")
        r["pass"], r["clean"], r["failed_gates"] = g.get("pass"), g.get("clean"), g.get("failed_gates")
        r["gates"] = f"{g.get('gates_passed')}/{g.get('gates_total')}"
        r["final_failed_checks"] = len(g.get("failed_gates") or []) + (0 if g.get("clean") else 1)
        (out / "grade-final.json").write_text(json.dumps(g, indent=1))
    record["regraded"] = ts
    record["regraded_from"] = path.name
    new = args.out / f"{name}-regrade-{ts}.json"
    new.write_text(json.dumps(record, indent=1))
    log(f"regraded {path} -> {new}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--neoscad", default=str(ROOT / "target" / "release" / "neoscad"),
                    help="the neoscad binary for the MCP condition and its recompute timing")
    ap.add_argument("--openscad", default=OPENSCAD)
    ap.add_argument("--venv", type=Path, default=VENV, help="the CadQuery venv (setup-cadquery.sh)")
    ap.add_argument("--model", default="claude-opus-5[1m]",
                    help="ModelRift ran Claude Opus 5 with 1M context")
    ap.add_argument("--tasks", default="T1,T2,T3")
    ap.add_argument("--conditions", default="openscad,cadquery,neoscad")
    ap.add_argument("--n", type=int, default=1, help="runs per cell")
    ap.add_argument("--max-turns", type=int, default=200,
                    help="a backstop; the 12-version cap in the prompt is the real budget")
    ap.add_argument("--max-budget-usd", type=float, default=20.0, help="per run")
    ap.add_argument("--timeout-min", type=float, default=45)
    ap.add_argument("--mem-limit-mb", type=int, default=guard.DEFAULT_LIMIT_MB,
                    help="per geometry process (Claude Code itself: 4096)")
    ap.add_argument("--claude", default=shutil.which("claude") or "claude")
    ap.add_argument("--out", type=Path, default=ROOT / "progress" / "agent-eval")
    ap.add_argument("--keep-workdir", action="store_true", help="leave the temp dirs (they are copied anyway)")
    ap.add_argument("--print-commands", action="store_true")
    ap.add_argument("--regrade", metavar="NAME")
    args = ap.parse_args()
    args.python = args.venv / "bin" / "python"
    args.out.mkdir(parents=True, exist_ok=True)
    if args.regrade:
        regrade(args.regrade, args)
        return
    tasks = args.tasks.split(",")
    conds = args.conditions.split(",")
    for t in tasks:
        if t not in SPEC["tasks"]:
            sys.exit(f"unknown task {t}")
    for c in conds:
        if c not in SPEC["conditions"]:
            sys.exit(f"unknown condition {c}")
    if "neoscad" in conds and not Path(args.neoscad).exists():
        sys.exit(f"no neoscad at {args.neoscad} (pass --neoscad)")
    if "cadquery" in conds and not args.python.exists():
        sys.exit(f"no CadQuery venv at {args.venv} (scripts/agent-eval/cad/setup-cadquery.sh)")
    if "openscad" in conds and not Path(args.openscad).exists():
        sys.exit(f"no OpenSCAD at {args.openscad}")
    args.neoscad = str(Path(args.neoscad).resolve())

    if args.print_commands:
        d = Path(tempfile.mkdtemp(prefix="cad-print-"))
        for t in tasks:
            for c in conds:
                k = d / f"{t}-{c}"
                k.mkdir()
                print(json.dumps(command(t, c, Path("/WORKDIR"), args, k)[0]))
        shutil.rmtree(d)
        return

    ts = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    name = f"cad-{ts}"
    rundir = args.out / name
    rundir.mkdir()
    sha = git("rev-parse", "--short", "HEAD")
    record = {"name": name, "timestamp": ts, "sha": git("rev-parse", "HEAD"),
              "dirty": bool(git("status", "--porcelain")), "model": args.model, "n": args.n,
              "max_turns": args.max_turns, "max_budget_usd": args.max_budget_usd,
              "timeout_min": args.timeout_min, "mem_limit_mb": args.mem_limit_mb,
              "versions": tool_versions(args), "neoscad_path": args.neoscad,
              "prompt_template": SPEC["prompt"], "runs": []}
    log(f"{name} ({sha}) model {args.model}: {len(tasks)} tasks x {len(conds)} conditions x {args.n}")
    path = args.out / f"{name}.json"
    for rep in range(1, args.n + 1):
        for t in tasks:
            for c in conds:
                record["runs"].append(run_one(t, c, rep, args, rundir))
                path.write_text(json.dumps(record, indent=1))
    with open(args.out / "cad-index.jsonl", "a") as f:
        f.write(json.dumps({"name": name, "model": args.model, "tasks": tasks, "conditions": conds, "n": args.n,
                            "pass": {c: f"{sum(bool(r['pass']) for r in record['runs'] if r['condition'] == c)}/"
                                        f"{sum(1 for r in record['runs'] if r['condition'] == c)}" for c in conds},
                            "cost_usd": round(sum(r["cost_usd"] or 0 for r in record["runs"]), 4)}) + "\n")
    log(f"\nwrote {path}\nsummary: {HERE / 'summarize.py'} {path}")


if __name__ == "__main__":
    main()
