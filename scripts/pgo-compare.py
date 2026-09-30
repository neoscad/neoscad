#!/usr/bin/env python3
"""Compare two `conformance bench` results: a PGO build against the plain one.

    scripts/pgo-compare.py PLAIN.json PGO.json [--out SUMMARY.md]

Each file is a result `conformance bench --refs neoscad --binary ...`
wrote under progress/bench/. Prints a Markdown table of neoscad's best
wall time per model in both and their ratio (PGO / plain, below 1 is
faster), and the geometric mean of the ratios over the models where both
took at least 30 ms, since shorter runs are mostly process start-up and
noise (perf-opportunities.md, P2, reports the same mean). Models that
failed or timed out in either run are listed, not averaged.
"""
import json
import math
import sys
from pathlib import Path

FLOOR_S = 0.030


def best(result):
    """model id -> neoscad's best wall time in seconds, or None."""
    out = {}
    for mid, m in result.get("models", {}).items():
        r = m.get("results", {}).get("neoscad") or {}
        out[mid] = r.get("best_s")
    return out


def main(argv):
    args = [a for a in argv if not a.startswith("--")]
    out = None
    if "--out" in argv:
        out = Path(argv[argv.index("--out") + 1])
        args.remove(str(out))
    if len(args) != 2:
        sys.exit(__doc__)
    plain_doc, pgo_doc = (json.loads(Path(a).read_text()) for a in args)
    plain, pgo = best(plain_doc), best(pgo_doc)
    rows, ratios, skipped = [], [], []
    for mid in sorted(set(plain) | set(pgo)):
        a, b = plain.get(mid), pgo.get(mid)
        if not a or not b:
            skipped.append(mid)
            continue
        r = b / a
        rows.append(f"| {mid} | {a:.4f} | {b:.4f} | {r:.3f} |")
        if min(a, b) >= FLOOR_S:
            ratios.append(r)
    lines = [
        "| model | plain s | PGO s | PGO / plain |",
        "|---|---:|---:|---:|",
        *rows,
        "",
    ]
    if ratios:
        gm = math.exp(sum(math.log(r) for r in ratios) / len(ratios))
        lines.append(f"Geometric mean over {len(ratios)} models of 30 ms or more: **{gm:.3f}**")
    else:
        lines.append("No model took 30 ms or more in both runs; no mean.")
    if skipped:
        lines.append(f"Not compared (failed or timed out in a run): {', '.join(skipped)}")
    machine = pgo_doc.get("machine", {})
    if machine:
        lines.append(f"Machine: {json.dumps(machine, sort_keys=True)}")
    text = "\n".join(lines) + "\n"
    print(text, end="")
    if out:
        out.write_text(text)


if __name__ == "__main__":
    main(sys.argv[1:])
