"""Progress snapshots of CAD comparison runs: what each agent's model looked
like as the run went on, for the side-by-side hero image (hero.py,
docs/agent-eval.md, "Progress snapshots and the hero image").

This module is stdlib only and does no rendering. It holds the logic that
the capture (run_cad.py), the backfill and the composer share, so the unit
tests (test_progress.py) can check it on synthetic runs:

- `stream_points`: turn and output-token counts as the stream-json events
  arrived, on the harness's own clock;
- `replay_source_writes`: a run's source files rebuilt from the Write and
  Edit calls in its transcript (for runs recorded before the harness
  snapshotted sources);
- `build_progress`: the `progress.json` record;
- `representative`: the one run per condition that the hero shows;
- the time-axis layout: `state_at`, `sample_times`, `axis_x`,
  `animation_clock`, `fmt_clock`.

Times are seconds since the run started, on the harness's monotonic clock
(the reader thread stamps every stream line as it arrives; the watcher
stamps every file copy). Nothing here reads the wall clock.
"""

import json
import math

SCHEMA = 1
CONDITION_ORDER = ("openscad", "cadquery", "neoscad")
LABEL = {"openscad": "OpenSCAD", "cadquery": "CadQuery", "neoscad": "NeoSCAD"}
SOURCE_EXTS = (".scad", ".py")
WRITE_TOOLS = ("Write", "Edit", "MultiEdit")


# ---------------------------------------------------------------------------
# Stream accounting


def _events(lines):
    for t, line in lines:
        try:
            m = json.loads(line)
        except (json.JSONDecodeError, TypeError):
            continue
        if isinstance(m, dict):
            yield t, m


def stream_points(lines):
    """[(t, json line)] -> one point per assistant API response, in arrival
    order: {"t", "turn", "output_tokens"}, plus one {"t", "result": True,
    "turns", "cost_usd"} per result event.

    `turn` counts distinct assistant message ids so far (one per model
    response); Claude Code's own `num_turns` is only in the result events,
    and cost likewise: the stream carries no cost until a result, so
    cost-so-far is known only at the end. A message's usage repeats on
    every content block of that message, so its output tokens are taken
    from the last block seen, not summed, which would count them twice."""
    out_tokens, points, last = {}, [], None
    for t, m in _events(lines):
        kind = m.get("type")
        if kind == "assistant":
            msg = m.get("message") if isinstance(m.get("message"), dict) else {}
            mid = msg.get("id") or m.get("uuid")
            n = (msg.get("usage") or {}).get("output_tokens") or 0
            if mid not in out_tokens:
                out_tokens[mid] = n
                last = {"t": t, "turn": len(out_tokens), "output_tokens": sum(out_tokens.values())}
                points.append(last)
            else:
                # A later block of a message already counted: its usage
                # supersedes the earlier block's, and the newest point
                # carries the running total.
                out_tokens[mid] = max(out_tokens[mid], n)
                last["output_tokens"] = sum(out_tokens.values())
        elif kind == "result":
            points.append({"t": t, "result": True, "turns": m.get("num_turns"),
                           "cost_usd": m.get("total_cost_usd")})
    return points


def point_at(points, t):
    """The last assistant point at or before t, or None."""
    best = None
    for p in points:
        if p.get("result"):
            continue
        if p["t"] <= t:
            best = p
        else:
            break
    return best


def workdir_of(lines):
    """The run directory from the stream's init event (`cwd`)."""
    for _, m in _events(lines):
        if m.get("type") == "system" and m.get("subtype") == "init":
            return m.get("cwd")
    return None


def _relative(path, workdir):
    if workdir and path.startswith(workdir.rstrip("/") + "/"):
        return path[len(workdir.rstrip("/")) + 1:]
    # The init event's cwd can name the temp directory through macOS's
    # /var symlink while tool inputs use its resolved target (or the other
    # way round), and a plain prefix test would then drop every write; fall
    # back on the part after the temp directory's own (unique) name.
    if workdir:
        name = workdir.rstrip("/").rsplit("/", 1)[-1]
        marker = "/" + name + "/"
        if marker in path:
            return path.split(marker, 1)[1]
    return None


def replay_source_writes(lines, exts=SOURCE_EXTS):
    """Rebuilds every version of the run's source files from its transcript:
    each Write, Edit and MultiEdit whose result was not an error, applied in
    order. Returns [{"t", "path", "text", "via", "call"}], `t` being the
    arrival of the tool's result (when the file had changed), `text` None
    when an Edit could not be replayed (the file was made some other way,
    e.g. by a shell heredoc, so its earlier text is unknown).

    Files written through Bash are not seen at all; the caller compares the
    last replayed text with the saved final file to say so."""
    workdir = workdir_of(lines)
    pending, files, out = {}, {}, []
    for t, m in _events(lines):
        kind = m.get("type")
        msg = m.get("message")
        content = msg.get("content") if isinstance(msg, dict) else None
        if not isinstance(content, list):
            continue
        if kind == "assistant":
            for c in content:
                if isinstance(c, dict) and c.get("type") == "tool_use" and c.get("name") in WRITE_TOOLS:
                    pending[c.get("id")] = (c["name"], c.get("input") or {})
        elif kind == "user":
            for c in content:
                if not (isinstance(c, dict) and c.get("type") == "tool_result" and c.get("tool_use_id") in pending):
                    continue
                name, inp = pending.pop(c["tool_use_id"])
                if c.get("is_error"):
                    continue
                rel = _relative(str(inp.get("file_path", "")), workdir)
                if rel is None or not rel.endswith(exts):
                    continue
                if name == "Write":
                    text = inp.get("content")
                else:
                    text = files.get(rel)
                    edits = inp.get("edits") if name == "MultiEdit" else [inp]
                    for e in edits or []:
                        if text is None:
                            break
                        old, new = e.get("old_string", ""), e.get("new_string", "")
                        if not old or old not in text:
                            text = None
                        elif e.get("replace_all"):
                            text = text.replace(old, new)
                        else:
                            text = text.replace(old, new, 1)
                files[rel] = text
                out.append({"t": t, "path": rel, "text": text, "via": name, "call": c["tool_use_id"]})
    return out


# ---------------------------------------------------------------------------
# The record


def build_progress(*, task, condition, rep, parts, wall_s, timed_out, passed, versions, sources, points,
                   timing, stl_root=".", record=None, extra=None):
    """The progress.json record of one run.

    versions: [{"n", "t", "state": {part: path under stl_root}, "changed"}],
    one per tool call that wrote new bytes to out/<part>.stl (run_cad's
    versions_from). sources: [{"t", "path", "copy" (under the record's
    directory) or None, "lines", "via"}]. points: stream_points(). timing:
    how exact the times are, in words, per kind ("versions", "sources",
    "stream"), so a reader of a backfilled record cannot mistake its
    precision."""
    vs = []
    for v in versions:
        p = point_at(points, v["t"])
        vs.append({"n": v["n"], "t": v["t"], "state": dict(v["state"]), "changed": list(v.get("changed", [])),
                   "complete": len(v["state"]) == len(parts),
                   "turn": p and p["turn"], "output_tokens": p and p["output_tokens"]})
    results = [p for p in points if p.get("result")]
    asst = [p for p in points if not p.get("result")]
    return {
        "schema": SCHEMA, "task": task, "condition": condition, "rep": rep, "record": record,
        "parts": list(parts), "wall_s": wall_s, "timed_out": bool(timed_out), "pass": passed,
        "timing": timing, "stl_root": str(stl_root),
        "versions": vs,
        "sources": sources,
        "stream": [{"t": p["t"], "turn": p["turn"], "output_tokens": p["output_tokens"]} for p in asst],
        "final": {"t": wall_s, "turns": sum(r.get("turns") or 0 for r in results) if results else None,
                  "cost_usd": results[-1].get("cost_usd") if results else None,
                  "assistant_messages": asst[-1]["turn"] if asst else 0},
        **(extra or {}),
    }


# ---------------------------------------------------------------------------
# Selection


RULE = ("per task and condition: the passing run with the median wall time (the lower median for an even "
        "count, ties to the smaller id); with no passing run, the median of all runs")


def representative(runs):
    """The run that represents a condition, by a fixed rule, never by hand:
    among the passing runs, the one with the median wall time; with no
    passing run, the median of all runs. With an even count the lower
    median is taken (the faster of the two middle runs), and ties in wall
    time go to the smaller id, so the choice is deterministic. A run with
    no wall time sorts last.

    runs: [{"id", "pass", "wall_s", ...}]. Returns (run, rule) or
    (None, None) for an empty list; `rule` says which branch applied."""
    if not runs:
        return None, None
    passing = [r for r in runs if r.get("pass")]
    pool, rule = (passing, "median wall time of the passing runs") if passing else \
        (list(runs), "no passing run: median wall time of all runs")
    pool = sorted(pool, key=lambda r: (r.get("wall_s") is None, r.get("wall_s") or 0, str(r.get("id"))))
    return pool[(len(pool) - 1) // 2], f"{rule} ({len(pool)} of {len(runs)})"


# ---------------------------------------------------------------------------
# Time-axis layout


def fmt_clock(s):
    """Seconds as m:ss, or h:mm:ss from an hour on."""
    if s is None:
        return "–"
    s = max(0, int(round(s)))
    h, rem = divmod(s, 3600)
    m, sec = divmod(rem, 60)
    return f"{h}:{m:02d}:{sec:02d}" if h else f"{m}:{sec:02d}"


def shared_t_max(runs):
    """The shared axis ends at the slowest selected run's wall time, so a
    bar's length is its run's time and every column reads off one clock."""
    walls = [r["wall_s"] for r in runs if r and r.get("wall_s")]
    return max(walls) if walls else 1.0


def state_at(versions, t):
    """The newest version written at or before t (versions sorted by t),
    or None while the run has none yet."""
    best = None
    for v in versions:
        if v["t"] <= t:
            best = v
        else:
            break
    return best


def sample_times(t_max, k):
    """k clock times strictly inside (0, t_max], evenly spaced and shared by
    every column: t_max * i / k for i = 1..k. The last is t_max itself,
    where every run has finished."""
    if k <= 0:
        return []
    return [t_max * i / k for i in range(1, k + 1)]


def axis_x(t, t_max, x0, width):
    """Pixel x of clock time t on an axis from x0 (t = 0) to x0 + width
    (t = t_max), clamped to the axis."""
    if t_max <= 0:
        return x0
    f = min(max(t / t_max, 0.0), 1.0)
    return x0 + int(round(f * width))


def animation_clock(t_max, duration_s, fps, hold_s):
    """The clock time of every animation frame: a linear sweep from 0 to
    t_max over duration_s, then hold_s of frames at t_max so the finished
    columns can be read. Linear, so the gap between two columns' finishes
    is proportional to the gap in their wall times."""
    n = max(2, int(round(duration_s * fps)))
    clock = [t_max * i / (n - 1) for i in range(n)]
    return clock + [t_max] * int(round(hold_s * fps))


def speedup(t_max, duration_s):
    """How many run seconds one second of the animation shows."""
    return t_max / duration_s if duration_s > 0 else math.inf
