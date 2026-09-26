#!/usr/bin/env python3
"""Agent-loop eval: agents do modeling tasks with NeoSCAD's MCP server (A)
or with Bash and the OpenSCAD command line (B); hidden graders score them.

    scripts/agent-eval/run.py --check-graders        # graders pass their references
    scripts/agent-eval/run.py [--model sonnet] [--tasks box_lid,gears]
                              [--conditions A,B] [--max-turns 40] [--dry-run]

Tasks are in conformance/agent-tasks.json (docs/agent-eval.md). Each run is
headless `claude -p` in its own temporary directory, one at a time, with
the same prompt and turn budget in both conditions; only the tools and one
appended system-prompt line (what the tools are) differ. After the run the
task's grader, a `neoscad test` file the agent never saw, is copied next to
the agent's model.scad and run. Results go to
progress/agent-eval/<ts>-<sha>.json (gitignored) with transcripts and models
in progress/agent-eval/<ts>-<sha>/, and one line per eval in
progress/agent-eval/index.jsonl.
"""

import argparse
import datetime
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
NEOSCAD = ROOT / "target" / "release" / "neoscad"
TASKS = ROOT / "conformance" / "agent-tasks.json"
OUT = ROOT / "progress" / "agent-eval"
MCP_TOOLS = ["evaluate", "render", "snapshot", "check", "measure", "test", "format", "docs"]
# A run that has not finished in this long is stopped and scored as it is.
RUN_TIMEOUT_S = 1800


def git(*args):
    return subprocess.run(["git", "-C", str(ROOT), *args], capture_output=True, text=True).stdout.strip()


def grade(task, workdir):
    """Run the task's hidden grader on workdir/model.scad."""
    model = workdir / "model.scad"
    if not model.exists():
        return {"pass": False, "reason": "no model.scad", "tests": []}
    shutil.copy(ROOT / "conformance" / task["grader"], workdir / "grade_test.scad")
    env = dict(os.environ, NEOSCAD_NO_SERVER="1")
    p = subprocess.run(
        [str(NEOSCAD), "test", "grade_test.scad", "--format", "json"],
        cwd=workdir, capture_output=True, text=True, env=env, timeout=600,
    )
    try:
        r = json.loads(p.stdout)
    except json.JSONDecodeError:
        return {"pass": False, "reason": f"grader output: {p.stderr[-500:]}", "tests": []}
    tests = [
        {"name": t.get("name"), "ok": t["ok"], "failures": [f["message"] for f in t["failures"]]}
        for t in r.get("tests", [])
    ]
    return {
        "pass": r.get("exit_code") == 0,
        "passed": r["counts"]["passed"],
        "total": r["counts"]["tests"],
        "reason": r.get("error"),
        "tests": tests,
    }


def check_graders(spec):
    ok = True
    for task in spec["tasks"]:
        d = Path(tempfile.mkdtemp(prefix=f"neoscad-grader-{task['id']}-")).resolve()
        shutil.copy(ROOT / "conformance" / task["reference"], d / "model.scad")
        g = grade(task, d)
        ok &= g["pass"]
        print(f"{task['id']:14} {'ok' if g['pass'] else 'FAILED'} ({g.get('passed')}/{g.get('total')})")
        for t in g["tests"]:
            if not t["ok"]:
                print(f"    {t['name']}: {'; '.join(t['failures'])}")
        shutil.rmtree(d, ignore_errors=True)
    return ok


def regrade(spec, name):
    """Grade an eval's saved models again with the current graders. The
    record keeps the first grade as `grade_at_run`, so a changed verdict
    stays visible."""
    path = OUT / f"{name}.json"
    record = json.loads(path.read_text())
    tasks = {t["id"]: t for t in spec["tasks"]}
    for r in record["runs"]:
        saved = OUT / name / f"{r['task']}-{r['condition']}"
        d = Path(tempfile.mkdtemp(prefix="neoscad-regrade-")).resolve()
        if (saved / "model.scad").exists():
            shutil.copy(saved / "model.scad", d / "model.scad")
        g = grade(tasks[r["task"]], d)
        shutil.rmtree(d, ignore_errors=True)
        r.setdefault("grade_at_run", r["grade"])
        if g["pass"] != r["pass"]:
            print(f"{r['task']} {r['condition']}: {r['pass']} -> {g['pass']}")
        r["grade"], r["pass"] = g, g["pass"]
    record["regraded"] = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    path.write_text(json.dumps(record, indent=1))
    index(record, regraded=record["regraded"])
    table(record["runs"])


def index(record, **extra):
    """One line for the eval in index.jsonl (a regrade adds another)."""
    runs = record["runs"]
    conds = sorted({r["condition"] for r in runs})
    by = {c: [r for r in runs if r["condition"] == c] for c in conds}
    line = {"name": record["name"], "model": record["model"],
            "tasks": sorted({r["task"] for r in runs}),
            **{f"{c}_pass": f"{sum(r['pass'] for r in rs)}/{len(rs)}" for c, rs in by.items()},
            **{f"{c}_cost_usd": round(sum(r["cost_usd"] or 0 for r in rs), 4) for c, rs in by.items()},
            **extra}
    with open(OUT / "index.jsonl", "a") as f:
        f.write(json.dumps(line) + "\n")


def table(runs):
    print("\n| task | cond | pass | tests | tool calls | turns | input tok | output tok | cost $ | wall s |")
    print("|---|---|---|---|---|---|---|---|---|---|")
    for r in runs:
        g = r["grade"]
        print(f"| {r['task']} | {r['condition']} | {'yes' if r['pass'] else 'no'} | "
              f"{g.get('passed')}/{g.get('total')} | {r['tool_calls']} | {r['turns']} | "
              f"{r['input_tokens_total']} | {r['tokens']['output']} | {r['cost_usd']} | {r['wall_s']} |")


def command(spec, task, cond, workdir, args, mcp_config):
    prompt = f"{task['prompt']}\n\n{spec['common']}"
    cmd = [
        args.claude, "-p", prompt,
        "--model", args.model,
        "--output-format", "stream-json", "--verbose",
        "--max-turns", str(args.max_turns),
        "--append-system-prompt", spec["conditions"][cond],
        # Only this run's MCP servers, and none of the user's settings,
        # hooks or permissions: both conditions start from the same place.
        "--strict-mcp-config",
        "--setting-sources", "project",
        "--no-session-persistence",
    ]
    if cond == "A":
        mcp_config.write_text(json.dumps({"mcpServers": {"neoscad": {
            "command": str(NEOSCAD), "args": ["mcp", "--root", str(workdir)]}}}))
        allowed = ["Read", "Write", "Edit"] + [f"mcp__neoscad__{t}" for t in MCP_TOOLS]
        cmd += ["--mcp-config", str(mcp_config), "--tools", "Read,Write,Edit",
                "--allowedTools", ",".join(allowed)]
    else:
        cmd += ["--tools", "Bash,Read", "--allowedTools", "Bash,Read"]
    return cmd


def summarise(lines):
    """Tool calls, turns, tokens and cost from a stream-json transcript."""
    tools = {}
    result = {}
    for line in lines:
        try:
            m = json.loads(line)
        except json.JSONDecodeError:
            continue
        if m.get("type") == "assistant":
            for c in m["message"].get("content", []):
                if c.get("type") == "tool_use":
                    name = c["name"].removeprefix("mcp__neoscad__")
                    tools[name] = tools.get(name, 0) + 1
        elif m.get("type") == "result":
            result = m
    usage = {"input": 0, "cache_read": 0, "cache_write": 0, "output": 0}
    for u in (result.get("modelUsage") or {}).values():
        usage["input"] += u.get("inputTokens", 0)
        usage["cache_read"] += u.get("cacheReadInputTokens", 0)
        usage["cache_write"] += u.get("cacheCreationInputTokens", 0)
        usage["output"] += u.get("outputTokens", 0)
    return {
        "tool_calls": sum(tools.values()),
        "tools": tools,
        "turns": result.get("num_turns"),
        "tokens": usage,
        "input_tokens_total": usage["input"] + usage["cache_read"] + usage["cache_write"],
        "cost_usd": result.get("total_cost_usd"),
        "stop": result.get("subtype"),
        "is_error": result.get("is_error"),
        "final": (result.get("result") or "")[:300],
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--model", default="sonnet")
    ap.add_argument("--tasks", help="comma-separated task ids (default: all)")
    ap.add_argument("--conditions", default="A,B")
    ap.add_argument("--max-turns", type=int, default=40)
    ap.add_argument("--claude", default=shutil.which("claude") or "claude")
    ap.add_argument("--check-graders", action="store_true")
    ap.add_argument("--dry-run", action="store_true", help="print the commands only")
    ap.add_argument("--regrade", metavar="NAME",
                    help="grade an earlier eval's saved models again (after fixing a grader)")
    args = ap.parse_args()

    spec = json.loads(TASKS.read_text())
    if not NEOSCAD.exists():
        sys.exit(f"build first: {NEOSCAD} is missing (cargo build --release)")
    if args.check_graders:
        sys.exit(0 if check_graders(spec) else 1)
    if args.regrade:
        regrade(spec, args.regrade)
        return

    wanted = args.tasks.split(",") if args.tasks else [t["id"] for t in spec["tasks"]]
    tasks = [t for t in spec["tasks"] if t["id"] in wanted]
    conds = args.conditions.split(",")
    ts = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    sha = git("rev-parse", "--short", "HEAD") + ("-dirty" if git("status", "--porcelain") else "")
    name = f"{ts}-{sha}"
    rundir = OUT / name
    if args.dry_run:
        rundir = Path(tempfile.mkdtemp(prefix="neoscad-eval-dry-"))
    rundir.mkdir(parents=True, exist_ok=True)

    runs = []
    for task in tasks:
        for cond in conds:
            keep = rundir / f"{task['id']}-{cond}"
            keep.mkdir(exist_ok=True)
            if args.dry_run:
                cmd = command(spec, task, cond, Path("WORKDIR"), args, keep / "mcp.json")
                print(" ".join(repr(c) if " " in c or "\n" in c else c for c in cmd))
                continue
            workdir = Path(tempfile.mkdtemp(prefix=f"neoscad-eval-{task['id']}-{cond}-")).resolve()
            cmd = command(spec, task, cond, workdir, args, keep / "mcp.json")
            print(f"{task['id']} {cond} ...", flush=True)
            t0 = time.monotonic()
            try:
                p = subprocess.run(cmd, cwd=workdir, stdin=subprocess.DEVNULL,
                                   capture_output=True, text=True, timeout=RUN_TIMEOUT_S)
                out, err, timed_out = p.stdout, p.stderr, False
            except subprocess.TimeoutExpired as e:
                out = (e.stdout or b"").decode() if isinstance(e.stdout, bytes) else (e.stdout or "")
                err, timed_out = "timeout", True
            wall = time.monotonic() - t0
            (keep / "transcript.jsonl").write_text(out)
            if err.strip():
                (keep / "stderr.txt").write_text(err)
            if (workdir / "model.scad").exists():
                shutil.copy(workdir / "model.scad", keep / "model.scad")
            g = grade(task, workdir)
            s = summarise(out.splitlines())
            run = {"task": task["id"], "condition": cond, "pass": g["pass"], "grade": g,
                   "wall_s": round(wall, 1), "timed_out": timed_out, "workdir": str(workdir), **s}
            runs.append(run)
            print(f"  {'PASS' if g['pass'] else 'FAIL'} {g.get('passed')}/{g.get('total')} "
                  f"tools {s['tool_calls']} turns {s['turns']} in {s['input_tokens_total']} "
                  f"out {s['tokens']['output']} ${s['cost_usd']} {wall:.0f}s", flush=True)
    if args.dry_run:
        shutil.rmtree(rundir, ignore_errors=True)
        return

    record = {"name": name, "timestamp": ts, "sha": git("rev-parse", "HEAD"), "dirty": sha.endswith("-dirty"),
              "model": args.model, "max_turns": args.max_turns,
              "claude": subprocess.run([args.claude, "--version"], capture_output=True, text=True).stdout.strip(),
              "runs": runs}
    (OUT / f"{name}.json").write_text(json.dumps(record, indent=1))
    index(record)

    table(runs)
    print(f"\nwrote {OUT / (name + '.json')}")


if __name__ == "__main__":
    main()
