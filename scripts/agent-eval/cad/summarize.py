#!/usr/bin/env python3
"""Markdown tables for the post from a CAD comparison record.

    summarize.py progress/agent-eval/cad-<ts>.json [more records...]

Prints ModelRift's table (metrics down, task x tool across), the grading
detail, and the method line (model, versions of every tool, n). With n > 1
per cell, each figure is the median with the range in brackets.
"""

import json
import sys
from pathlib import Path

LABEL = {"openscad": "OpenSCAD", "cadquery": "CadQuery", "neoscad": "NeoSCAD"}


def med(xs):
    xs = sorted(x for x in xs if x is not None)
    if not xs:
        return None
    m = len(xs) // 2
    return xs[m] if len(xs) % 2 else (xs[m - 1] + xs[m]) / 2


def fmt(xs, f):
    xs = [x for x in xs if x is not None]
    if not xs:
        return "–"
    if len(xs) == 1:
        return f(xs[0])
    return f"{f(med(xs))} [{f(min(xs))}–{f(max(xs))}]"


def tok(x):
    return f"{x / 1000:.0f}k"


def secs(x):
    return f"{x * 1000:.0f} ms" if x < 1 else f"{x:.2f} s"


def main():
    runs, records = [], []
    for p in sys.argv[1:]:
        r = json.loads(Path(p).read_text())
        records.append(r)
        runs += r["runs"]
    cells = []
    for t in sorted({r["task"] for r in runs}):
        for c in ("openscad", "cadquery", "neoscad"):
            rs = [r for r in runs if r["task"] == t and r["condition"] == c]
            if rs:
                cells.append((t, c, rs))
    head = "| Metric | " + " | ".join(f"{t} {LABEL[c]}" for t, c, _ in cells) + " |"
    print(head)
    print("|---" * (len(cells) + 1) + "|")
    rows = [
        ("Versions", lambda r: r["versions"], lambda x: f"{x:.0f}" if x == int(x) else f"{x:.1f}"),
        ("Lines of code", lambda r: r["lines_of_code"], lambda x: f"{x:.0f}"),
        ("Tool errors raised", lambda r: r["tool_errors"], lambda x: f"{x:.0f}"),
        ("Silent wrong geometry (versions)", lambda r: r["silent_wrong_versions"], lambda x: f"{x:.0f}"),
        ("Agent wall-clock", lambda r: r["wall_s"], lambda x: f"{x:.0f} s"),
        ("Tokens (excl. cache reads)", lambda r: r["tokens_excl_cache_reads"], tok),
        ("Tokens (total)", lambda r: r["tokens_total"], tok),
        ("Geometry recompute", lambda r: r["recompute_s"], secs),
        ("Cost", lambda r: r["cost_usd"], lambda x: f"${x:.2f}"),
    ]
    for name, get, f in rows:
        print(f"| {name} | " + " | ".join(fmt([get(r) for r in rs], f) for _, _, rs in cells) + " |")
    print("| Final STL clean | " + " | ".join(
        f"{sum(bool(r['clean']) for r in rs)}/{len(rs)}" for _, _, rs in cells) + " |")
    print("| Spec checks passed | " + " | ".join(", ".join(r["gates"] for r in rs) for _, _, rs in cells) + " |")

    print("\n**Grading detail.**\n")
    for t, c, rs in cells:
        for r in rs:
            fails = r.get("failed_gates") or []
            extra = []
            if not r["clean"]:
                extra.append("not clean")
            if r.get("timed_out"):
                extra.append("timed out")
            if (r.get("guard") or {}).get("killed"):
                extra.append("killed by the memory guard")
            if r.get("versions_over_cap"):
                extra.append("over the 12-version cap")
            if r.get("stray_stls"):
                extra.append(f"STLs outside out/: {', '.join(r['stray_stls'])}")
            detail = "; ".join(extra + [f"failed: {', '.join(fails)}"] if fails else extra) or "all checks pass"
            print(f"- {t} {LABEL[c]} #{r['rep']}: {detail}")

    print("\n**Method.**\n")
    for rec in records:
        v = rec["versions"]
        n = max(r["rep"] for r in rec["runs"])
        print(f"- `{rec['name']}` ({rec['sha'][:10]}{', dirty tree' if rec['dirty'] else ''}): model "
              f"`{rec['model']}`, n = {n} per cell, Claude Code {v.get('claude')}; OpenSCAD `{v.get('openscad')}` "
              f"(`--backend=manifold`); CadQuery/Python `{v.get('cadquery_python')}`; `{v.get('neoscad')}`. "
              f"Limits: {rec['timeout_min']} min and ${rec['max_budget_usd']} per run, "
              f"{rec['mem_limit_mb']} MB per geometry process.")
    print("- Recompute is the median of 3 command-line exports of the final sources, outside the agent "
          "(NeoSCAD: the CLI, a cold process; CadQuery: includes Python and CadQuery start-up).")
    print("- n = 1 per cell is an anecdote: agent variance alone can swing any cell.")


if __name__ == "__main__":
    main()
