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

**What the grader does not judge**, and says so in the check (`ok:
null`, not gated): whether T1 prints without supports (bridges and small
horizontal holes are fine, so overhang area is only reported), whether
T2's snaps work (it reports the spread of the lip width, which shows
bumps), T2 hole positions against a PCB (the spec gives none), and
self-intersection (not computed). Known blind spots: a USB cutout made
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
and rings in the flange-down stack; a round flange; two barbs); and on each condition's toolchain
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
