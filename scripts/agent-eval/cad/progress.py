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
  `animation_clock`, `fmt_clock`;
- source frames for the .scad conditions: `source_versions` (which saved
  states get a frame, and which are incomplete), `enabled_features`
  (what a render may --enable) and `timeline` (STL and source versions
  on one clock).

Times are seconds since the run started, on the harness's monotonic clock
(the reader thread stamps every stream line as it arrives; the watcher
stamps every file copy). Nothing here reads the wall clock.
"""

import json
import math
import posixpath
import re

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


RULE = ("per task and condition: of the passing runs (all runs if none passes), the run whose wall time is "
        "closest to their median; ties to the slower run, then the smaller id")


def pool_stats(runs):
    """The pool a condition's representative is drawn from, and the
    figures the image prints beside it: the passing runs (all runs when
    none passes), how many there are, and the median of their wall times
    (the mean of the two middle times for an even count, as the post's
    table computes it). Runs without a wall time count in `n` but not in
    the median.

    Returns {"basis": "passing"|"all", "n", "of", "median_s"}; median_s is
    None when no run in the pool has a wall time."""
    passing = [r for r in runs if r.get("pass")]
    pool = passing or list(runs)
    walls = sorted(r["wall_s"] for r in pool if r.get("wall_s") is not None)
    k = len(walls)
    med = None if not k else (walls[k // 2] if k % 2 else (walls[k // 2 - 1] + walls[k // 2]) / 2)
    return {"basis": "passing" if passing else "all", "n": len(pool), "of": len(runs), "median_s": med}


def pool_caption(stats):
    """The pool line under a column's header ("median 4:07 of 2 passing"),
    so the image states the same median as the post's table even when the
    run it shows is not at that median."""
    med = fmt_clock(stats["median_s"])
    if stats["basis"] == "passing":
        return f"median {med} of {stats['n']} passing"
    return f"median {med} of {stats['n']} runs, none passing"


def representative(runs):
    """The run that represents a condition, by a fixed rule, never by hand:
    among the passing runs (all runs when none passes), the one whose wall
    time is closest to the median of their wall times (pool_stats).

    The image shows a real run, so with an even count it cannot show the
    median itself. An earlier rule took the lower median, which with two
    passing runs showed a tool's faster run while its median was much
    slower, so the image flattered that tool against the post's own table.
    Ties in distance therefore go to the slower run (with two runs, always
    the slower one), then to the smaller id, so the choice never flatters
    a tool and is deterministic. A run with no wall time is chosen only
    when no run in the pool has one (then the smallest id).

    runs: [{"id", "pass", "wall_s", ...}]. Returns (run, rule) or
    (None, None) for an empty list; `rule` says which branch applied."""
    if not runs:
        return None, None
    st = pool_stats(runs)
    passing = st["basis"] == "passing"
    pool = [r for r in runs if r.get("pass")] if passing else list(runs)
    rule = "closest to the median wall time of the passing runs" if passing else \
        "no passing run: closest to the median wall time of all runs"
    med = st["median_s"]
    if med is None:
        chosen = min(pool, key=lambda r: str(r.get("id")))
    else:
        timed = [r for r in pool if r.get("wall_s") is not None]
        chosen = min(timed, key=lambda r: (abs(r["wall_s"] - med), -r["wall_s"], str(r.get("id"))))
    return chosen, f"{rule} ({len(pool)} of {len(runs)})"


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


# ---------------------------------------------------------------------------
# Source frames
#
# NeoSCAD's agents preview through the MCP server's snapshot tool and
# export an STL only near the end, so frames from STL versions alone left
# that column on "no STL yet" for most of its run. For the .scad
# conditions the hero also renders each saved source version (with the
# same renderer and camera); these helpers decide which versions those
# are and what they may enable. CadQuery sources are Python and would have
# to run outside the sandbox, so that column keeps STL frames only.

SOURCE_RENDER_EXT = {"openscad": ".scad", "neoscad": ".scad"}

_SCAD_REF = re.compile(r"\b(?:include|use)\s*<([^>]*)>")


def strip_scad_comments(text):
    """The text with // and /* */ comments and string contents blanked, so
    a commented-out include (or one quoted in an echo) is not taken for a
    dependency: it would make a version "incomplete" that OpenSCAD
    renders fine."""
    out, i, n = [], 0, len(text)
    while i < n:
        c = text[i]
        if c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            out.append('""')  # a string's contents are never a use/include
            i = j + 1
        elif text.startswith("//", i):
            j = text.find("\n", i)
            i = n if j < 0 else j
        elif text.startswith("/*", i):
            j = text.find("*/", i + 2)
            out.append("\n" * text.count("\n", i, n if j < 0 else j))
            i = n if j < 0 else j + 2
        else:
            out.append(c)
            i += 1
    return "".join(out)


def scad_refs(text):
    """The paths a .scad file includes or uses, in order."""
    return [m.group(1).strip() for m in _SCAD_REF.finditer(strip_scad_comments(text))]


def resolve_ref(including, ref):
    """A use/include path as a path relative to the run directory, or None
    when it points outside it. OpenSCAD looks first beside the including
    file, then on its library path (.reference/openscad
    src/core/parsersettings.cc:98-104); the library path of the agent's
    run is not recorded, so only the run directory counts here and a
    library reference makes the version incomplete rather than guessed."""
    if ref.startswith("/"):
        return None
    p = posixpath.normpath(posixpath.join(posixpath.dirname(including), ref))
    return None if p == ".." or p.startswith("../") else p


def source_closure(files, parts, ext=".scad"):
    """The files one source state needs: every part file present
    (`<part><ext>` at the top of the run directory) and what they use or
    include, transitively. files: {relative path: text, or None when the
    text of that version is unknown}. Returns (closure {path: text},
    present parts, problems [str]): a reference to a file the state does
    not have, or a needed file whose text is unknown. A part whose file
    does not exist yet is simply absent, as in the STL frames."""
    present = [p for p in parts if f"{p}{ext}" in files]
    closure, problems = {}, []
    todo = [f"{p}{ext}" for p in present]
    while todo:
        path = todo.pop(0)
        if path in closure:
            continue
        text = files.get(path)
        if text is None:
            problems.append(f"{path}: text unknown (changed outside Write/Edit)" if path in files
                            else f"{path}: not in the run directory")
            closure[path] = None
            continue
        closure[path] = text
        for ref in scad_refs(text):
            target = resolve_ref(path, ref)
            if target is None:
                problems.append(f"{path}: <{ref}> is outside the run directory")
            elif target not in closure:
                todo.append(target)
    return closure, present, problems


def source_versions(sources, parts, ext, read):
    """The source states worth a frame: after each saved version (sources
    sorted by time, as in progress.json), the closure of the part files,
    kept only when it differs from the previous state's (a save to a
    check script, or a save of identical text, adds no frame). read(entry)
    returns the saved text of a sources entry, or None when the entry has
    no copy. Returns [{"n", "t", "closure", "parts", "problems"}], n from 1."""
    files, out, last = {}, [], None
    for s in sorted((s for s in sources if s.get("t") is not None), key=lambda s: s["t"]):
        if not s["path"].endswith(ext):
            continue
        files[s["path"]] = read(s)
        closure, present, problems = source_closure(files, parts, ext)
        if not present:
            continue
        sig = (tuple(sorted(closure.items(), key=lambda kv: kv[0])), tuple(present))
        if sig == last:
            continue
        last = sig
        out.append({"n": len(out) + 1, "t": s["t"], "closure": closure, "parts": present, "problems": problems})
    return out


def enabled_features(condition, mcp_config, lines):
    """The --enable features a source render of this run may use: none for
    OpenSCAD (plain language: the agent ran the OpenSCAD nightly, whose
    experiments it was not told about), and for NeoSCAD what its MCP
    server was started with (mcp.json's --enable arguments) plus `part`
    if any of the agent's MCP calls passed `parts: true`, which the server
    grants per call. Rendering with more would draw sources the agent's
    own tools rejected; with less, sources they accepted would fail."""
    if condition != "neoscad":
        return []
    feats = []
    for server in ((mcp_config or {}).get("mcpServers") or {}).values():
        a = server.get("args") or []
        for i, x in enumerate(a):
            if x == "--enable" and i + 1 < len(a):
                feats.append(a[i + 1])
            elif x.startswith("--enable="):
                feats.append(x.split("=", 1)[1])
    for _, m in _events(lines):
        if m.get("type") != "assistant":
            continue
        content = (m.get("message") or {}).get("content") if isinstance(m.get("message"), dict) else None
        for c in content or []:
            if isinstance(c, dict) and c.get("type") == "tool_use" and str(c.get("name", "")).startswith(
                    "mcp__neoscad__") and (c.get("input") or {}).get("parts") is True:
                feats.append("part")
    return sorted(set(feats))


def timeline(stl_versions, src_versions):
    """STL and source versions on one clock: [{"kind": "stl"|"src", "n",
    "t"}] sorted by time. At an equal time the STL sorts last, so
    state_at shows the export (what was graded) over the source frame."""
    items = [{"kind": "src", "n": v["n"], "t": v["t"]} for v in src_versions] + \
            [{"kind": "stl", "n": v["n"], "t": v["t"]} for v in stl_versions]
    return sorted(items, key=lambda x: (x["t"], x["kind"] == "stl", x["n"]))
