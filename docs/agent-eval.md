# Agent-loop eval

Does NeoSCAD's agent surface help an AI agent model things? The eval
gives an agent a modeling task twice, with the same model, prompt and
turn budget:

- **A:** the NeoSCAD MCP server (`docs/mcp.md`) plus Read, Write and
  Edit;
- **B:** Bash and Read, with the nightly OpenSCAD command line
  (`/Applications/OpenSCAD.app/Contents/MacOS/OpenSCAD`, which exports
  meshes and PNGs that Read can show the model).

Each run is headless `claude -p` in its own temporary directory, run one
after another. Only the tools and one appended system-prompt line (what
the tools are, `conditions` in the task file) differ between the
conditions. Runs use `--strict-mcp-config` and `--setting-sources
project` (the temporary directory has none), so the user's MCP servers,
hooks and permissions stay out of both.

## Tasks and grading

`conformance/agent-tasks.json` holds the tasks: a prompt each, stating
the modules the model must define and the contract they are graded on,
plus a common closing paragraph. Each task has a **hidden grader**, a
`neoscad test` file (`conformance/agent-tasks/<id>_test.scad`,
`docs/model-tests.md`) that `use`s the agent's `model.scad`. It is
copied next to the model only after the run ends; the agent never sees
it. Graders test the contract on derived solids: the intersection of the
agent's parts with probe solids (a pipe, a slab behind the phone's back,
a ring where a wall must be), unioned with a 1 mm³ marker cube so that
"nothing here" measures exactly 1 mm³ instead of failing as an empty
model. Tooth counts are tested by symmetry (a gear with N teeth is
unchanged by a turn of 360/N degrees).

| Task | The agent must | The grader checks |
|---|---|---|
| `box_lid` | A 60 x 40 x 30 box, 2 mm walls, and a lid that fits | box size and volume; lid footprint; no overlap; lid on top; a lip inside the opening |
| `phone_stand` | Hold a given phone solid at 70° | one printable solid; no overlap; supports within 1 mm behind the back and under the bottom; a front lip; nothing below the table; base reach |
| `pipe_adapter` | Reduce a 32 mm pipe to 20 mm | pipes (at their true diameters) fit both sockets; the bottom socket stops the pipe; 16 mm passage open; walls of 2.5 mm; height |
| `gears` | Mesh a 24- and a 12-tooth module-2 gear 36 mm apart | size; tooth counts; volume; no overlap; teeth reach into the other's pitch circle; bores |

`scripts/agent-eval/run.py --check-graders` runs each grader on its
reference solution (`<id>.ref.scad`), which must pass. While writing
them, each grader was also run on wrong solutions, which failed: an
overlapping lid, a lid with no lip, a stand for 60°, a thin wall, a
bore drawn at the pipe's own size, a 23-tooth gear, gears 38 mm apart,
and gears in phase (teeth on teeth). A minimum-wall adapter at default
`$fn` passes (the probes leave room for facets).

## Running

    cargo build --release
    scripts/agent-eval/run.py --check-graders
    scripts/agent-eval/run.py [--model sonnet] [--tasks box_lid,gears] \
        [--conditions A,B] [--max-turns 40] [--dry-run]

Measures, per run: pass or fail (and tests passed) from the grader, tool
calls (by tool), turns, input tokens (uncached + cache reads + cache
writes, from the run's `modelUsage`) and output tokens, cost as Claude
Code reports it, and wall time. Runs use `--output-format stream-json`
(whose last line is the object `--output-format json` prints), because
the transcript is needed to count tool calls and read failure modes.

Results go to `progress/agent-eval/<UTC ts>-<sha>[-dirty].json`
(gitignored), transcripts and final models to the directory of the same
name, and one summary line per eval to `progress/agent-eval/index.jsonl`.

Graders `include` the model rather than `use` it. `use` drops a file's
top-level assignments, including `$fn` (the nightly draws a `use`d
`$fn = 64` circle with 16 segments, and 64 with `include`), so a grader
that `use`s the model would judge it at default resolution, not as its
author renders it: a small bore becomes a heptagon and teeth can part
from a coarse root disc. `run.py --regrade` grades saved models again
after a grader fix, and the record keeps the original grade in
`grade_at_run`.

Run records stay local: `progress/` is gitignored, and results are not
published in this repository.

## CAD comparison (ModelRift replication)

`scripts/agent-eval/cad/` replicates ModelRift's "CadQuery vs OpenSCAD"
agent benchmark (<https://modelrift.com/blog/cadquery-vs-openscad/>,
retrieved 2026-09-28) with NeoSCAD as a third condition. It is separate from `run.py`
above: other tasks, an independent mesh grader instead of `neoscad test`,
and ModelRift's metrics.

    scripts/agent-eval/cad/setup-cadquery.sh      # CadQuery 2.8.0 venv (.cache/, gitignored)
    scripts/agent-eval/cad/test_grade.py          # grader tests (no model calls)
    scripts/agent-eval/cad/run_cad.py --neoscad PATH \
        [--model 'claude-opus-5[1m]'] [--effort LEVEL] [--tasks T1,T2,T3] \
        [--conditions openscad,cadquery,neoscad] [--n 1]
    scripts/agent-eval/cad/run_cad.py --tasks T0 --model haiku ...  # plumbing check
    scripts/agent-eval/cad/summarize.py progress/agent-eval/cad-<ts>.json
    scripts/agent-eval/cad/run_cad.py --regrade cad-<ts>           # after a grader fix

**Conditions.** (a) `openscad`: the nightly via Bash with
`--backend=manifold`; (b) `cadquery`: CadQuery 2.8.0 on Python 3.14 (as
ModelRift) via Bash, Python scripts that export STL; (c) `neoscad`:
`neoscad mcp` through `--mcp-config`, with the same Bash as the others.
All three get Bash, Read, Write and Edit. The prompt (`tasks.json`) is
identical: ModelRift's task wording, the same 3D-printing rules, the
12-version cap, "never fake success", and "report every failure with the
verbatim error text". Only a short tool briefing differs, saying how
to export a mesh and how to get a PNG to Read: `-o x.png` (a),
`view_stl.py` (b), `snapshot` (c).

**Differences from ModelRift.** Their OpenSCAD
agent had their proprietary "openscad-skill" and their CadQuery agent a
port of it; ours have neither, only the briefing. Their 3D-printing
rules were not published beyond "wall thickness, clearances and
overhangs" and "FDM with a 0.4 nozzle", so ours are our own (in
`tasks.json`). T3's last sentence, "Stacked rings are not a thread and
are not allowed", is ours: ModelRift's write-up says only that the spec banned
them. Output names (`out/<part>.stl`, one source per part) are ours, so
the grader can find the files.

**Held-out tasks.** NeoSCAD's MCP server was improved using findings
from T1-T3 (its recipes include a countersink, a thread and a snap
hook), so T4-T6 are tasks it was never tuned on, written and fixed
before any tool ran them: a pin-hinged box (body and lid, five
interleaved knuckles), a knob for a D-shaft (flutes, a blind D bore, a
radial set-screw hole) and a spur gear pair on a plate with axles. They
are ours, in ModelRift's style, and avoid T1-T3's features. Run them with
`--tasks T4,T5,T6` and report them apart from T1-T3.

**Protocol.** One run at a time, each in a fresh temporary directory:
`claude -p --restricted` (the file tools confined to that directory; no
user or project settings, hooks, skills or MCP servers),
`--strict-mcp-config`, `--no-session-persistence`, `--permission-prompts
none`, stream-json output. The default model is `claude-opus-5[1m]`
(Opus 5 with 1M context, as ModelRift; this id was accepted by Claude
Code 2.1.284). `--max-turns 200` and `--max-budget-usd 20` are
backstops; the version cap is the budget. `--effort` passes Claude
Code's reasoning effort; without it the model's default applies, since
`--restricted` ignores the user's `effortLevel` setting (Opus 5's
default was high on Claude Code 2.1.286, where a Bash `echo
$CLAUDE_EFFORT` in a run printed `high`). Each record keeps the effort it
ran at.

**Safety.** Claude Code's Bash sandbox is on (`failIfUnavailable`, no
unsandboxed escape): writes only in the run directory, no network (a
`curl` fails with "host is not on the allow list"), the NeoSCAD
repository unreadable, and each condition blind to the others' tools.
Two commands run outside it, as `excludedCommands`: the OpenSCAD
binary, which aborts in the sandbox ("Incompatible processor. This Qt
build requires the following features: neon": Qt reads the
`hw.optional.neon` sysctl, which the sandbox denies), and
`view_stl.py`, because CadQuery's own viewer gets no OpenGL context in
the sandbox and this script only reads and writes under the working
directory. Claude Code exempts a compound command only when every part
matches, so the briefings say to run those as commands of their own.
The MCP server runs outside the sandbox with its own roots
(`docs/mcp.md`). `guard.py` polls every process under the run twice a
second and kills the run when a geometry process passes 2 GB (RSS, and
`footprint` above 512 MB) or Claude Code passes 4 GB; a run stops after
45 minutes. The grader, recompute and tests run under the same guard,
and the grader refuses meshes over 900k triangles (about 2 KB each).

**Metrics per run** (the record's fields):

- `versions`: tool calls that wrote new bytes to `out/<part>.stl`. A
  watcher copies every STL written in the run directory with its time;
  each copy is matched to the tool call that was running.
- `tool_errors`: CAD-tool calls whose result was an error: OpenSCAD
  output with `ERROR:`; a Python traceback or `...Error:` from the
  CadQuery interpreter (this includes any failing script run with it);
  an MCP result with `isError`, `failed` or an error diagnostic.
- `silent_wrong_versions`: versions that the grader finds wrong (a part
  not clean, or, with every part present, a failed spec check) while
  the call that made them raised no tool error.
- `recompute_s`: the final sources copied to a fresh directory and
  exported from the command line, median of 3 (NeoSCAD: the CLI, a
  cold process, not the MCP server's warm cache; CadQuery: includes
  Python start-up). `identical_to_agent_stl` says whether the export
  reproduced the agent's file byte for byte.
- `wall_s`, `tokens` (input, cache read, cache write, output from the
  result's `modelUsage`; `tokens_total` and `tokens_excl_cache_reads`),
  `cost_usd`, `turns` (summed over the stream's result events: a
  background command finishing after the report wakes the agent for a
  follow-up with its own result, whose `num_turns` counts only itself
  while tokens and cost are session totals; the report is the last result
  that answered the prompt, follow-ups go to `followup_messages`), tool calls by tool, `lines_of_code` (non-blank
  lines of the part sources and the local files they include or
  import).

**Grader** (`grade.py`, `stlmesh.py`: Python stdlib only; it imports
none of the tools). Per part, ModelRift's facts: triangles, bbox,
volume, watertight, boundary and non-manifold edges, flipped faces
(orientation propagated across manifold edges, then the sign of the
volume), components (faces joined by edges), genus, min z. **Clean** is
theirs: watertight, one component, no boundary or non-manifold edges,
min z within 0.01 of 0. Vertices are welded by exact float32 equality,
so a crack is not hidden; a face whose corners weld to fewer than three
points (a sliver nanometres wide in the exporter's doubles) counts as
degenerate but is not a component. Dimensional checks use axis-aligned rays
(inside intervals by winding number) and planar sections (loops chained
by mesh edge), which measure like a caliper. Gated checks, with their
tolerances:

| Task | Gated checks |
|---|---|
| T1 | plates 4 thick (±0.1, median over rays from the outer face); 2 countersunk + 2 plain holes; head 9 (±0.4, cone extrapolated from sections 0.3 and 0.8 mm deep) at 90° (±8); countersinks on the inner face; every hole's line of sight clear; R4 inner fillet (±0.8, from the diagonal gap in profile sections); R3 on at least 2 of the plates' free corners and the L's outer corner (±0.6); two gussets (solid spans along the edge 3 mm beyond the inner corner) |
| T2 | walls 2 (±0.1) and cavity 50.8 × 26.8 (±0.1), medians of rays at 6 heights × 9 positions; four M2 post holes (1.5-3.6 dia, circumscribed, so a faceted 1.5 hole is 1.5) 1 mm above the floor; a 9.5 × 3.5 opening (±0.25) in a wall, the floor or the lid plate; five vent slots (the largest group of identical openings at least 1.5 times as long as wide: the spec gives no proportion, and 2.5 × 4.5 vents are slots); lid lip (or skirt) clearance 0.2 per side (±0.05) over a band at least 0.6 mm tall (levels 0.2 mm apart, each the median over positions), so a lip relieved for snap clearance is judged by its locating band |
| T3 | Segments found by radial profile, in any order and orientation: hex (mean radius ≥ 13.5), thread-like (some direction reaches radius 9), barb (the rest), each the longest run of its levels. 8 channel (±0.2) open at every level and along the axis; hex 30 across flats (±0.2) over the levels with full hexagon corners (corners/flats 1.13-1.18, at least two levels 0.25 mm apart; chamfered corners elsewhere are allowed); thread 12 long (±1, the longest cluster of levels whose groove is open: some direction within 30% of the depth of the root, the crest being the 80th percentile of the widest direction, so a 45° cone under the flange is not thread); major diameter 24 (-0.6/+0.1, over the threaded levels); pitch 2 (±0.1, autocorrelation of the radius along one direction); helical, depth at least 0.8: the profile at 90°, 180° and 270° shifted by a quarter, half and three quarters of a pitch (stacked rings shift by 0); right-hand (with no thread to sample, all four thread gates fail, so every part has 11 gates); barb 25 long (±1, from the hex face or the thread's end, whichever is nearer, to the next feature or the part's end; or from where a flare into that feature starts, the levels narrowing away from it at a slope of 0.5 or more over at least 0.5 mm, since the spec does not say whether a root fillet or a skirt under the flange is barb or flange); three barbs (plateaus of the mean radius with a prominence of 0.3, not counting a flare's wide end) of 12.5-16 dia; layout: thread and barb at opposite ends with the hex between (our reading of "hose-barb adapter": the spec does not state the order, but a thread with no free end cannot screw into a port without burying the barb; this is our interpretation). 11 gates |
| T4 | body 70 × 45 × 30 outside (±0.2; rays below half height, so the hinge is not in them, each face the innermost quartile of the rays, so ribs carrying the knuckles down the wall do not count; rim the median top of the walls); walls 2 and floor 2 (±0.1); lid plate 70 × 45 × 3 (±0.2, ±0.1; each the most common value over many rays, so knuckles and notches do not count); the pin hole's axis found as the small hole (narrowest width 1.2-3.5) seen in most sections along the 70 side; 3 body and 2 lid knuckles 12 long (±0.2: spans along the axis at mid-knuckle, in the direction that sees most separate spans); knuckle outer diameter 7 (±0.3, median distance from the axis to the outline within 5 of it); pin hole 2 (±0.15, narrowest width, so a teardrop counts) clear through every knuckle; knuckles alternate B L B L B with 0.4 (±0.1) between neighbours, the lid's spans placed by its plate's ends either way round; closed, the lid's axis on the body's (±0.3): the same distance out from the hinge-side wall and up from the rim, the lid's rim face being either face of its plate. 8 gates |
| T5 | 18 tall (±0.1; the shortest extent is the axis); 30 diameter (±0.2) over the lands, 20 flutes (dips of the radius profile) 1 deep (±0.25), medians of five sections; bore 12 deep (±0.2) from the face it opens on and not through; D bore 6.2 across and 4.7 flat to far side (±0.15, calipers of sections at 1.5, 2.5, 8 and 10); set-screw hole 2.5 (±0.2, narrowest width) on the flat's normal, its axis 5 (±0.3) from the bottom and within 0.3 of the flat's middle, clear from the bore to the outside. 7 gates |
| T6 | plate 80 × 55 × 4 (±0.2, ±0.1); two axles 5 (±0.15, circumscribed) standing 10 (±0.2) above the plate; axle centres 34 apart (±0.1); per gear: tooth count (30, 15: dips of the radius profile at mid-thickness) and outside diameter m(z + 2) = 48, 25.5 (±0.3); gears 6 thick (±0.1); bores 5.4 (±0.15, circumscribed); meshing: the mid-thickness outlines 34 apart, from some phase of the small gear (0.25° steps), turn through one tooth pitch of the large gear in 12 steps at 30:15 without crossing, and the tips engage at least one module (1.5) deep. 10 gates |

**What the grader does not judge**, and says so in the check (`ok:
null`, not gated): whether T1 prints without supports (bridges and small
horizontal holes are fine, so overhang area is only reported), whether
T2's snaps work (it reports the spread of the lip width, which shows
bumps), T2 hole positions against a PCB (the spec gives none), and
self-intersection (not computed), whether T4's lid swings clear and its knuckles print without support (both need the assembly), and T6's 20° pressure angle (a 25° gear passes; only meshing at 34 is judged). Known blind spots: a USB cutout made
as a notch open to the rim is not found (only closed openings are); a
T1 gusset with legs under 6 mm is missed; T3 assumes the part's longest
axis is its axis, and a barb under radius 9 and a thread over it (a
barb for 12 ID hose is under 8 by its own gate); "R3 on the outer vertical corners" is ambiguous, so
the check accepts any two of the candidates. `test_grade.py` checks
the grader on synthetic meshes (cube, cube with a hole, two shells, a
flipped face, inside-out, open, a non-manifold edge, off the bed,
ASCII vs binary), on references in `refs/` (which pass) and on wrong
variants of them (plates 5 thick, countersinks outside, no fillet, an
11 head; lip clearance 0.4, walls 2.5, four vents, cavity clearance
0.2; stacked rings, a left-hand thread, pitch 1.5, 32 across flats),
each failing exactly its check; on variants of valid shapes that an
earlier version of the grader wrongly failed (a lip relieved
0.8 with a 1 mm band at 0.2; a flange-down hex, thread, barb stack,
also turned over; hex corners chamfered over 5 of 8 mm; a plain stem
and tip chamfer after the barbs), which pass, and on wrong versions
of them (band at 0.4; thread 10, barb 20, pitch 1.5, 32 across flats
and rings in the flange-down stack; a round flange; two barbs); T4-T6 references (in `refs/`, also turned 90° and 180°, with knuckle ribs
down the back wall, printed bore down, with thinner teeth or a 25° pressure
angle), which pass, and their wrong variants (a 47 deep box, walls 2.5,
knuckles 8, a 3 pin hole, 0.8 between knuckles, the lid's knuckles 1 off
the body's axis; 18 flutes, flutes 2 deep, flat 4.2, a 14 deep and a
through bore, the set screw at 7; axles 36 apart or 6 across, 5 bores, 7
thick gears, a 5 plate, teeth 0.25° fatter), each failing its own check,
and a 16-tooth small gear, which fails its count, its diameter and the mesh; and on each condition's toolchain
making the same small plate.

**Output.** `progress/agent-eval/cad-<ts>.json` (a new file per eval,
rewritten after each run so a crash keeps what finished; `--regrade`
leaves it and its grade files alone, grades every version and the final
parts again, re-derives turns, tokens, cost and the final message from
the saved transcript (old values in `account_at_run`), and writes `cad-<ts>-regrade-<ts2>.json` beside it, with
each run's old figures in `grade_at_run` and its new grades in
`cad-<ts>/<run>/regrade-<ts2>/`), `cad-<ts>/<task>-<cond>-<n>/`
with the transcript, arrival times, calls, every STL version and its
grade, and a copy of the run directory, and one line per eval in
`progress/agent-eval/cad-index.jsonl`.

### Progress snapshots and the hero image

`run_cad.py` keeps, for every run, each version of the model as the
agent wrote it, so a side-by-side can show how each condition's model
came together and how long it took. `hero.py` turns that into a still
for `og:image`/`twitter:image` and an animation.

**What is captured** (`cad-<ts>/<run>/progress.json`, beside the run's
other files). Every time is seconds since the run started on the
harness's monotonic clock; nothing reads the wall clock.

- `versions`: each STL version (as in the metrics above) with its time
  (the watcher's first sight of the finished file; it polls every 0.5 s),
  its `{part: copy}` state under `history/`, and the turn and output
  tokens reached by then.
- `sources`: each saved version of every `.scad` and `.py` file in the
  run directory, copied to `sources/` by a second watcher (same poll), with
  its non-blank line count. Files over 2 MB are noted, not copied.
- `stream`: one point per assistant message as the stream-json line
  arrived (the reader thread stamps each line): the message count so far
  and output tokens so far. The stream carries no cost until its result
  event, so cost is known only at the end (`final`).
- `timing`: how exact each kind of time is, in words.

The watchers only read the run directory; the agent sees nothing new.
A failure writing `progress.json` is logged and does not fail the run.

**Backfill** for runs recorded before this existed:

    scripts/agent-eval/cad/hero.py backfill [cad-<ts> ...]

reads `progress/agent-eval/` (`--src`) and writes
`progress/agent-eval/hero/runs/<record>/<run>/` (`--out`). STL versions
and their times come from the record (the newest regrade, whose version
states are complete): these are the harness clock recorded during the
run, as exact as live capture. Source versions are rebuilt by replaying
the transcript's successful Write, Edit and MultiEdit calls, each timed
by the arrival of its result line on the same clock (`arrivals.json`),
so within the stream's buffering of the real write. Files changed by
shell commands (`sed -i`, heredocs) are not seen: `source_replay` says,
per file, whether the replayed text matches the run's saved final copy.
A run without `arrivals.json` falls back on the events' own timestamps,
and failing those on order only; `timing` says which applied.

**Render**: `hero.py render --task T --neoscad PATH` renders every
version of the selected runs with NeoSCAD's command line (one renderer
for all three conditions, so their frames look alike): each part
imported, coloured from the site's palette, orthographic from one
fixed rotation (55, 0, 25), with one camera distance for every frame of
the composite. That distance comes from the largest bounding-box
diagonal among the task's reference solution (`refs/`) and the selected
runs' final models: the specs leave some sizes open, so a distance from
the reference alone cut larger models off, while a diagonal bounds every
projection of its box. Each frame is centred on its own model's bounding
box, since agents put parts at different origins. Every `neoscad` runs
under `guard.run` (2 GB, 300 s), and `hero.py` stops itself above 2 GB.
A version with no triangles, a missing STL or a failed render gets a
placeholder frame that says which.

**Source frames** (OpenSCAD and NeoSCAD conditions). NeoSCAD's agents
preview through the MCP server's `snapshot` tool and export an STL late,
so STL frames alone left that column empty for most of its run. For the
two `.scad` conditions `render` therefore also draws every saved source
version: after each save, the part files (`<part>.scad`) and everything
they `use` or `include` from the run directory, as they stood then
(`progress.source_versions`; a save that changes none of these, such as
a check script, adds no frame). Each part is exported to STL by the same
`neoscad` under `guard.run` (2 GB, 300 s) and drawn exactly as an STL
version is. The render enables only what the condition allowed
(`progress.enabled_features`): nothing for OpenSCAD; for NeoSCAD the
`--enable` arguments in the run's `mcp.json`, plus `part` when one of
the agent's MCP calls passed `parts: true`, which the server grants per
call. A version that needs a file the run directory did not have at that
time (a library path, a file outside the run directory, or one whose
text the backfill could not replay) shows "incomplete" and is not
rendered, rather than filled in from a later version; one that does not
export shows "render error". CadQuery sources are Python and would have
to run outside the sandbox, so that column keeps STL frames only.

A frame at clock time t shows the newest of the STL versions and the
rendered source versions at or before t; at an equal time the STL wins.
Source-rendered frames carry a `src` badge. The large "final" cell is
always the last STL version, the file the grader judged, so its pass or
fail line describes what it shows; only a run that never exported falls
back to its newest source frame there.

**Representative run** (`progress.representative`, tested in
`test_progress.py`): per task and condition, among the passing runs
the one with the median wall time; with no passing run, the median of all
runs. An even count takes the lower median, ties go to the smaller run
id, and pass comes from the record's newest regrade. No run is chosen
by hand. The pool is every record found; pass `--records cad-a,cad-b`
to compare like with like (`hero.py` warns when the selected runs differ
in model or effort). The task is the caller's choice (`--task`), one
composite per task.

**Compose**: `hero.py compose --task T` writes, under the output
directory:

- `hero-<task>.png`, 1200 × 630 (the `og:image` size): a column per
  condition with its name, its wall time and whether its final model
  passes, the final render large, three smaller frames at shared clock
  times (a quarter, half and three quarters of the slowest selected
  run), and a bar on one axis from 0 to that run's time, with ticks for
  STL versions (tall) and source writes (short);
- `hero-<task>.mp4` (H.264, 1200 × 630, 15 fps, a 12 s sweep and a 3 s
  hold; `--duration`, `--hold`, `--fps`), or an animated GIF without
  ffmpeg: every column on one linear clock with a running timer, each
  freezing at its run's end;
- `hero-<task>.json`: the runs picked and the rule, every candidate, the
  timing notes, the camera, the renderer's version, and per `.scad`
  column the features its source frames enabled.

Composition needs Pillow, which the CadQuery venv has; under another
Python, `hero.py` re-executes itself with the venv's interpreter
(`--pillow-python` to choose one). Fonts are the vendored Liberation
Sans and Mono; colours are the site's dark theme. `hero.py all --task T`
runs backfill, render and compose in turn. Before a column's first
frame it says "no model yet" ("no STL yet" for CadQuery), with the
source edits made so far.

Everything `hero.py` writes is a result: it goes to `progress/` (or
`results/` with `--out`) and is never committed.

    scripts/agent-eval/cad/test_progress.py      # selection rule, time axis, capture (synthetic runs)
    scripts/agent-eval/cad/hero.py all --task T2 --neoscad target/release/neoscad
