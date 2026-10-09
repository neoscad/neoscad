#!/usr/bin/env python3
"""Progress snapshots and the hero image of the CAD comparison
(docs/agent-eval.md, "Progress snapshots and the hero image").

    hero.py backfill [RECORD ...] [--src DIR] [--out DIR]
    hero.py render --task T [--out DIR] [--neoscad PATH] [--records A,B]
    hero.py compose --task T [--out DIR] [--records A,B] [--duration 12]
    hero.py all --task T [RECORD ...] [--out DIR] [--neoscad PATH]

backfill: rebuilds progress.json for runs recorded before run_cad.py wrote
  one, from what each run saved (the STL versions and their times, the
  transcript and its arrival times), into OUT/runs/<record>/<run>/.
render: every model version of the representative runs to PNG with
  NeoSCAD's command line, from one camera per task, into OUT/frames/:
  every STL version, and for the .scad conditions every saved source
  version too (CadQuery sources are not run outside the sandbox).
compose: OUT/hero-<task>.png (1200 x 630, for og:image and twitter:image)
  and OUT/hero-<task>.mp4 (an animated GIF without ffmpeg), plus
  OUT/hero-<task>.json saying which runs were picked and why.

OUT defaults to progress/agent-eval/hero (gitignored). Everything this
writes is a result: it stays in progress/ or results/ and is never
committed.

compose needs Pillow, which the CadQuery venv (setup-cadquery.sh) has; run
under another Python, it re-executes itself with that venv's interpreter
(or --pillow-python). The other steps are stdlib only.
"""

import argparse
import hashlib
import json
import os
import re
import resource
import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
sys.path.insert(0, str(HERE))
import guard  # noqa: E402
import progress as pg  # noqa: E402

SPEC = json.loads((HERE / "tasks.json").read_text())
VENV_PYTHON = ROOT / ".cache" / "agent-eval" / "cadquery-venv" / "bin" / "python"
FONTS = ROOT / "assets" / "fonts" / "Liberation-2.00.1" / "ttf"
SELF_LIMIT_MB = 2048  # this process; neoscad runs under guard.run's own 2 GB

# The camera: one rotation for every task and condition (OpenSCAD's
# default view direction), orthographic so sizes compare, and one distance
# for every frame of a composite: set by the largest bounding-box diagonal
# among the task's reference solution (refs/) and the selected runs' final
# models. The specs leave some sizes open (T1 gives no plate lengths), so
# a distance from the reference alone cut the larger brackets off; a
# diagonal is an upper bound on any projection of its box, so no final
# model clips, and a model twice another's size still draws twice as
# large. Each frame is centred on its own model's bounding box: agents
# place parts at different origins, and centring on a fixed point would
# push some off the frame.
ROTATION = (55, 0, 25)
FOV_SPAN = 0.398  # 2 tan(22.5 deg / 2): view height per unit distance in ortho
FILL = 1.0  # the scale diagonal spans the frame's shorter side
COLORSCHEME = "Tomorrow Night"
SIZES = {"still": (746, 560), "anim": (746, 760)}  # rendered at 2x, then downscaled

# The website's palette (neoscad.org theme.css, dark), and its sans and
# mono roles filled by the vendored Liberation fonts.
BG = "#0a0b14"
SURFACE = "#14162a"
BORDER = "#2e3252"
TEXT = "#e8e9f5"
MUTED = "#9a9cc0"
ACCENT = "#8f84ff"
GOOD = "#a6e3a1"
BAD = "#ff6b61"
PART_COLORS = ("#8f84ff", "#19c3d6", "#ff5a8a", "#ffd97a")
W, H = 1200, 630


def log(msg):
    print(msg, flush=True)


def check_self_memory():
    """Fails this process above SELF_LIMIT_MB of peak RSS: two runaways
    have filled the owner's swap before, so nothing here grows unwatched."""
    peak = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    mb = peak / 2**20 if sys.platform == "darwin" else peak / 1024
    if mb > SELF_LIMIT_MB:
        sys.exit(f"hero.py: {mb:.0f} MB peak RSS, over {SELF_LIMIT_MB} MB; stopping")


# ---------------------------------------------------------------------------
# Records


def latest_record(src, name):
    """The newest regrade of a record if there is one (its grades and
    version states are the current grader's), else the record itself;
    with the file name it came from."""
    regrades = sorted(src.glob(f"{name}-regrade-*.json"))
    path = regrades[-1] if regrades else src / f"{name}.json"
    return json.loads(path.read_text()), path.name


def record_names(src):
    return sorted(p.stem for p in src.glob("cad-*.json") if "-regrade-" not in p.stem)


def timed_lines(keep):
    """The transcript's lines with their time on the harness clock, and how
    exact that time is. arrivals.json (one time per line, from the
    harness's reader thread) is exact to the stream's own buffering; failing
    that, the events' own `timestamp` fields; failing that, only the order."""
    raw = (keep / "transcript.jsonl").read_text(errors="replace").splitlines()
    arr = keep / "arrivals.json"
    if arr.exists():
        ts = json.loads(arr.read_text())
        if len(ts) == len(raw):
            return list(zip(ts, raw)), "harness clock: arrival of each stream-json line (arrivals.json)"
    import datetime
    stamps = []
    for line in raw:
        try:
            s = json.loads(line).get("timestamp")
            stamps.append(datetime.datetime.fromisoformat(s.replace("Z", "+00:00")).timestamp() if s else None)
        except (json.JSONDecodeError, AttributeError, ValueError):
            stamps.append(None)
    known = [s for s in stamps if s is not None]
    if known:
        t0, out, last = known[0], [], 0.0
        for s, line in zip(stamps, raw):
            last = s - t0 if s is not None else last
            out.append((round(last, 2), line))
        return out, ("events' own timestamps, relative to the first stamped event (the start of the run "
                     "itself is not stamped; lines without one take the previous line's time)")
    return [(float(i), line) for i, line in enumerate(raw)], "order only: no times were saved"


def backfill(args):
    import run_cad  # history_states, for records written before versions kept their state
    names = args.records or record_names(args.src)
    n_runs = 0
    for name in names:
        record, used = latest_record(args.src, name)
        for r in record["runs"]:
            run_id = f"{r['task']}-{r['condition']}-{r['rep']}"
            keep = args.src / name / run_id
            if not (keep / "transcript.jsonl").exists():
                log(f"{name}/{run_id}: no transcript; skipped")
                continue
            parts = SPEC["tasks"][r["task"]]["parts"]
            detail = r.get("version_detail", [])
            states = ([v["state"] for v in detail] if all("state" in v for v in detail)
                      else run_cad.history_states(keep, detail, parts))
            if states is None:
                log(f"{name}/{run_id}: history does not match the versions; versions left out")
                detail, states = [], []
            versions = [{"n": v["n"], "t": v["t"], "state": st, "changed": v.get("changed", [])}
                        for v, st in zip(detail, states)]
            lines, stream_timing = timed_lines(keep)
            dest = args.out / "runs" / name / run_id
            if dest.exists():
                shutil.rmtree(dest)
            (dest / "sources").mkdir(parents=True)
            sources, last_text = [], {}
            for i, w in enumerate(pg.replay_source_writes(lines)):
                entry = {"t": w["t"], "path": w["path"], "copy": None, "via": w["via"]}
                if w["text"] is not None:
                    copy = f"sources/{i:03d}-{w['path'].replace('/', '__')}"
                    (dest / copy).write_text(w["text"])
                    entry.update(copy=copy, lines=sum(1 for line in w["text"].splitlines() if line.strip()))
                sources.append(entry)
                last_text[w["path"]] = w["text"]
            replay = {}
            for path, text in last_text.items():
                saved = keep / "work" / path
                if not saved.exists():
                    replay[path] = "no saved final copy"
                elif text is None:
                    replay[path] = "not replayable (an Edit to a file not written by Write)"
                else:
                    same = saved.read_text(errors="replace") == text
                    replay[path] = "matches the saved final file" if same else \
                        "differs from the saved final file (changed outside Write/Edit, e.g. by a shell command)"
            exact = stream_timing.startswith("harness clock")
            rec = pg.build_progress(
                task=r["task"], condition=r["condition"], rep=r["rep"], parts=parts, wall_s=r.get("wall_s"),
                timed_out=r.get("timed_out"), passed=r.get("pass"), versions=versions, sources=sources,
                points=pg.stream_points(lines), stl_root=str(keep / "history"), record=name,
                timing={"source": "backfill",
                        "versions": "harness clock, recorded during the run (the STL watcher's first sight of "
                                    "the finished file, polled every 0.5 s): as exact as live capture",
                        "sources": ("the arrival of each Write/Edit call's result on the " + stream_timing
                                    if exact else stream_timing) +
                                   "; files written by shell commands are not seen",
                        "stream": stream_timing},
                extra={"grade_source": used, "model": record.get("model"), "effort": record.get("effort"),
                       "source_replay": replay})
            (dest / "progress.json").write_text(json.dumps(rec, indent=1))
            n_runs += 1
            check_self_memory()
            log(f"{name}/{run_id}: {len(versions)} STL versions, {len(sources)} source writes; "
                f"times: {'harness clock' if exact else stream_timing}")
    log(f"\nbackfilled {n_runs} runs into {args.out / 'runs'}\n"
        "Timing of backfilled runs: STL versions carry the harness clock recorded during the run (the "
        "watcher's 0.5 s poll), as exact as live capture. Source versions are timed by the arrival of the "
        "Write/Edit result line on the same clock, within the stream's buffering of the real write; files "
        "the agent wrote with shell commands are missing. Turn counts are exact; cost is known only at the end.")


def load_runs(args):
    """{(record, run_id): progress} from OUT/runs (backfill) and the live
    records' run directories under --src (run_cad.py's own progress.json,
    which wins). The pass value is refreshed from the record's newest
    regrade, so a grader fix after the run is honoured."""
    found = {}
    for p in sorted((args.out / "runs").glob("*/*/progress.json")):
        found[(p.parent.parent.name, p.parent.name)] = p
    for p in sorted(args.src.glob("cad-*/*/progress.json")):
        found[(p.parent.parent.name, p.parent.name)] = p
    runs = []
    for (rec, run_id), p in sorted(found.items()):
        if args.records and rec not in args.records:
            continue
        r = json.loads(p.read_text())
        r["id"] = f"{rec}/{run_id}"
        r["dir"] = str(p.parent)
        if (args.src / f"{rec}.json").exists():
            record, used = latest_record(args.src, rec)
            for x in record["runs"]:
                if (x["task"], x["condition"], x["rep"]) == (r["task"], r["condition"], r["rep"]):
                    r["pass"], r["grade_source"] = x.get("pass"), used
            r.setdefault("model", record.get("model"))
            r.setdefault("effort", record.get("effort"))
        runs.append(r)
    return runs


def select(args, runs):
    """One run per condition for the task, by progress.representative."""
    chosen = {}
    for cond in pg.CONDITION_ORDER:
        pool = [r for r in runs if r["task"] == args.task and r["condition"] == cond]
        run, rule = pg.representative(pool)
        if run:
            chosen[cond] = {"run": run, "rule": rule,
                            "candidates": [{"id": r["id"], "pass": r.get("pass"), "wall_s": r.get("wall_s")}
                                           for r in pool]}
    if not chosen:
        sys.exit(f"no runs of {args.task} found (backfill first, or check --src/--records)")
    models = sorted({str((c["run"].get("model"), c["run"].get("effort"))) for c in chosen.values()})
    if len(models) > 1:
        log(f"warning: the selected runs differ in model/effort: {models}; pass --records to compare like "
            "with like")
    return chosen


# ---------------------------------------------------------------------------
# Rendering


_VERTEX = re.compile(rb"vertex\s+(\S+)\s+(\S+)\s+(\S+)")


def stl_bbox(path):
    """(lo, hi, triangles) of a binary or ASCII STL, streamed so a large
    mesh costs no more memory than its bytes; None for an empty mesh."""
    data = Path(path).read_bytes()
    lo, hi = [float("inf")] * 3, [float("-inf")] * 3
    n = 0
    if len(data) >= 84 and len(data) == 84 + 50 * struct.unpack_from("<I", data, 80)[0]:
        n = struct.unpack_from("<I", data, 80)[0]
        for i in range(n):
            v = struct.unpack_from("<9f", data, 84 + 50 * i + 12)
            for k in range(3):
                for x in (v[k], v[3 + k], v[6 + k]):
                    if x < lo[k]:
                        lo[k] = x
                    if x > hi[k]:
                        hi[k] = x
    else:
        for m in _VERTEX.finditer(data):
            n += 1
            for k in range(3):
                x = float(m.group(k + 1))
                if x < lo[k]:
                    lo[k] = x
                if x > hi[k]:
                    hi[k] = x
        n //= 3
    return (lo, hi, n) if n else None


def union_bbox(boxes):
    boxes = [b for b in boxes if b]
    if not boxes:
        return None
    return ([min(b[0][k] for b in boxes) for k in range(3)], [max(b[1][k] for b in boxes) for k in range(3)],
            sum(b[2] for b in boxes))


def neoscad_png(args, scad_text, png, size, center, distance):
    with tempfile.TemporaryDirectory(prefix="hero-") as d:
        f = Path(d) / "frame.scad"
        f.write_text(scad_text)
        cam = ",".join(f"{x:.4f}" for x in (*center, *ROTATION, distance))
        cmd = [str(args.neoscad), "-q", "-o", str(png), "--imgsize", f"{size[0]},{size[1]}", "--projection", "o",
               "--camera", cam, "--colorscheme", COLORSCHEME, str(f)]
        rc, _, err, _, g = guard.run(cmd, timeout=300)
    return rc == 0 and Path(png).exists(), (err or "")[-400:]


def reference_diagonal(args, task):
    """The diagonal of the task's reference solution (refs/<task>_*.scad,
    exported once with the same neoscad and cached), the smallest scale a
    composite of the task uses."""
    cache = args.out / "cache"
    cache.mkdir(parents=True, exist_ok=True)
    meta = cache / f"ref-{task}.json"
    if meta.exists():
        return json.loads(meta.read_text())["diagonal"]
    refs = sorted((HERE / "refs").glob(f"{task.lower()}_*.scad"))
    if not refs:
        return None
    stl = cache / f"ref-{task}.stl"
    rc, _, err, _, _ = guard.run([str(args.neoscad), "-q", "-o", str(stl), str(refs[0])], timeout=600)
    b = stl_bbox(stl) if rc == 0 and stl.exists() else None
    if not b:
        log(f"reference {refs[0].name} did not export ({err[-200:]}); framing on the models instead")
        return None
    diag = diagonal(b)
    meta.write_text(json.dumps({"reference": refs[0].name, "diagonal": diag, "bbox": [b[0], b[1]]}))
    return diag


def version_files(run, v):
    root = Path(run["stl_root"])
    if not root.is_absolute():
        root = Path(run["dir"]) / root
    return {p: root / c for p, c in sorted(v["state"].items())}


def diagonal(bb):
    return sum((bb[1][k] - bb[0][k]) ** 2 for k in range(3)) ** 0.5


def composite_scale(args, chosen):
    """The diagonal that fixes the camera distance of every frame: the
    largest of the reference's and the selected runs' final models'."""
    diags = {"reference": reference_diagonal(args, args.task)}
    for cond, c in chosen.items():
        if c["run"]["versions"]:
            bb = union_bbox(stl_bbox(f) for f in version_files(c["run"], c["run"]["versions"][-1]).values()
                            if f.exists())
            diags[cond] = bb and diagonal(bb)
    known = [d for d in diags.values() if d]
    return (max(known) if known else None), diags


def frame_scad(files, boxes):
    return "".join(f'color("{PART_COLORS[i % len(PART_COLORS)]}") import("{f}");\n'
                   for i, (p, f) in enumerate(files.items()) if boxes.get(p))


def frame_key(key, diag, size):
    return f"{key}:{diag}:{ROTATION}:{FILL}:{size}"


def cached(out, index, name, cam_key):
    entry = index.get(name)
    return bool(entry and entry.get("key") == cam_key and (entry["status"] != "ok" or (out / name).exists()))


def render_state(args, out, index, stem, files, key, diag):
    """Renders one model state, files {part: STL path}, at each size to
    out/<stem>-<size>.png, recording each frame's status in index under
    the cache key (the state's content and the camera)."""
    missing = [p for p, f in files.items() if not Path(f).exists()]
    boxes = {p: stl_bbox(f) for p, f in files.items() if Path(f).exists()}
    bb = union_bbox(boxes.values())
    for size_name, size in SIZES.items():
        name = f"{stem}-{size_name}.png"
        cam_key = frame_key(key, diag, size)
        if cached(out, index, name, cam_key):
            continue
        if missing and not boxes:
            index[name] = {"key": cam_key, "status": "missing", "detail": f"no STL for {missing}"}
            continue
        if bb is None:
            index[name] = {"key": cam_key, "status": "empty", "detail": "the STL has no triangles"}
            continue
        center = [(bb[0][k] + bb[1][k]) / 2 for k in range(3)]
        d = diag or diagonal(bb)
        distance = d / (FOV_SPAN * FILL * min(1.0, size[0] / size[1]))
        ok, err = neoscad_png(args, frame_scad(files, boxes), out / name, size, center, distance)
        index[name] = {"key": cam_key, "status": "ok" if ok else "failed", "detail": "" if ok else err,
                       "center": center, "distance": distance}


def renders_sources(run):
    return run["condition"] in pg.SOURCE_RENDER_EXT


def source_versions(run):
    """The run's source states worth a frame (progress.source_versions),
    for the conditions whose sources NeoSCAD can render; [] otherwise."""
    if not renders_sources(run):
        return []
    if "_src_versions" not in run:
        def read(s):
            f = Path(run["dir"]) / s["copy"] if s.get("copy") else None
            return f.read_text(errors="replace") if f and f.exists() else None
        run["_src_versions"] = pg.source_versions(run.get("sources", []), run["parts"],
                                                  pg.SOURCE_RENDER_EXT[run["condition"]], read)
    return run["_src_versions"]


def timeline(run):
    """STL and source versions on the run's clock (progress.timeline)."""
    return pg.timeline(run["versions"], source_versions(run))


def run_enable(args, run):
    """What a source render of this run may --enable
    (progress.enabled_features), from its mcp.json and transcript under
    --src."""
    rec, run_id = run["id"].split("/", 1)
    keep = args.src / rec / run_id
    mcp = keep / "mcp.json"
    config = json.loads(mcp.read_text()) if mcp.exists() else None
    tr = keep / "transcript.jsonl"
    lines = [(0.0, x) for x in tr.read_text(errors="replace").splitlines()] if tr.exists() else []
    return pg.enabled_features(run["condition"], config, lines)


def render_sources(args, run, out, index, diag, enable):
    """Renders every source version of a .scad run: each part exported to
    STL by the same neoscad (under guard.run, 2 GB / 300 s, with only the
    features the run's own tools allowed), then drawn exactly as an STL
    version is. A version that needs a file the run directory did not
    have then (or whose text is unknown) is marked incomplete and not
    rendered, rather than filled in from a later version; one that does
    not export is marked failed ("render error")."""
    flags = [x for f in enable for x in ("--enable", f)]
    ext = pg.SOURCE_RENDER_EXT[run["condition"]]
    for v in source_versions(run):
        h = hashlib.sha1(json.dumps([sorted(v["closure"].items()), v["parts"], enable]).encode())
        key = "src:" + h.hexdigest()[:16]
        stem = f"s{v['n']:02d}"
        names = {f"{stem}-{s}.png": frame_key(key, diag, size) for s, size in SIZES.items()}
        if all(cached(out, index, n, k) for n, k in names.items()):
            continue
        if v["problems"]:
            for n, k in names.items():
                index[n] = {"key": k, "status": "incomplete", "detail": "; ".join(v["problems"])}
            continue
        with tempfile.TemporaryDirectory(prefix="hero-src-") as d:
            d = Path(d)
            for path, text in v["closure"].items():
                (d / path).parent.mkdir(parents=True, exist_ok=True)
                (d / path).write_text(text)
            (d / "out").mkdir(exist_ok=True)
            errors = []
            for p in v["parts"]:
                stl = d / "out" / f"{p}.stl"
                rc, _, err, _, _ = guard.run([str(args.neoscad), "-q", "-o", str(stl), *flags, str(d / f"{p}{ext}")],
                                             cwd=d, timeout=300)
                if rc != 0 or not stl.exists():
                    errors.append(f"{p}: exit {rc}: {(err or '').strip()[-300:]}")
            if errors:
                for n, k in names.items():
                    index[n] = {"key": k, "status": "failed", "detail": "; ".join(errors)}
            else:
                render_state(args, out, index, stem, {p: d / "out" / f"{p}.stl" for p in v["parts"]}, key, diag)
        check_self_memory()


def render_run(args, run, diag):
    """Renders every version of one run at each size (STL versions as
    v<n>, and for the .scad conditions every source version as s<n>);
    writes frames.json with each frame's status (ok, empty, missing,
    failed, incomplete) for compose."""
    out = args.out / "frames" / run["id"]
    out.mkdir(parents=True, exist_ok=True)
    index_path = out / "frames.json"
    index = json.loads(index_path.read_text()) if index_path.exists() else {}
    for v in run["versions"]:
        files = version_files(run, v)
        h = hashlib.sha1()
        for p, f in files.items():
            h.update(p.encode())
            h.update(f.read_bytes() if f.exists() else b"")
        render_state(args, out, index, f"v{v['n']:02d}", files, h.hexdigest()[:16], diag)
        check_self_memory()
    if renders_sources(run):
        enable = run_enable(args, run)
        index["_source_render"] = {"enable": enable, "versions": len(source_versions(run))}
        render_sources(args, run, out, index, diag, enable)
    index_path.write_text(json.dumps(index, indent=1))
    return index


def render(args):
    chosen = select(args, load_runs(args))
    diag, diags = composite_scale(args, chosen)
    (args.out / f"scale-{args.task}.json").write_text(json.dumps({"diagonal": diag, "from": diags}, indent=1))
    for cond, c in chosen.items():
        idx = render_run(args, c["run"], diag)
        frames = {k: v for k, v in idx.items() if not k.startswith("_")}
        bad = {k: v["status"] for k, v in frames.items() if v["status"] != "ok"}
        src = idx.get("_source_render")
        log(f"{args.task} {cond}: {c['run']['id']} ({c['rule']}); {len(frames)} frames"
            + (f" ({src['versions']} source versions, --enable {src['enable'] or 'nothing'})" if src else "")
            + (f", not rendered: {bad}" if bad else ""))
    return chosen


# ---------------------------------------------------------------------------
# Composition (Pillow)


def ensure_pillow(args):
    try:
        import PIL  # noqa: F401
        return
    except ImportError:
        pass
    py = Path(args.pillow_python or VENV_PYTHON)
    if os.environ.get("HERO_REEXEC") or not py.exists():
        sys.exit("hero.py compose needs Pillow: run setup-cadquery.sh (its venv has it) or pass --pillow-python")
    env = dict(os.environ, HERO_REEXEC="1")
    os.execve(str(py), [str(py), str(Path(__file__).resolve()), *sys.argv[1:]], env)


class Canvas:
    def __init__(self):
        from PIL import Image, ImageDraw, ImageFont
        self.Image, self.ImageDraw, self.ImageFont = Image, ImageDraw, ImageFont
        self.fonts = {}
        self.images = {}

    def font(self, kind, size):
        key = (kind, size)
        if key not in self.fonts:
            name = {"sans": "LiberationSans-Regular.ttf", "bold": "LiberationSans-Bold.ttf",
                    "mono": "LiberationMono-Regular.ttf", "monob": "LiberationMono-Bold.ttf"}[kind]
            self.fonts[key] = self.ImageFont.truetype(str(FONTS / name), size)
        return self.fonts[key]

    def frame(self, path, box):
        """A render fitted into box (w, h): its flat background replaced
        by the panel colour before scaling, so edges blend into the panel
        rather than into the colour scheme's grey."""
        key = (str(path), box)
        if key not in self.images:
            from PIL import ImageChops
            im = self.Image.open(path).convert("RGB")
            bg = im.getpixel((0, 0))
            mask = ImageChops.difference(im, self.Image.new("RGB", im.size, bg)).convert("L")
            mask = mask.point(lambda v: 255 if v == 0 else 0)
            im.paste(self.Image.new("RGB", im.size, SURFACE), (0, 0), mask)
            scale = min(box[0] / im.width, box[1] / im.height)
            im = im.resize((max(1, round(im.width * scale)), max(1, round(im.height * scale))),
                           self.Image.LANCZOS)
            cell = self.Image.new("RGB", box, SURFACE)
            cell.paste(im, ((box[0] - im.width) // 2, (box[1] - im.height) // 2))
            self.images[key] = cell
        return self.images[key]


def frame_for(args, run, item, size_name):
    """(png path or None, placeholder text) for one timeline item of a run
    ({"kind": "stl"|"src", "n"}, from timeline())."""
    if item is None:
        return None, "no model yet"
    out = args.out / "frames" / run["id"]
    cache = args.__dict__.setdefault("_frame_index", {})
    if run["id"] not in cache:
        f = out / "frames.json"
        cache[run["id"]] = json.loads(f.read_text()) if f.exists() else {}
    name = f"{'s' if item['kind'] == 'src' else 'v'}{item['n']:02d}-{size_name}.png"
    e = cache[run["id"]].get(name)
    if not e:
        return None, "not rendered"
    if e["status"] == "ok":
        return out / name, None
    src = item["kind"] == "src"
    return None, {"empty": "empty mesh", "missing": "STL missing", "failed": "render error" if src else "render failed",
                  "incomplete": "incomplete\n(a used file is missing)"}[e["status"]]


def badge_for(item, done=False, final=False):
    """The cell's corner label: STL versions by number, source-rendered
    frames marked "src" so the image never passes a render of the source
    off as an exported model."""
    if item is None:
        return "done" if done else None
    words = (["final"] if final else []) + (["src"] if item["kind"] == "src" else [f"v{item['n']}"])
    return " ".join(words + (["done"] if done and not final else []))


def waiting_text(run, t):
    """The placeholder before a run's first frame: says so, with how many
    source writes the agent had made by then, so an agent that previews
    through its own tool and exports late does not look idle."""
    edits = sum(1 for s in run.get("sources", []) if s.get("t") is not None and s["t"] <= t)
    head = "no model yet" if renders_sources(run) else "no STL yet"
    return head + (f"\n{edits} source edit{'s' if edits != 1 else ''}" if edits else "")


def final_item(run):
    """The large cell's frame: the last STL version, which is what the
    grader judged; a source edited after it is not shown there, since the
    pass/fail line beside it is about the export. Only a run that never
    exported shows its newest source frame (badged src)."""
    if run["versions"]:
        return {"kind": "stl", "n": run["versions"][-1]["n"], "t": run["versions"][-1]["t"]}
    srcs = [x for x in timeline(run) if x["kind"] == "src"]
    return srcs[-1] if srcs else None


def text_center(draw, box, text, font, fill):
    """Centred lines (split on newlines), so small cells stay legible."""
    x0, y0, x1, y1 = box
    lines = text.split("\n")
    step = font.size * 1.3
    y = (y0 + y1) / 2 - step * (len(lines) - 1) / 2
    for line in lines:
        tw = draw.textlength(line, font=font)
        draw.text(((x0 + x1 - tw) / 2, y), line, font=font, fill=fill, anchor="lm")
        y += step


def paste_cell(cv, img, draw, box, path, placeholder, badge=None):
    x0, y0, x1, y1 = box
    if path is not None:
        img.paste(cv.frame(path, (x1 - x0, y1 - y0)), (x0, y0))
    else:
        draw.rectangle(box, fill=SURFACE)
        text_center(draw, box, placeholder, cv.font("sans", 18 if y1 - y0 > 150 else 14), MUTED)
    draw.rectangle(box, outline=BORDER, width=1)
    if badge:
        f = cv.font("monob", 14)
        tw = draw.textlength(badge, font=f)
        draw.rectangle((x0 + 6, y0 + 6, x0 + 16 + tw, y0 + 26), fill=BG)
        draw.text((x0 + 11, y0 + 16), badge, font=f, fill=MUTED, anchor="lm")


def column_boxes(n):
    margin, gap = 24, 18
    cw = (W - 2 * margin - gap * (n - 1)) // n
    return [(margin + i * (cw + gap), cw) for i in range(n)]


def draw_header(cv, draw, x, cw, cond, run, shown_t, done):
    draw.text((x, 84), pg.LABEL.get(cond, cond), font=cv.font("bold", 30), fill=TEXT, anchor="ls")
    t = pg.fmt_clock(shown_t)
    f = cv.font("monob", 30)
    draw.text((x + cw - draw.textlength(t, font=f), 84), t, font=f, fill=ACCENT if done else TEXT, anchor="ls")
    if done:
        if run.get("timed_out"):
            status, col = "stopped at the time limit", BAD
        elif run.get("pass"):
            status, col = "final model passes the grader", GOOD
        else:
            status, col = "final model fails the grader", BAD
    else:
        status, col = "working", MUTED
    nv = len(run["versions"])
    draw.text((x, 108), status, font=cv.font("sans", 16), fill=col, anchor="ls")
    if done:
        s = f"{nv} STL version{'s' if nv != 1 else ''}"
        fs = cv.font("sans", 16)
        draw.text((x + cw - draw.textlength(s, font=fs), 108), s, font=fs, fill=MUTED, anchor="ls")


def draw_bar(cv, draw, x, cw, y, run, t_max, upto):
    """The shared time axis: the bar's full width is t_max (the slowest
    selected run), the fill this run's elapsed time up to `upto`; bright
    ticks mark STL versions, faint ones the source writes."""
    draw.rounded_rectangle((x, y, x + cw, y + 12), radius=6, fill=BORDER)
    end = min(upto, run.get("wall_s") or 0)
    xe = pg.axis_x(end, t_max, x, cw)
    if xe > x + 2:
        draw.rounded_rectangle((x, y, xe, y + 12), radius=6, fill=ACCENT)
    for s in run.get("sources", []):
        if s.get("t") is not None and s["t"] <= upto:
            xs = pg.axis_x(s["t"], t_max, x, cw)
            draw.line((xs, y + 15, xs, y + 20), fill=MUTED, width=1)
    for v in run["versions"]:
        if v["t"] <= upto:
            xv = pg.axis_x(v["t"], t_max, x, cw)
            draw.line((xv, y - 3, xv, y + 15), fill=TEXT, width=2)


def draw_title(cv, draw, task, right):
    title = f"{task}: {SPEC['tasks'][task]['title']}"
    draw.text((24, 44), title, font=cv.font("bold", 24), fill=TEXT, anchor="ls")
    f = cv.font("sans", 18)
    draw.text((W - 24 - draw.textlength(right, font=f), 44), right, font=f, fill=MUTED, anchor="ls")


def draw_axis_labels(cv, draw, x, cw, y, t_max):
    f = cv.font("mono", 13)
    draw.text((x, y), "0:00", font=f, fill=MUTED, anchor="ls")
    s = pg.fmt_clock(t_max)
    draw.text((x + cw - draw.textlength(s, font=f), y), s, font=f, fill=MUTED, anchor="ls")


def compose_still(args, cv, chosen, t_max):
    img = cv.Image.new("RGB", (W, H), BG)
    draw = cv.ImageDraw.Draw(img)
    conds = [c for c in pg.CONDITION_ORDER if c in chosen]
    draw_title(cv, draw, args.task, "same prompt and model, one representative run each")
    strip_times = pg.sample_times(t_max, 4)[:-1]
    used = {}
    for (x, cw), cond in zip(column_boxes(len(conds)), conds):
        run = chosen[cond]["run"]
        draw_header(cv, draw, x, cw, cond, run, run.get("wall_s"), True)
        line = timeline(run)
        final = final_item(run)
        path, ph = frame_for(args, run, final, "still")
        paste_cell(cv, img, draw, (x, 122, x + cw, 400), path, ph if final else "no STL exported",
                   badge=badge_for(final, final=True) if final else None)
        used[cond] = {"final": final and {"kind": final["kind"], "n": final["n"]}, "strip": []}
        gap = 8
        sw = (cw - 2 * gap) // 3
        for i, ts in enumerate(strip_times):
            v = pg.state_at(line, ts)
            sx = x + i * (sw + gap)
            path, ph = frame_for(args, run, v, "still")
            done = run.get("wall_s") is not None and ts >= run["wall_s"]
            if v is None:
                ph = waiting_text(run, ts)
            paste_cell(cv, img, draw, (sx, 410, sx + sw, 494), path, ph if path is None else None,
                       badge=badge_for(v, done=done) if (done or (v and v["kind"] == "src")) else None)
            draw.text((sx + sw / 2, 512), f"at {pg.fmt_clock(ts)}", font=cv.font("mono", 14), fill=MUTED,
                      anchor="ms")
            used[cond]["strip"].append({"t": round(ts, 1), "frame": v and {"kind": v["kind"], "n": v["n"]}})
        draw_bar(cv, draw, x, cw, 536, run, t_max, t_max)
        draw_axis_labels(cv, draw, x, cw, 572, t_max)
    draw.text((24, 610), "Shared clock times, one axis (0 to the slowest run). Ticks: STL versions (tall), "
              "source edits (short). src: rendered from the saved source.", font=cv.font("sans", 14),
              fill=MUTED, anchor="ls")
    out = args.out / f"hero-{args.task}.png"
    img.save(out, optimize=True)
    return out, {"strip_times": [round(t, 1) for t in strip_times], "frames": used}


def compose_animation(args, cv, chosen, t_max):
    conds = [c for c in pg.CONDITION_ORDER if c in chosen]
    clock = pg.animation_clock(t_max, args.duration, args.fps, args.hold)
    cols = list(zip(column_boxes(len(conds)), conds))
    ff = shutil.which("ffmpeg")
    out_mp4 = args.out / f"hero-{args.task}.mp4"
    out_gif = args.out / f"hero-{args.task}.gif"
    proc, frames = None, []
    if ff:
        proc = subprocess.Popen([ff, "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "rgb24",
                                 "-s", f"{W}x{H}", "-r", str(args.fps), "-i", "-", "-c:v", "libx264",
                                 "-pix_fmt", "yuv420p", "-crf", "20", "-preset", "slow",
                                 "-movflags", "+faststart", str(out_mp4)], stdin=subprocess.PIPE)
    speed = pg.speedup(t_max, args.duration)
    for i, t in enumerate(clock):
        img = cv.Image.new("RGB", (W, H), BG)
        draw = cv.ImageDraw.Draw(img)
        draw_title(cv, draw, args.task, f"t = {pg.fmt_clock(t)}   ({speed:.0f}x)")
        for (x, cw), cond in cols:
            run = chosen[cond]["run"]
            wall = run.get("wall_s") or 0
            done = t >= wall
            draw_header(cv, draw, x, cw, cond, run, min(t, wall), done)
            v = pg.state_at(timeline(run), min(t, wall))
            path, ph = frame_for(args, run, v, "anim")
            if v is None:
                ph = "no model" if done else waiting_text(run, t)
            paste_cell(cv, img, draw, (x, 122, x + cw, 502), path, ph,
                       badge=badge_for(v, final=done) if v else None)
            draw_bar(cv, draw, x, cw, 530, run, t_max, t)
            draw_axis_labels(cv, draw, x, cw, 566, t_max)
        draw.text((24, 610), "Each column runs on one shared clock and stops when its agent finished. "
                  "src: rendered from the saved source, not an exported STL.",
                  font=cv.font("sans", 14), fill=MUTED, anchor="ls")
        if proc:
            proc.stdin.write(img.tobytes())
        else:
            frames.append(img.convert("P", palette=cv.Image.ADAPTIVE, colors=128))
        if i % 20 == 0:
            check_self_memory()
    if proc:
        proc.stdin.close()
        if proc.wait() != 0:
            sys.exit("ffmpeg failed")
        return out_mp4
    frames[0].save(out_gif, save_all=True, append_images=frames[1:], duration=int(1000 / args.fps), loop=0)
    return out_gif


def compose(args, chosen=None):
    ensure_pillow(args)
    chosen = chosen or select(args, load_runs(args))
    for c in chosen.values():
        if not (args.out / "frames" / c["run"]["id"] / "frames.json").exists():
            sys.exit(f"{c['run']['id']} is not rendered; run `hero.py render --task {args.task}` first")
    t_max = pg.shared_t_max([c["run"] for c in chosen.values()])
    cv = Canvas()
    still, layout = compose_still(args, cv, chosen, t_max)
    anim = compose_animation(args, cv, chosen, t_max)
    meta = {
        "task": args.task, "t_max": t_max, "still": still.name, "animation": anim.name,
        "selection_rule": pg.RULE,
        "selected": {cond: {"id": c["run"]["id"], "rule": c["rule"], "pass": c["run"].get("pass"),
                            "wall_s": c["run"].get("wall_s"), "timing": c["run"].get("timing"),
                            "grade_source": c["run"].get("grade_source"), "candidates": c["candidates"]}
                     for cond, c in chosen.items()},
        "camera": {"rotation": ROTATION, "projection": "ortho", "fill": FILL, "colorscheme": COLORSCHEME,
                   "scale": json.loads((args.out / f"scale-{args.task}.json").read_text()),
                   "renderer": subprocess.run([str(args.neoscad), "--version"], capture_output=True,
                                              text=True).stdout.strip() if Path(args.neoscad).exists() else None},
        "animation_s": {"sweep": args.duration, "hold": args.hold, "fps": args.fps},
        "source_frames": {
            "rule": "for the .scad conditions, a frame at time t shows the newest of the STL versions and the "
                    "rendered source versions at or before t (an STL wins a tie); source frames are badged src. "
                    "CadQuery columns show STL versions only.",
            **{cond: json.loads((args.out / "frames" / c["run"]["id"] / "frames.json").read_text())
               .get("_source_render") for cond, c in chosen.items() if renders_sources(c["run"])}},
        **layout,
    }
    (args.out / f"hero-{args.task}.json").write_text(json.dumps(meta, indent=1))
    log(f"wrote {still}\nwrote {anim}\nwrote {args.out / f'hero-{args.task}.json'}")


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("step", choices=["backfill", "render", "compose", "all"])
    ap.add_argument("records", nargs="*", help="record names (cad-<ts>); default: every record in --src")
    ap.add_argument("--src", type=Path, default=ROOT / "progress" / "agent-eval",
                    help="where run_cad.py wrote the records and their run directories (read only)")
    ap.add_argument("--out", type=Path, default=ROOT / "progress" / "agent-eval" / "hero")
    ap.add_argument("--task", help="the task to render and compose (e.g. T2)")
    ap.add_argument("--neoscad", default=str(ROOT / "target" / "release" / "neoscad"))
    ap.add_argument("--duration", type=float, default=12.0, help="animation sweep, seconds")
    ap.add_argument("--hold", type=float, default=3.0, help="seconds held on the finished state")
    ap.add_argument("--fps", type=int, default=15)
    ap.add_argument("--pillow-python", help="a Python with Pillow for compose (default: the CadQuery venv's)")
    args = ap.parse_args()
    args.src, args.out = args.src.resolve(), args.out.resolve()
    args.out.mkdir(parents=True, exist_ok=True)
    if args.records and "," in ",".join(args.records):
        args.records = [x for r in args.records for x in r.split(",") if x]
    if args.task and args.task not in SPEC["tasks"]:
        sys.exit(f"unknown task {args.task}")
    if args.step in ("render", "compose", "all") and not args.task:
        sys.exit("--task is required")
    if args.step in ("render", "all") and not Path(args.neoscad).exists():
        sys.exit(f"no neoscad at {args.neoscad} (pass --neoscad)")
    if args.step in ("compose", "all"):
        ensure_pillow(args)  # before any work, since it may re-execute this script
    if args.step in ("backfill", "all"):
        backfill(args)
    chosen = None
    if args.step in ("render", "all"):
        chosen = render(args)
    if args.step in ("compose", "all"):
        compose(args, chosen)


if __name__ == "__main__":
    main()
