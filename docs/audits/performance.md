# Audit: performance (after phase 8)

> **Status (2026-09-28).** The body below is the audit as written at
> `df6731d`; it is not updated.
>
> - **Superseded: the unwind finding.** B1, §1.2's conclusion ("abort
>   no longer helps"), §3's `panic=unwind` row and "Decisions" item 3
>   say unwind costs 0–1.5%. `docs/audits/unwind.md` (at `970f630`)
>   measured 5–7% on evaluation-bound BOSL2 models and up to 12% on
>   call-heavy code, once `bd4e4f0` had removed the limits' hot-path cost
>   that masked it. After D1 (`cd7d5c7`) the gap is 5–9%; D2 was measured
>   slower and reverted. Release builds still unwind.
> - **Opportunities:**
>   - O1 mimalloc: done, `b7e9941` (also removed the app's
>     `MallocLargeCache=0`).
>   - O2 include fragments: done, `4d877c7`; the off-path `Program` free
>     in `db54307`.
>   - O3 statement reuse: done, `0f4b8e8`.
>   - O4 static names: done for call sites and slots, `970f630`; fixed
>     (depth, slot) addressing is not (`docs/followups.md`).
>   - O5 STL/OFF writing and O6 cache keys: done, `e1a2b63`.
>   - O7 (R1) and O8 (R3, `concat`/`each`): done, `bd4e4f0`.
>   - O9 manifold-rust hole triangulation and O10 clipper2-rust rounding:
>     done, `9b89400`.
>   - O11 duplicate parallel subtrees: not done.
>   - O12 cold start: frameworks linked delay-init instead, `b7e9941`;
>     small chain-shaped renders skip the thread pool, `db54307`
>     (cold start 2.9 ms median). A `dlopen`ed renderer is not done.
>   - O13 (lazy booleans, GPU CSG preview, a parallel evaluator): not
>     started. Evaluator work after this audit is in
>     `docs/audits/bytecode-vm.md` and `docs/architecture.md`
>     ("Evaluator performance").

Audited at `df6731d` (clean tree), release build (`lto = "thin"`,
`codegen-units = 1`, `panic = "unwind"`), on an M4 Pro (10 performance +
4 efficiency cores, 48 GB), macOS 27.0, on AC power. The reference is the
nightly `/Applications/OpenSCAD.app` (2026.09.23, `--backend=manifold`).
No code was changed. The full benchmark is
`progress/bench/20260927T072506Z-df6731d.json`.

**Measurement conditions.** `dasd` (a system daemon) used about 94% of one
core for the whole session, and a virtual machine used another 5–20%.
Neither could be stopped. So every comparison below is an **interleaved
A/B**: the binaries alternate run by run, and the best of N is reported.
Absolute times from separate sessions can be a few percent off. Older
builds were release-built from `git worktree`s in the scratchpad (same
profile, `neoscad` binary only). CPU profiles come from `xctrace` (Time
Profiler, 1 ms samples), parsed per function and per 25–500 ms time
bucket. `sample` could not attach to runs this short.

## On firm ground

- **neoscad is still faster than the nightly on 13 of 14 benchmark
  models**, with a geometric mean of 2.24× (it was 2.45×). The only
  model where the nightly wins is `text_30lines`: 0.639 s against 0.633 s,
  which is a tie within noise.
- **The agent edit loop has not regressed.** A BOSL2 one-line edit to a
  snapshot takes 45.5 ms through `serve` (7a recorded 44 ms), 73.4 ms cold
  (74 ms in 7a), and 252.6 ms on the nightly.
- **The hero is reproduced:** 3.69 s for neoscad against 6.09 s for the
  nightly (`docs/icon.md` says 3.66 s and 6.05 s).
- **The evaluator is about 2× faster than the nightly per operation**
  (table in 2.5). There are two exceptions. Arithmetic and indexing are at
  parity. List accumulation through `concat`/`each` is **9× slower**
  (finding R3).
- **The regressions are small and understood.** Evaluation-bound models
  are 4–9% slower and every process starts 1.3 ms later. Each has a cause
  pinned to a commit (section 1).

## Summary of findings

| # | Finding | Size | Affects |
|---|---|---|---|
| R1 | Evaluation got 4–9% slower. The cause is now the resource-limits commit `508d1d0`, **not** `panic = "unwind"` | isosurface +6.8%, fractal_tree +6.6%, screws +4.5% | every BOSL2 model, every client |
| R2 | Cold start rose from 2.8 to 4.3 ms, because the CLI now links Metal, QuartzCore and Foundation (`9336b1c`) | +1.3 ms per process | one-shot CLI, `eval_only` (+1.3 s over 976 processes), small models |
| R3 | `concat(acc, [x])` and `[each acc, x]` accumulation is O(n²). OpenSCAD's is O(n) | 20k elements: 521 ms against 59 ms | tail-recursive accumulators (some BOSL2 path and string code, and user code) |
| B1 | The brief's "panic=unwind costs 5–8%" (`docs/architecture.md:164-167`, `docs/followups.md:356-363`) no longer holds | 0–1.5% at HEAD | documentation, and the decision about an `abort` profile |
| H1 | In the hero, evaluation is 67% of wall time, all on one core. BOSL2's `isosurface()` is 87% of that | 2.47 s of 3.69 s | heavy BOSL2 models, the edit loop on them |
| H2 | Serial stretches after evaluation: cache keys (0.20 s) and ASCII STL writing (0.28 s) | 13% of the hero, 33% of `csg_spheres` | every export of a big mesh |
| H3 | A one-line edit to the hero through `serve` still costs 2.9 s, because evaluation is never reused | 2.47 s of 2.9 s | the agent loop on heavy models |

Ranked opportunities are in section 4.

## 1. Regression check

### 1.1 Full benchmark against the earlier run

`conformance bench` was run in full: all references, best of 3, cold
start, `eval_only` and the edit loop. It is compared with
`20260926T150451Z-496a74d-dirty.json`. The nightly column is the control:
the binary is the same, so a change there is noise.

| Model | neoscad before | neoscad now | Δ | Nightly Δ (control) |
|---|---|---|---|---|
| bosl_fractal_tree | 5.917 | 6.216 | +5.0% | −2.5% |
| bosl_gears__003 | 0.0661 | 0.0701 | +6.1% | +2.3% |
| bosl_isosurface__006 | 1.265 | 1.432 | +13.2% | +5.2% |
| bosl_screws__001 | 0.269 | 0.290 | +7.9% | +0.5% |
| bosl_spring_handle | 0.358 | 0.365 | +2.0% | −1.4% |
| csg_deep_union | 0.0491 | 0.0526 | +7.1% | +0.6% |
| csg_spheres | 0.731 | 0.721 | −1.4% | −3.1% |
| ex_csg_basic | 0.0084 | 0.0166 | +97.6% | +23.6% |
| ex_menger | 0.159 | 0.181 | +14.3% | +13.4% |
| extrude_twist | 0.0872 | 0.0884 | +1.4% | −0.7% |
| import_stl | 0.175 | 0.165 | −5.7% | −2.1% |
| mink_convex | 0.0243 | 0.0268 | +10.3% | −3.0% |
| mink_nonconvex | 0.0128 | 0.0148 | +15.6% | −4.5% |
| text_30lines | 0.637 | 0.639 | +0.4% | −8.0% |
| cold_start | 0.0022 | 0.0041 | +86% | −1.7% |
| eval_only (976 processes) | 31.6 s | 34.1 s | +7.8% | −0.7% |

A best-of-3 bench run is too noisy for the small models: `ex_menger`'s
control moved as much as neoscad did. So every model over 3% was re-timed
as an interleaved A/B against a fresh release build of `496a74d` (5–7
runs):

| Model | `496a74d` | HEAD | Δ | Cause |
|---|---|---|---|---|
| bosl_isosurface__006 | 1.239 s | 1.353 s | +9.2% | R1 |
| bosl_screws__001 | 274 ms | 289 ms | +5.4% | R1 |
| bosl_spring_handle | 353 ms | 372 ms | +5.5% | R1 |
| bosl_gears__003 | 66.9 ms | 70.4 ms | +5.3% | R1 (about 3%) + R2 (1.3 ms) |
| bosl_fractal_tree | 5.81 s (`49cf1c0`) | 6.19 s | +6.6% | R1 |
| csg_deep_union | 50.0 ms | 52.1 ms | +4.2% | R2 (1.3 ms) + 0.8 ms unexplained |
| mink_convex | 24.4 ms | 26.6 ms | +9.1% | R2 + about 0.9 ms |
| mink_nonconvex | 13.5 ms | 15.5 ms | +14.9% | R2 + about 0.7 ms |
| ex_csg_basic | 8.6 ms | 10.1 ms | +17.8% | R2 (the bench's +98% was noise) |
| ex_menger | 155 ms | 159 ms | +2.0% | R2 (the bench's +14% was noise) |
| cold_start | 2.8 ms | 4.3 ms | +55% | R2 |

### 1.2 R1: evaluation cost, bisected

Release builds at each commit, interleaved, 5 runs, isosurface model
(`bosl_isosurface__006`; screws agrees):

| Build | isosurface | vs `49cf1c0` |
|---|---|---|
| `496a74d` (panic=abort) | 1235 ms | — |
| `8581f5a` | 1244 ms | +0.7% |
| `49cf1c0` (7a, abort) | 1235–1251 ms | 0 |
| `bf43852` (7b-1, **unwind**) | 1364–1376 ms | +10.0% |
| `bf43852` rebuilt with `panic=abort` | 1253 ms | +0.1% |
| `37a4e53` (unwind) | 1361–1369 ms | +9.3% |
| `37a4e53` with abort | 1267 ms | +1.8% |
| `508d1d0` (H4 limits, unwind) | 1327–1337 ms | +7.3% |
| `508d1d0` with abort | 1362 ms | **+9.3%** |
| HEAD (unwind) | 1320–1335 ms | +6.3–6.8% |
| HEAD with abort | 1349–1356 ms | +8.3–8.7% |

What this shows:

- **At 7b-1, unwind did cost about 8–10%,** exactly as documented, and
  building with abort removed it.
- **Since the resource limits (`508d1d0`), abort no longer helps.** At
  HEAD, unwind and abort are within 2% of each other on isosurface, screws
  (`+5.3%` against `+3.5%`), gears and fractal_tree (6.19 s against
  6.10 s). Both sit 5–9% above `49cf1c0`. The cost moved from the
  unwinding tables to the limits' hot-path checks. Both show up as the
  same kind of code-generation cost in the evaluator's innermost loop.
- **The profile agrees.** Comparing `49cf1c0` with HEAD-abort on
  isosurface, `branch<Value, Box<Unwind>>` (the `?` on every `eval`)
  goes from 2.5% to 5.7% self. `eval` is no longer inlined (2.7% self),
  and `eval_args` doubles (1.2% to 2.4%). The added work per expression is
  `check_hard`'s second `Cell` test (`crates/eval/src/eval.rs:496-507`),
  run after every `eval`. On top of that come `list_bytes` charges and
  credits on every `Vector` build and drop
  (`crates/eval/src/value.rs:347-411`) and the list cap in
  `eval_element`.
- **One experiment:** an `#[inline(always)]` fast test plus a `#[cold]`
  slow path for `check_hard`. It recovered about 1.5 points (isosurface
  +6.8% down to +5.2%; screws +4.5% to +2.8%). The rest needs a builder
  with a profiler (opportunity O7).

### 1.3 R2: cold start

`--version` alone takes 2.0 ms at `496a74d` and 3.3 ms at HEAD. Timed at
each commit, `cold_start` steps from 3.3 to 4.7 ms at `9336b1c` (the
render crate). `otool -L` shows that the HEAD binary links QuartzCore,
CoreGraphics, Metal, Foundation, CoreFoundation and libobjc, while
`496a74d`'s links only libSystem. So the cost is dyld loading those
frameworks at every launch, including runs that never draw.

### 1.4 App-side numbers

- **`MallocLargeCache=0`** (the app's `LSEnvironment`,
  `docs/followups.md:578`), A/B as an environment variable on the CLI, 9
  runs: `csg_spheres` +6.9% (720 to 770 ms), screws +1.1%, isosurface
  +0.4%. The cost is on large-buffer geometry, as 8f found (8%).
- **mimalloc** (an experiment, opportunity O1) makes the same models
  faster than the system allocator *with* its large cache, and lowers
  peak RSS. Whether it also releases memory at idle the way
  `MallocLargeCache=0` does was not measured in the app (see "Could not
  verify").

## 2. Planetary gearbox (`apple/Icon/hero.scad`)

### 2.1 Phase breakdown

The hero was split into its five top-level objects, each file with the
same includes and `planetary_gears()` header (`h_*.scad` in the
scratchpad). Each was timed as `.echo` export (parse and evaluate only),
`.stl` export (everything), and a full-model profile. Best of 3:

| Phase | neoscad | nightly |
|---|---|---|
| Process start, read and parse BOSL2 `std`+`gears`+`isosurface`, `planetary_gears()` | 31 ms | 150 ms |
| Evaluate: sun | 50 ms | 95 ms |
| Evaluate: 4 planets | 152 ms | 292 ms |
| Evaluate: ring | 56 ms | 106 ms |
| Evaluate: **plinth (`isosurface()` gyroid)** | **2,207 ms** | 3,774 ms |
| Evaluate: carrier | 38 ms | 68 ms |
| **Evaluate, whole model** (`-o x.echo`) | **2,515 ms** | 4,467 ms |
| Geometry cache keys (`eval::dump::Keys::new`) | 200 ms, serial | n/a |
| Geometry: gears, herringbone extrusions and booleans | runs alongside the plinth, about 2 cores | |
| Geometry: plinth (polyhedron to Manifold, intersection, rim, cutaway) | 840 ms alone (1.18 s CPU) | 1,080 ms |
| Final union, then ASCII STL writing (`io::stl::write`) | 280 ms, serial | |
| **Total** | **3,692 ms** | 6,088 ms |

The parts, measured alone (`.stl` minus `.echo`), give the geometry
split: sun 58 ms, planets 197 ms (0.73 s CPU), ring 99 ms, plinth
840 ms and carrier 43 ms.

Inside `isosurface()`, measured by calling BOSL2's internal steps
directly (`iso/s*.scad`):

| Step | neoscad | nightly |
|---|---|---|
| Field: 151,424 calls of the `gyroid` function literal | 183 ms (1.2 µs a call) | 320 ms |
| `_isosurface_cubes` (104×104×14 voxels; 79,256 kept) | 1,196 ms (about 9 µs a voxel) | 2,082 ms |
| `_isosurface_triangles` (591,246 points) | 624 ms | 1,062 ms |
| The `faces` filter (`norm(cross(...))`) | 116 ms | 130 ms |
| The module wrapper (`vnf_polyhedron`, anchors) | 21 ms | 76 ms |

**Why `_isosurface_cubes` dominates.** Per voxel
(`.reference/BOSL2/isosurface.scad:837-874`) it does 8 corners of
`min(1e9, max(-1e9, field[i][j][k]))`: 16 builtin calls and 24 levels of
indexing. On top of that it builds the `cf` vector, calls `min(cf)` and
`max(cf)` and `_bbox_faces()`, and evaluates about 12 `let` bindings in
seven nested scopes. That is about 60 interpreter operations at the
costs in 2.5.

### 2.2 Thread utilisation over time

From the full-model profile (100 ms buckets; "cores" is samples per
millisecond):

| Time | Cores busy | What |
|---|---|---|
| 0–2,480 ms | 1.0 | Evaluation (`eval::Evaluator`), all on the main thread |
| 2,480–2,700 ms | 1.0 | `Keys::new`: formatting the plinth polyhedron's 591k points to text for SHA-256 (`grisu::format_shortest_opt` 17% of the window, `sha256` 6%) |
| 2,700–2,800 ms | 6.8 | Geometry starts: leaves and gear booleans in parallel |
| 2,800–3,400 ms | 1.6–2.3 | Plinth booleans and `from_polyset`, which dominate; other subtrees finish |
| 3,400–3,700 ms | 1.0 | Root union, then `io::stl::write`: `text::shortest` 66% of the window, `format!` and `String` reallocation |

**About 80% of the wall time runs on one core while 13 idle.** The
evaluator is single-threaded by design, as OpenSCAD's is. The only
parallel stretch is about 0.7 s of geometry.

### 2.3 Cache behaviour

- **One-shot:** 12 geometry cache entries. Within one render the cache
  only helps with repeated subtrees (`cutaway()` twice). Parallel
  siblings with the same key are all computed, because there is no
  in-flight deduplication. On concept C this costs about 0.7 s of CPU for
  36 identical `channel()` polyhedra (RAYON_NUM_THREADS=1 uses 4.05 s of
  CPU against 4.76 s in parallel).
- **Served (the edit loop):** an edit to one line of the carrier
  (`cyl(h = 20, d = 26)`) re-renders in **2.90 s**: parse 18 ms,
  evaluate 2,472 ms, geometry 393 ms (5 edits, `serve_drive.py`). The
  geometry cache does its job, and only the carrier, the root union and
  the keys are recomputed. But **evaluation is never reused**, so an agent
  pays 2.5 s per edit on this model whatever line it touches (H3).

### 2.4 What dominates evaluation, and why

Self time over the evaluation window (2,450 samples), grouped by
function. The grouping is approximate, because inlined helpers such as
`as_slice<>` and `get<>` are attributed by name:

| Share | What | Main symbols |
|---|---|---|
| about 25–30% | **Name lookup:** walking the context chain for variables and functions | `Ctx::lookup_lexical` (13.9% inclusive), `lookup_function` + `local_function` (12.2%), `Vars::get`/`position` (linear scan below 12 entries), hash-map `get` for scope function tables and `builtin_fns` |
| about 22% | **Allocator** | `_xzm_malloc_tc`, `_xzm_free_tc`, `_free`, `realloc`: an `Rc<Ctx>` plus a `Vec` for every call, `let` and loop iteration, a `Vec<ArgVal>` for every call's arguments, and every small vector |
| about 6% (+ 10% `Rc<Ctx>` and `Vec` drops inclusive) | **Drops** | `drop_glue<Value>`, `Rc<Ctx>::drop_slow` (5.3% inclusive), `Vars` drop |
| about 29% | **Dispatch** | `eval_expr`, `eval`, `branch<Value, Box<Unwind>>`, `check_hard`, `simplify`, `eval_element`, `sequential_assign` |
| about 6% | **Real work:** operators, indexing, builtins, trig | `ops::index`, `min_max`, `ops::sub` |

The code behind it:

- **Variables.** Every variable read walks the parent chain
  (`crates/eval/src/context.rs:153-161`), borrowing each frame's
  `RefCell` and scanning its `Vars`. Inside `_isosurface_cubes`, reaching
  `field` or `voxsize` crosses about 8 frames.
- **Functions.** Every call to a builtin (`min`, `max`, `len`, `abs`)
  walks the whole chain down to the builtin context
  (`crates/eval/src/call.rs:437-515`). At each scope frame that costs a
  hash lookup in the scope's function table, a `Vars` lookup, and at the
  file frame the `uses` list (cloned at `call.rs:499`), before
  `builtin_fns`.
- **Calls.** Every user call allocates a child `Ctx` (`call.rs:419`), an
  argument vector (`eval_args`, `call.rs:49-57`) and a `Vars` frame
  (`bind`, `call.rs:62-128`). It also copies the caller's `$` variables
  into the new frame (`copy_config`, `call.rs:206-217`).

### 2.5 Micro-benchmarks: the evaluator against the nightly

Each file is a 1M-element list comprehension (fewer where noted),
exported to `.echo`, best of 3. The cost of an operation is the file's
time minus the plain loop's (or minus the empty file, or minus
include-only for the BOSL2 rows), divided by the iteration count. The
files are in the scratchpad (`micro/`).

| Operation | neoscad | nightly | Nightly ÷ neoscad |
|---|---|---|---|
| Loop iteration (list comprehension element) | 38 ns | 80 ns | 2.1× |
| `i*2+1` (two arithmetic ops) | 18 ns | 14 ns | 0.8× |
| `f(i)`, one parameter | 111 ns | 217 ns | 1.9× |
| `f(...)`, 5 positional | 180 ns | 521 ns | 2.9× |
| `f(...)`, 5 named | 229 ns | 753 ns | 3.3× |
| `f(i)`, 1 given and 4 defaults | 170 ns | 422 ns | 2.5× |
| Function literal `g(i)` | 112 ns | 227 ns | 2.0× |
| `let` with 3 bindings | 89 ns | 222 ns | 2.5× |
| 4 nested `let`s plus a global read | 193 ns | 320 ns | 1.7× |
| `v[i % 1000]` | 41 ns | 35 ns | 0.9× |
| `len(v)` (builtin call) | 64 ns | 108 ns | 1.7× |
| `sin(i)` | 73 ns | 105 ns | 1.4× |
| `f(i)` that reads `$fn` | 135 ns | 251 ns | 1.9× |
| `concat([i], [1, 2])` | 172 ns | 862 ns | 5.0× |
| Non-tail recursive call (depth 5,000) | 160 ns | 283 ns | 1.8× |
| `a(i)`→`b`→`c`→`d` (per `a()`) | 510 ns | 977 ns | 1.9× |
| `[i,1,2] + [1,2,3]*2` | 188 ns | 447 ns | 2.4× |
| `[i, j]` element in a nested comprehension | 86 ns | 347 ns | 4.0× |
| BOSL2 `default(i, 1)` (with the loop) | 218 ns | 464 ns | 2.1× |
| BOSL2 `path_length` of 100 points | 55 µs | 91 µs | 1.6× |
| BOSL2 `move()` of 1,000 points | 641 µs | 1,196 µs | 1.9× |
| BOSL2 `apply(rot(30)*scale(2), …)` of 1,000 points | 650 µs | 1,214 µs | 1.9× |
| **`build(n, concat(acc, [n]))`, 20,000 deep** | **521 ms** | **59 ms** | **0.11×** |
| **`build(n, [each acc, n])`, 20,000 deep** | **529 ms** | **56 ms** | **0.11×** |

What the numbers show:

- **The cost is per call and per scope, not per argument.** A call costs
  about 110 ns, and each further parameter adds about 15 ns (named: 25 ns).
  Each `let` costs about 30 ns per binding, or about 40 ns per nested
  scope. A builtin call costs 64 ns, 3.5× an arithmetic op, because the
  name is looked up the long way.
- **Loaded BOSL2 does not slow lookups much.** A BOSL2 global read costs
  about 6 ns more than a local. A call to `abs` from a function defined
  next to BOSL2 costs about 12 ns more than the same call without BOSL2.
  The file frame's hash maps are fine. The cost is the number of frames
  walked.
- **Accumulation is the one real gap.** OpenSCAD's `concat` wraps each
  argument vector as an `EmbeddedVectorType` instead of copying it
  (`.reference/openscad/src/core/builtin_functions.cc:449-461`,
  `Value.h:90-107`). So `concat(acc, [x])` in a tail-recursive loop is
  O(1) a step there. Here the builtin copies (`crates/eval/src/builtins/functions.rs:341-364`),
  because `acc` is still held by the caller's frame, so
  `Vector::into_vec` (`value.rs:328-338`) clones. The same holds for
  `each`.

**The icon builder's note** was that hoisting `path()` out of a
per-vertex loop cut a model from 4.0 s to 2.8 s. The per-vertex version
was never committed (`git diff 8e3070b df6731d -- apple/Icon/concept-c.scad`
shows only the hoisted one), so the 4.0 s run cannot be reproduced. A
reconstruction (`micro/28` against `29`: 540 × 72 vertices, with and
without a `path()` per vertex) costs 869 ns per `path()` call here
against 1,376 ns in the nightly. So the overhead is real, but it is not
a neoscad-specific pathology. On concept C as committed, evaluation is
still 1.58 s of 2.80 s (nightly: 2.48 s of 8.17 s), because `channel()`
is instantiated 36 times and rebuilds its 38,880 points each time. That
is OpenSCAD's semantics too.

## 3. Known gaps, re-measured

Best of 3 unless noted, interleaved with the nightly.

| Gap (followup) | Now: neoscad | Nightly | Profile |
|---|---|---|---|
| Menger level 4 (`docs/followups.md:7`) | 2.87 s (8.2 s CPU) | 2.01 s (14.4 s CPU) | 5.6–7.9 cores for the first 1.3 s. Then **1.2 s on one core** in the last big differences: `simplify_topology` (`collapse_colinear_edges` 15%, `collapse_edge`, `split_pinched_verts`) and boolean assembly. Unchanged diagnosis: eager booleans, and few parallel kernels in manifold-rust (8 `maybe_par` sites) |
| 200 lines of `text()`, 2D, to SVG (`:24`) | 2.85 s (5.4 s CPU) | 1.63 s | SVG byte-identical. 0.25 s of parallel shaping, then **2.0 s serial** in clipper2-rust's union: `build_intersect_list` 27%, `top_x` 23%, **`nearbyint_f64` 16%** (a software rounding function, `clipper2-rust-1.2.0/src/core.rs:1603`) |
| The same, `linear_extrude(2)` (not in followups) | 64.4 s | 31.5 s | 50 lines: 4.98 s against 3.12 s. 68% of it is `manifold_rust::polygon::triangulate`, in `cut_keyhole`→`loop_verts`, which builds a `Vec` of every outer vertex for each hole (`vendor/manifold-rust/src/polygon_earclip.rs:169-194, 541-633`). `Vec` growth alone is 29% |
| Circle-grid extrude (`:19`) | 71×71 circles, `$fn=16`: 0.24 s; as 5,041 holes in a square: 1.35 s | 0.53 s; 1.42 s | **Parity or better.** The holes case is 60% `loop_verts`, 7% `realloc`. The followup's exact model is not recorded, so its 1.0 s against 0.7 s could not be re-run |
| OFF `fmt_g` export (`:16`), 1M-triangle `linear_extrude(twist=720, slices=2000) circle(10, $fn=250)` | OFF 0.46 s; ASCII STL 1.26 s | OFF 1.41 s; STL 3.21 s | **neoscad now leads 3×.** The followup's 0.55 s against 0.41 s was for an unrecorded base shape. The STL path is the heavier one here (H2, O5) |
| Preview of cube − 125 spheres (`:268-273`) | 356 ms (1.04 s CPU) | 253 ms | 75–300 ms is the real boolean (`union_tree` of the 125 spheres, then the product), 11 cores for 50 ms then 1–4. OpenCSG never computes it. Drawing is about 40 ms |
| BOSL2 edit-loop include re-parse (`:304`) | serve: 33.4 ms median; parse 15.3, evaluate 17.1, geometry 0.6 | nightly cold 172 ms | Over 300 edits: `Session::load` 44% (`lower` 20%, `parse` 12.8%, include `splice` 8%), evaluation 39% (the model's own `cuboid`/`cyl`/`prismoid` module bodies, not BOSL2's constants: an include-only file evaluates in ≈0), dropping the previous `Program`/`Ast` 5.7% |
| `panic=unwind` cost (`:356`) | 0–1.5% at HEAD (isosurface −2%, screws +1.8%, fractal_tree +1.5%) | — | See 1.2. The documented 5–8% was true at 7b-1 |
| CLI cold start (`architecture.md:54`) | 4.1–4.3 ms | 46 ms | +1.3 ms since `9336b1c` (R2). Still 10× under the nightly |

## 4. Opportunities, ranked by expected wall time saved in realistic use

Ranking puts the agent edit loop and typical BOSL2 models first. "Group"
marks files. Items in different groups can run on parallel builders
without touching the same files. Items in the same group must be run one
after another.

### O1. mimalloc as the global allocator (CLI, serve, app)

- **Evidence:** a scratch build with `mimalloc 0.1.52` as
  `#[global_allocator]` in `crates/cli/src/main.rs`, run interleaved
  against HEAD (5 runs; 3 for the long models):

  | Model | Change |
  |---|---|
  | isosurface | −8.2% |
  | screws | −8.9% |
  | spring_handle | −11.7% |
  | gears | −8.7% |
  | csg_spheres | −14.6% |
  | ex_menger | −11.4% |
  | deep_union | −15.4% |
  | fractal_tree | −7.2% |
  | hero | −6.8% |
  | concept C | −8.2% |
  | menger4 | −6.6% |
  | 125-sphere preview | −7.7% |
  | 2D text | ±0 |
  | serve BOSL2 edit | −11% (34.1 to 30.5 ms median) |

  Peak RSS also falls by 17–20% (`csg_spheres` 640 to 513 MB; fractal_tree
  1.95 to 1.60 GB; hero 1.30 to 1.07 GB). The hero's STL is
  byte-identical over 3 runs. Cold start is +0.3 ms.
- **Change:** add `mimalloc` to `crates/cli` and `crates/ffi`, with
  `#[global_allocator]` set on native targets only
  (`cfg(not(target_arch = "wasm32"))`). Library crates are untouched. In
  the app, measure idle footprint with and without `MallocLargeCache=0`
  (`docs/followups.md:566-590`). mimalloc's own purging may make that
  environment variable unnecessary.
- **Gain:** 7–15% on everything allocation-heavy, which includes the
  edit loop.
- **Risk:** no effect on output; allocation order does not reach results.
  It adds a C dependency built by `cc` (MIT licence, compatible with
  GPL-2.0-or-later). That is an **owner decision**, since the stack is
  otherwise pure Rust apart from system frameworks.
- **Effort:** S (plus an app memory check). **Group A:** `crates/cli`,
  `crates/ffi`, `Cargo.lock`.

### O2. Parse each included file once and splice ASTs (the known 7a followup)

- **Evidence:** parse is 15.3 of 33.4 ms of a served BOSL2 edit (46%):
  lower 20%, parse 12.8% and include splice 8% of samples. Cold CLI edits
  pay it too (47.5 ms).
- **Change:** as `docs/followups.md:304-314` describes. Cache the lowered
  AST per included file (keyed on path + content hash + the include's
  lexical position), and splice the scopes. Take care with reassignment
  across includes and with includes that are not whole statements. Files:
  `crates/lang/src/loader.rs`, `crates/lang/src/ast.rs`,
  `crates/session/src/parse.rs`. Also drop the previous `Program` on a
  background thread, or after answering (5.7% of samples, about 1.5 ms an
  edit): `crates/session/src/lib.rs`.
- **Gain:** a served BOSL2 edit goes from about 33 to about 19 ms
  (about −40%), or about 16 ms with O1. The CLI cold path gains less (it
  still reads and lexes).
- **Risk:** medium. AST dumps (tier 0) and include semantics must stay
  identical. The `.ast` export and the LSP use the same loader.
- **Effort:** M–L. **Group E:** `crates/lang`, `crates/session/src/parse.rs`.

### O3. Reuse top-level statements' evaluation across edits in the session

- **Evidence:** H3. A one-line carrier edit to the hero costs 2.47 s of
  evaluation for a result the geometry cache already holds. For typical
  multi-part BOSL2 files, each edit re-evaluates every part.
- **Change:** in `session`, memoise each top-level instantiation's node
  subtree and messages. Key it on:
  - its source span's text;
  - the values of the top-level variables it reads (a free-variable set
    computed when lowering);
  - the definitions of every function and module it can reach. Simplest
    first version: one hash of all definitions in the program, BOSL2
    included, so that editing any definition invalidates everything;
  - the `$` variables at file scope and the `-D` overrides;
  - the timestamps of files it `import()`s or `surface()`s.

  Don't cache a statement that called unseeded `rands()` or
  `parent_module`, or that reached a resource limit. Replay its echoes
  and warnings in order. Files: `crates/session/src/lib.rs`,
  `crates/eval/src/inst.rs` (per-statement entry and free-variable
  capture), `crates/lang/src/ast.rs` (free variables per statement).
- **Gain:** hero edits outside the plinth: 2.9 s to about 0.4 s (0.2 s
  after O6). For typical BOSL2 multi-object files, an edit costs one
  object's evaluation instead of all of them. OpenSCAD has no equivalent.
- **Risk:** medium–high for invalidation bugs, and zero for cold renders.
  It is testable by construction: in a test mode, re-render every warm
  result cold and diff the `.csg` and echo. Run that over `examples/`,
  MCAD and the BOSL2 tests.
- **Effort:** L. **Group F:** `crates/session`, `crates/eval/src/inst.rs`.
  This conflicts with group D on `inst.rs`; sequence it after O4 or
  agree on the entry point first.

### O4. Resolve names statically in the evaluator

- **Evidence:** name lookup is about 25–30% of evaluation self time on
  the hero (2.4). A builtin call costs 64 ns, of which about 45 ns is the
  walk. The tightest loops in BOSL2 (`min`/`max`/`len`/indexing) pay it
  on every operation.
- **Change, in two steps:**
  1. **Call sites.** When lowering, record for each call site whether
     any lexically enclosing binder (a parameter, `let`, `for`, or
     assignment in an enclosing statement scope) can bind that name. If
     none can, resolve straight to the file scope's function table, then
     the `use`d libraries, then builtins, skipping the frame walk.
     Otherwise fall back to the walk.
  2. **Variables.** Give `let`/`for`/parameters `(depth, slot)`
     addresses, so a read is a fixed number of parent hops and an index.
  
  Files: `crates/lang/src/ast.rs` (the annotations), `crates/eval/src/call.rs`
  (`lookup_function`, `local_function`), `crates/eval/src/context.rs`,
  `crates/eval/src/eval.rs`.
- **Gain:** about 15–25% of evaluation on BOSL2-heavy models: hero about
  −0.4 to −0.6 s, isosurface model about −0.25 s. The edit loop's 17 ms of
  evaluation shrinks about 3 ms.
- **Risk:** medium. Dynamic `$` scoping, function-valued variables
  shadowing functions, `use` versus `include`, and recursion-budget
  frames all have to be preserved. Tier 1 and 2 conformance plus the BOSL2
  suite (`eval_only`: 976 tests) cover it well.
- **Effort:** L. **Group D:** `crates/eval` (eval, call, context), `crates/lang/src/ast.rs`.
  `ast.rs` is shared with O2, so agree the order.

### O5. Allocation-free ASCII STL (and OFF) writing

- **Evidence:** `io::stl::write` is 225 ms of `csg_spheres`' 700 ms (all
  serial after geometry) and 280 ms of the hero's 3.7 s. The profile shows
  `text::shortest` 66%, `format!` 51% and `String` reallocation 40%
  inclusive. A 5-line scratch change pre-reserved the output buffer and
  replaced `shortest`'s intermediate `format!`/`collect` with
  pre-sized `String`s (`crates/io/src/stl.rs:171-186`,
  `crates/io/src/text.rs:120-160`). Output stayed byte-identical (hero
  STL compared). Results: `csg_spheres` −12.2%, screws −5.9%, the
  1M-triangle twist extrude **−31.7%** (1.25 to 0.85 s).
- **Change:** write digits into a stack buffer and append straight to one
  pre-sized `Vec<u8>`, with no `String` per number. Format each normal
  once per facet into that buffer. Apply the same to OFF/OBJ/WRL through
  `lang::number::fmt_g` (`crates/io/src/off.rs` and others). Optionally
  format vertex strings in parallel chunks, which is deterministic because
  the strings are independent.
- **Gain:** 5–30% on any model exported as ASCII STL, which is what the
  bench and most agents use. About 0.2 s on the hero.
- **Risk:** low, with exact byte comparison against the current writer
  over the tier 3 exports. Keep `shortest_matches_double_conversion`.
- **Effort:** S. **Group B:** `crates/io`, `crates/lang/src/number.rs`.

### O6. Cheaper geometry cache keys

- **Evidence:** `Keys::new` formats every number of every node with
  `format!("{v:?}")` before SHA-256 (`crates/eval/src/dump.rs:124-137`).
  On concept C that is 650 ms serial (23% of wall time: 36 channel
  polyhedra of 38,880 points), and 200 ms on the hero. A scratch change
  wrote the key's numbers as `to_bits()` hex without allocating. Concept C
  went **−11.3%** (2.85 to 2.53 s), the hero −2.7%, and the output was
  byte-identical.
- **Change:** in `Style::Key`, feed numbers to the hash as their 8 bytes,
  with an unambiguous framing (a tag byte, and length-prefixed strings),
  rather than as text. Emit polyhedron point and face lists straight from
  the `Value`s. Hash sibling subtrees on rayon: a Merkle hash is
  order-independent to compute.
- **Gain:** 0.1–0.6 s on models with big polyhedra (BOSL2's VNF output:
  `isosurface`, `sweep`, rounded shapes). The same cost recurs in every
  served re-render.
- **Risk:** low. The key is internal. Bits are exactly as fine as `{:?}`,
  except that NaN payloads now split keys, which is harmless. The `.csg`
  export (`Style::Csg`) is untouched.
- **Effort:** S. **Group C:** `crates/eval/src/dump.rs` only.

### O7. Win back R1 (limits and unwind code generation in the evaluator)

- **Evidence:** section 1.2. The measured `#[cold]` split of `check_hard`
  recovers about 1.5 of the 5–9 points.
- **Change:** keep one `Cell<bool>` "something pending" flag that
  `hard`/`limit` set, and test only that on the hot path. Move
  `list_bytes` accounting out of every small `Vector` build and drop:
  only lists of at least `LIST_MIN` count, so test `len()` before calling
  anything. Check the list cap in `eval_element` only when the length
  crosses a threshold. Re-profile `eval`/`eval_args` inlining with
  `#[inline]` hints. Files: `crates/eval/src/eval.rs`,
  `crates/eval/src/value.rs`.
- **Gain:** up to 5–7% of evaluation (isosurface about −80 ms,
  fractal_tree about −0.3 s).
- **Risk:** low. Limit behaviour has tests in `crates/eval` and `session`.
- **Effort:** S–M. **Group D** (conflicts with O4 and O8).

### O8. O(1) amortised `concat`/`each` accumulation

- **Evidence:** 2.5, 9× slower than the nightly at 20,000 steps, and
  quadratic. BOSL2 has the pattern in `path_cut_points_recurse`
  (`paths.scad:1090`), `_err_resample` (`paths.scad:716-723`),
  `_fix_angle_list` (`rounding.scad:3875-3877`) and `str_split`
  (`strings.scad:256-264`). Agents write it often.
- **Change, in order of effort:**
  - (a) In a tail call, move argument values out of the old frame once
    they are bound, so that `concat(acc, …)` sees a unique `acc`.
    `Vector::into_vec` then reuses the buffer, and `Vec` growth is
    amortised.
  - (b) Give `concat` an exact `with_capacity(n)`: it already computes
    `n` at `functions.rs:344-350` and then allocates `a.len()`.
  - (c) A rope/embedded vector like OpenSCAD's, for the general case.

  Files: `crates/eval/src/call.rs`, `builtins/functions.rs`, `value.rs`.
- **Gain:** large on the affected code (0.5 s at 20k, growing
  quadratically), and nothing elsewhere.
- **Risk:** (a) and (b) are low. (c) is high, because it touches every
  vector consumer.
- **Effort:** S for (b), M for (a). **Group D.**

### O9. manifold-rust: stop materialising `loop_verts` for every hole

- **Evidence:** `loop_verts` is 60% of the 5,041-hole extrude and a
  large part of 3D text. `Vec` growth is 29% of the 50-line text extrude.
  It is called twice per hole over every outer loop, and `self.outers` is
  cloned each time (`vendor/manifold-rust/src/polygon_earclip.rs:558, 618`).
- **Change:** iterate the ring in place (a closure, as C++'s `Loop`
  does), or reuse one buffer. Borrow `outers` instead of cloning it.
  Keep the visiting order identical. This goes in the existing vendor
  patch, and is documented in `vendor/README.md`.
- **Gain:** about 25–40% of triangulation on many-holed shapes (3D text:
  maybe 64 s down to 45 s on the 200-line case; the circle grid 1.35 s
  down to about 0.9 s).
- **Risk:** low. The order is unchanged, so the triangles are unchanged.
  Diff meshes on the tier 3 extrude cases.
- **Effort:** S. **Group G:** `vendor/manifold-rust`.

### O10. clipper2-rust: hardware rounding

- **Evidence:** `nearbyint_f64` is 16% of the 2.0 s serial union in the
  200-line text case. It is a branchy software round-half-even
  (`clipper2-rust-1.2.0/src/core.rs:1603-1618`).
- **Change:** `x.round_ties_even()` (a single `frintn` on arm64). It is
  identical for all finite inputs; the old code turns ±∞ into NaN, which
  never occurs for Clipper's int64-range coordinates. This needs
  clipper2-rust vendored and patched like manifold-rust, or an upstream
  release.
- **Gain:** about 15% of large 2D unions: 2D text about −0.3 s. Also
  `offset()` and 2D booleans in general.
- **Risk:** low, but check it with the 2D tier 3 cases and SVG
  byte-comparisons. Vendoring is an **owner decision**.
- **Effort:** S. **Group H:** a new `vendor/clipper2-rust`, and the root
  `Cargo.toml` `[patch]`. That is shared with group A through `Cargo.lock`,
  so merge one after the other.

### O11. Compute duplicate parallel subtrees only once

- **Evidence:** concept C uses 0.7 s more CPU in parallel than serially
  (2.3), and `from_polyset` runs for each of the 36 identical `channel()`
  children. Wall time gained: about 0.1 s.
- **Change:** an in-flight map from key to a shared once-cell in
  `crates/geom/src/evaluate.rs`. Later siblings wait for the first. Only
  the first "prints" messages, which the replay already models.
- **Gain:** small on wall time, more on CPU and memory for patterned
  models (arrays of identical parts).
- **Risk:** medium. Message replay order and ID blocks must stay
  deterministic, so add a determinism test.
- **Effort:** M. **Group I:** `crates/geom/src/evaluate.rs`.

### O12. Cold start without the GPU frameworks

- **Evidence:** R2, +1.3 ms per process: 30% of a trivial run, 1.3 s over
  `eval_only`'s 976 processes, and 3% of a cold BOSL2 edit.
- **Change:** load the renderer only when a PNG or snapshot is requested:
  a separate helper binary, or `dlopen` of a `render` dylib. Or accept it,
  because agents that care use `serve`.
- **Gain:** 1.3 ms a process. **Risk:** packaging (the release tarball,
  signing). **Effort:** M. **Group J:** `crates/cli`, `scripts/release`
  (conflicts with group A on `crates/cli`). Low priority.

### O13 and later: structural, and exotic for now

- **Lazy booleans (Menger n=4, deep unions):** unchanged since 5b. The
  final 1.2 s of `simplify_topology` is serial in C++ Manifold as well.
  L.
- **Preview of big differences (125 spheres):** neoscad computes a real
  boolean where OpenCSG composites in image space. A GPU image-space CSG
  path (Goldfeather) would match OpenCSG. L. This is a product decision:
  the real boolean is what makes the preview exact.
- **A parallel evaluator:** OpenSCAD's language is sequential, but
  sibling module instantiations with no `$`-writes and no echo could run
  in parallel with ordered message replay. L, high risk. It would only
  pay off on models like the hero, where one statement dominates.

### Which to run together (3 builders)

| Wave | Builder 1 | Builder 2 | Builder 3 |
|---|---|---|---|
| 1 | O1 (group A) | O5 (B) | O6 (C) |
| 2 | O7, then O8 (D) | O2 (E) | O9 (G) |
| 3 | O4 (D, after O7/O8) | O10 (H, after A merges `Cargo.lock`) | O11 (I) |
| 4 | O3 (F: after O2 and O4 settle `ast.rs` and `inst.rs`) | | |

Estimated combined effect for a served BOSL2 edit of the bench part:
33 ms, then about 30 ms (O1), about 16 ms (O2), about 14 ms (O4/O7). For
the hero one-shot: 3.7 s, then about 3.4 s (O1), about 3.0 s (O5, O6),
about 2.5 s (O4, O7). For the hero in `serve` after a non-plinth edit,
O3 takes 2.9 s to about 0.3 s.

## Checked and found fine

- **8f and the GPU gate (`7b338cb`, `3730c3b`)** have no measurable cost
  on the CLI. `7b338cb` and HEAD are within 1% on isosurface and screws.
  The edit-loop snapshot numbers match 7a (45.5 against 44 ms served,
  73.4 against 74 ms cold).
- **Output is deterministic under every experiment.** mimalloc, the bits
  key and the STL buffer change all produced byte-identical STL for the
  hero and concept C.
- **Mesh flags in the bench** are only against 2021.01
  (`extrude_twist`, `text_30lines`), as in the earlier run.
- **BOSL2's top-level constants are not an edit-loop cost.** An
  include-only file evaluates in effectively zero time after parsing
  (`.ast` 26.8 ms, `.echo` 25.8 ms).
- **`import_stl`** improved by 5.7% (0.175 to 0.165 s).
- **OFF export** is now 3× faster than the nightly on 1M triangles, so
  the `fmt_g` followup's premise no longer holds.
- **Triangulation with thousands of holes** is at parity with the
  nightly (1.35 against 1.42 s). The circle-grid followup could be
  closed or re-stated once its original model is found.
- **Geometry parallelism** reaches 7–11 cores whenever siblings exist
  (hero, Menger, 125 spheres). The serial stretches are single big
  booleans, which C++ Manifold also runs serially in `simplify_topology`.
- **Evaluation within BOSL2** is uniformly about 1.6–2× faster than the
  nightly (`move`, `apply`, `path_length`, `default`, the isosurface
  steps). No BOSL2 function was found where neoscad is slower, other
  than the `concat`/`each` accumulators.

## Decisions for the owner

1. **A C allocator (mimalloc) in a pure-Rust stack** (O1). It is the
   biggest cheap win, and may also replace the app's `MallocLargeCache=0`
   workaround.
2. **Vendoring clipper2-rust** for a one-line fix (O10), or waiting for
   upstream.
3. **`panic = "unwind"`** no longer costs anything measurable, so the
   "separate abort profile" idea in `docs/followups.md:360-363` can be
   dropped. Update `docs/architecture.md:164-167` and the followup text:
   the 5–8% is now the limits' hot path (O7), and it is paid under
   `abort` too.
4. **Evaluation reuse across edits (O3)** goes beyond anything OpenSCAD
   does. It is invisible in output but adds a class of cache-invalidation
   bugs. It is the only change that makes heavy BOSL2 models interactive
   for agents.

## Could not verify

- **The icon builder's 4.0 s to 2.8 s.** The per-vertex version was
  never committed. A reconstruction is in 2.5.
- **The circle-grid and OFF followups' original models**
  (`docs/followups.md:16-22`). Their parameters are not recorded. The
  numbers above are for stated substitutes.
- **The app with mimalloc:** idle footprint, memory returned after a big
  render, and whether `MallocLargeCache=0` is still needed. Not run: an
  app build and footprint session was out of scope for a CLI audit.
- **`xctrace` attribution of inlined frames** (`get<>`, `as_slice<>`,
  `position<>`) is by the inlined function's name, so the category
  shares in 2.4 are approximate (±5 points).
- **The effect on WASM** of any item. Not built. O1 and O12 are
  native-only by design.

## Reproduction

All in the scratchpad (it is not kept):

- `ab.py RUNS MODELS BIN...` runs binaries interleaved, with the bench's
  environment.
- `xrec.sh` and `xprof.py` record and parse `xctrace` profiles.
- `serve_drive.py BIN N` drives `neoscad serve` over stdio with
  numbered edits.
- The `micro/`, `iso/`, `gaps/` and `h_*.scad` models.

The bench itself: `./target/release/conformance bench` (about 40 minutes)
wrote `progress/bench/20260927T072506Z-df6731d.json`.
