# Follow-ups

Deferred items found along the way, with where they came from (a phase
such as 5b or 8f, a hardening step H1–H4, or an audit finding such as
O4). Remove an entry when it is done. Sections, and the entries that
lead them, come roughly in order of user impact.

## Serve and session
- Statement reuse across edits (O3, `crates/eval/src/memo.rs`) keys each
  top-level statement on the names it mentions, followed through
  top-level definitions by name alone. A local binder that shares a
  top-level variable's name (BOSL2's `mod`, `base`, `r` parameters) makes
  that variable an input of every statement reaching the code, so editing
  it re-evaluates more than it must. The resolver (`eval::resolve`) knows
  which references a lexical binder captures; using it would narrow the
  key. Two more limits of the first version: top-level assignments always
  run again (in the hero, its `planetary_gears()` call is part of the
  32 ms a carrier edit still evaluates), and a statement is the unit of
  reuse, so an edit inside the hero's plinth statement still costs the
  whole isosurface.
- The statement-reuse harness's opt-in full-corpus shard `1/4`
  (`crates/eval/tests/incremental.rs`) exceeds its default 4096 MB RSS
  cap (peaks vary by run: 4.2–4.9 GB around `vnf__015`/`v6.scad`) and
  passes at 8192 MB. Raise the default for full-corpus runs, or reset the
  session per file.
- A one-line edit re-parses the main file. Since `9dbb98b` an include
  between top-level statements of a file that parses on its own
  (`include <BOSL2/std.scad>`) is parsed and lowered once and spliced
  into each new program (`lang::fragment`, the session's
  `FragmentStore`); with the evaluator work since, the served BOSL2
  edit (`edit_loop`) takes about 18 ms, from 34.
  What is still redone on every edit: the main file's own parse; the
  splice, which copies each fragment's tokens and syntax tree into the
  new program (`Cst::splice`) and renumbers a copy of its AST, rather
  than sharing them; every include that is not a whole top-level unit
  (inside a module body or an expression, mid-statement, after a syntax
  error, or of a file with errors of its own), which is spliced as
  tokens and parsed again, as before; and whole-program evaluation,
  which is now most of an edit.
  One-shot command-line runs parse everything, as there is nothing to
  reuse. An incremental parser for the main file is not started. (7a,
  `9dbb98b`)
- The command line's own export path (`crates/cli/src/run.rs`) does not
  go through `session::Session`; the session re-implements its steps for
  served exports and shares only the encoder (`session::export`). That
  served and direct runs agree is checked by
  `crates/cli/tests/serve.rs` (files and stderr for 3D and 2D formats,
  warnings, errors, `--format json` and snapshots), not guaranteed by
  construction (PNG exports: the server renders on the session and draws
  with the command line's `png` module). Moving `run.rs` onto the
  session would also bring echo, AST, CSG and param exports, `--animate`
  and `--hardwarnings` to the server, which run in-process today. (7a)
- A served run's render summary reports the server's `Geometries in
  cache` count and times, which differ from a fresh process's. (7a)
- Cancellation (and the time limit) stops the evaluator at the next
  call or loop iteration, the geometry evaluator before the next node,
  and primitives and extrusions at their next ring or slice; one long
  kernel operation (a big boolean, a hull, minkowski) runs to its end.
  `neoscad mcp` exits 2 s after the end of input whatever is running.
  (7a, H4)
- The memory limit is an estimate kept at the allocation-heavy points
  (`eval::limits`), not a measurement: every evaluator value (each
  list, string, range and function literal, charged when made since
  `88de26e`), nodes and messages, and the geometry results a render holds until
  their parents use them. A kernel operation's own working memory (a
  boolean's intermediate meshes), the geometry cache (its own budget),
  the check's rays and the snapshot's drawing are not counted; the
  triangle limit bounds their inputs. Geometry results are weighted 6x
  their cache cost for the kernel's working copies, calibrated on the
  benchmark models; still, BOSL2's fractal_tree peaks at 1.96 GB real
  against under 512 MiB estimated (its evaluation alone is 451 MB).
  **Measured where the host can (done for the web core):** a host may
  hand the session a `MemoryProbe` (`session::Config::memory_probe`,
  `Guard::with_probe`), checked against the memory limit wherever the
  guard reads the clock (each geometry node and primitive ring, the
  evaluator's limit ticks); a trip says "(measured)". The web core's
  probe is its counting global allocator's peak since the request began
  (`crates/web/src/heap.rs`); without a probe nothing changes.
  **Natively** (`crates/cli/src/memory.rs`, compiled into `cli` and
  `ffi`): the process's footprint on macOS (`ri_phys_footprint`), its
  resident set on Linux (`/proc/self/statm`), its private bytes on
  Windows, for `serve`, `mcp`, the one-shot `--limit memory=` and the
  app. The session reads the probe at most every 10 ms, and before a
  reading over the limit fails a request it evicts half of every
  renderer's geometry cache, has mimalloc return freed pages
  (`mi_collect`) and measures again, until under the limit or eviction
  stops helping (`session::memory`). What is left:
  - `linux-app/src/host.rs` does not set the probe yet (one line and
    the module by path, as `ffi` does; not built here, which has no
    GTK).
  - Relief is reactive and covers the geometry caches only: parses,
    statement memos, products and fonts are not trimmed, and nothing
    listens for the system's memory-pressure notices (the app could
    call `clear_caches` on one). Each session relieves its own caches;
    no host runs two limited sessions in one process today.
  - The trip goes to whichever request checked, not the one that grew,
    and its message says "the engine uses N MiB" for the whole process.
  - Linux's resident set leaves out swap (`smaps_rollup` has it, at the
    cost of a walk of every mapping per read).
  (H4)
- `eval::limits`'s `fmt_num` prints a fractional number as a broken
  exponent: a memory limit of 3.05 MiB reads "3.053e MiB", and 2.5
  "2.500e" (`{:.3e}` gives "3.053e0", and the trailing zeros trimmed
  are the exponent's). Seen with a byte-sized limit in a test.
- When several parallel geometry siblings pass a count limit, the
  earliest in the source that recorded one is reported; a sibling that
  stopped (on the others' trip) before its own check never records, so
  which one is named can vary between runs. One over-limit node is
  reported the same every time (`crates/session/tests/session.rs`). The
  time and memory limits depend on timing by nature. (H4)
- The one-shot command line applies `--limit` to evaluation and mesh
  and PNG geometry; `neoscad check`, `measure`, `snapshot` and `test`
  have no `--limit` (they are unlimited, as the command line is). (H4)
- The session keeps up to four renderers (one per colour scheme and
  font set in use, since geometry keys include neither), each with its
  own geometry budget, so the worst case is four budgets. (7a)
- Unix sockets only; a Windows server would need a named pipe. The
  default socket path must fit `SUN_LEN` (104 bytes on macOS); a long
  `$TMPDIR` or `$XDG_RUNTIME_DIR` would need `NEOSCAD_SOCKET`. (7a)
- `progress` notifications report stages, not fractions of the work.
  (7a)
- Fix hints are a table by code plus "did you mean" over the program's
  and OpenSCAD's builtin names (`session::diag`, the builtin list copied
  from the reference's `Builtins::init` registrations). Scoped names
  (a module's parameters and local variables) are not candidates. (7a)
- The snapshot headlight's direction, ambient and diffuse terms were
  chosen by eye on a bracket (`render::Lighting::Headlight`); no test
  pins its images. (7a)
- Cached geometry replays its messages in a warm render only when the
  pattern of first occurrences below it is the same as when it was
  computed; otherwise the node is computed again from its children's
  cached results (`geom::RenderOptions::replay`). Correct, but a cached
  subtree whose earlier twin was edited away is recomputed once. (7a)
- A geometry cache hit is used only if the fragment, slice and triangle
  counts its subtree asked for are within the request's limits
  (`geom::evaluate`'s `Demand`), and a document's last product is reused
  only under the same limits; otherwise the node is computed again and
  refused where a cold render refuses it, so lowering the limits cannot
  let a warm cache pass a model a cold one stops. Results computed
  without limits have no recorded demand and are computed again once
  under limits. Memory and time are not re-checked on a hit (it
  allocates and takes nothing). (8f)

- **Preview product cache and wasm memory.** Since preview products are
  cached in the renderer's geometry cache, a long /try session's wasm
  memory peak is higher: threaded-ring then gearbox, eight previews in
  one worker, peaked at 719-763 MiB against 505 MiB before, with the live
  heap only 9-30 MiB higher (wasm memory never shrinks, so this looks
  like fragmentation). The web limit is 1 GiB. If users hit it, give the
  web core a smaller geometry budget or a separate product budget, or
  restart the worker between documents.
- **The web page's "Previewed in" leaves out the GPU upload.** The
  native apps count it in the total (`crates/ffi/src/document.rs`,
  `crates/linux-app/src/run.rs`); the page shows the core's summary as
  it comes from the worker (`web/src/app.js`, `runOnce`), before
  `viewer.setScene` uploads the packed scene (`crates/web-view`,
  `upload_packed`), so neither that nor the transfer from the worker is
  in it.

- **`neoscad` isn't bit-reproducible.** Two builds of the same commit into
  the same target path differ in 118 bytes: the Mach-O UUID and ad-hoc
  signature, rustc's random `rustcXXXX` temp-dir names in the debug map,
  and mimalloc's C code compiling in `__DATE__`/`__TIME__` (its "built on"
  string). Output is unaffected (conformance, bench STL hashes). For
  reproducible releases: set `SOURCE_DATE_EPOCH` (clang honours it for
  `__DATE__`/`__TIME__`), and check the debug map and signing. Found by
  the sccache spike (2026-10-02), which also found sccache doesn't help a
  new worktree (proc-macro install names, `OUT_DIR` and the checkout path
  are in its keys), so it isn't wired in.

## Performance

- **A stack-independent evaluator (owner decision, 2026-10-01: do it
  after the 0.2.1 work).** Module and function evaluation on an explicit
  heap stack, as the render walk is since 6c50a57, or further toward
  the bytecode VM in `docs/audits/bytecode-vm.md` §6. Why:
  - no recursion-depth limit in any browser (WebKit's large JSC frames
    are why /try needs a probed budget) or natively;
  - interruptible evaluation;
  - PGO without the depth trade-off, so it could also go on the DMG's
    core and Windows arm64.

  The baseline, as of 34d69e2:
  - native module depth is 65,507 (2.16× OpenSCAD's) at 64 MiB;
  - WebKit stops m() through translate at about 30 levels, plain modules
    at 80, functions at 60, comprehensions at 20; Chromium and Firefox at
    150-300.

  The audit measured the explicit stack alone at 0.95-1.08×, so
  "no regression" is the bar. Benchmark every stage as a matrix:
  - {current, heap evaluator} × {plain release (thin LTO), PGO
    (retrained per evaluator)}, plus fat LTO if cheap;
  - the macOS app core (neoscad-ffi) with PGO included;
  - end-to-end edit-to-preview latency through client's DocumentController
    and through the web worker, with the mesh copies timed separately;
  - the serve edit_loop;
  - interleaved runs on a quiet machine.

  Conformance and BOSL2 output must stay byte-identical.

  Design and staged plan: `docs/audits/heap-evaluator.md`. Stage 0 (the
  walks over the finished node tree made iterative) is done; its numbers
  are in that file's "Stage 0 done". Stage 1 (statements and module
  instantiation on a heap stack, behind the `heap-eval` cargo feature,
  with the counted `--limit depth=N`) is done; see "Stage 1 done" there.
  Stage 2 (function calls, comprehensions and `let`/`assert`/`echo` on
  the heap past 8 native call levels, with the counted limit covering
  functions) is done; see "Stage 2 done". **The heap evaluator is the
  only evaluator** (owner decision, 2026-10-01): the recursive statement
  driver and the `heap-eval` feature are gone from every crate, and
  `conformance depth` now checks that every build, PGO included, reports
  the counted limit's depths; see "Feature removed" in the audit. (The
  web core was held back once because WebKit's first preview came about
  5 s late with the feature: the core's `heapStatements()` read the web
  crate's own feature, which was off while the evaluator's was on, so
  all eight stack probes ran against a heap evaluator to the depth
  limit, about 7 s in a WebKit worker. The probe, its frame weights and
  `heapStatements()` are gone now.)
  Left from stage 2 (updated 2026-10-06; numbers and method in
  `docs/audits/heap-evaluator.md`, "Native shapes and value depth"):
  - done: every shape that stayed native but one runs on the heap.
    Ranges' bounds, `is_undef()`'s argument, callees that are
    expressions and methods' arguments moved first; C-style `for`
    comprehensions (every part), `object()`'s arguments and parameter
    defaults that may call followed (`heap_expr`'s module docs). A
    recursion through any of them reaches the counted limit in every
    build and engine (`wasm-check.sh --depths`: `function-cfor` and
    `function-default` 99,999 in node), where it stopped at 30-37 levels
    in browsers;
  - left: `use`d libraries' assignments, which `library_context`
    evaluates during a function lookup. A recursion through them needs
    two libraries that `use` each other and a `$` variable to drive it.
    Natively it reaches 20,163 levels (3.3 KiB a level; OpenSCAD's
    nightly stops between 3,000 and 6,000), and in a browser the frame
    budget stops it after about 30 (its arithmetic: 64 frames a level;
    not measured in a browser). Moving it means making the lookup
    (`find_function` -> `library_context` -> `init_scope`) resumable;
    not done, as no real program recursing that way is known;
  - done: value depth. Printing, the element-wise operators, `chr()` and
    freeing a chain of closures walk a nested value with a stack of
    their own (comparing, hashing, and freeing lists and objects already
    did). `crates/eval/tests/value_depth.rs` runs 43 such cases, values
    100,000 to 999,999 deep, on a 512 KiB thread, in debug and release.
    Printing still stops with OpenSCAD's "Stack exhausted", at a counted
    depth now (`print.rs`), the same in every build and engine: 46,918
    levels of a nested list (browsers stopped at 250), and `issue4172`
    prints 302 levels (301 before in release, fewer in debug), which
    `conformance depth` now holds a build to;
  - the heap path costs 1.3-1.6 times the native one per call, so a
    deep non-tail recursion is slower past 8 levels than it was
    natively: `fib(25)` 28% more instructions and `1 + f(n - 1)` 36% at
    stage 2. The shapes moved now pay it too past 8 levels: a program
    running a C-style `for` that calls 12 levels deep takes 5.5% more
    instructions, one calling a function whose default calls 8.7%.
    Elsewhere this change is at parity (instructions retired
    against `d8c73e7`): the eval-bound bench models +0.05% to +0.55%,
    BOSL2's tests +0.02%, `fib(25)`, `1 + f(n - 1)` and a top-level
    C-style `for` +0.9-1.0% (layout: edits to `heap_expr` that change
    no behaviour moved the first two between +0.9% and +2%), and
    nested-list arithmetic and printing 9% and 5% fewer;
  - `resolve::Stats` does not count the `may_call` share of a corpus;
  - proposed, not applied: a smaller native stack. Measured, what still
    uses it needs little: every BOSL2 corpus file evaluates on a 64 KiB
    thread in release and 256 KiB in debug (byte-identical output);
    source nested to the parser's limit needs at most 2.7 MiB (debug,
    at its lower limit, 4.3 MiB); only the `use` recursion above grows
    with depth. So `DEFAULT_STACK_LIMIT` could be 16 MiB (that recursion
    then stops near 4,900 levels, inside OpenSCAD's range) and the
    evaluation thread 32 MiB. Two cautions: `DEFAULT_THREAD_STACK` also
    sizes the geometry pool's threads and the CLI thread that runs short
    geometry chains, where Clipper2's polytree recursion needs over
    2 MiB for 12,000 nested rings, so geometry should get a constant of
    its own first; and the size is reserved address space, touched only
    as used, so the gain natively is small.

  Left from stage 1:
  - Done: the apps' `ResourceLimits` record (`crates/client/src/types.rs`,
    declared to UniFFI in `crates/ffi/src/types.rs`) carries `depth`
    (`None` the default, 0 refused), so the Swift and C# bindings have
    it (both are generated at build time; there is no Kotlin binding),
    and the web protocol's `ResourceLimits` takes it too
    (`docs/web-protocol.md`). Nothing in the apps sets it yet.
    `web/src/engine/mock-core.js`'s limits object does not list it.
  - Done: the heap frames are charged to the memory estimate
    (`Evaluator::held_bytes`, read at the periodic limit check, O(1)):
    contexts in use, the statement and expression frames and their side
    stacks. The estimate's smallest passing `--limit memory` at 90,000
    levels (release, no host probe) went from 87 MiB (nodes only) to
    191 MiB for a module recursion through `if` (205 MB peak resident),
    from 1 to 24 MiB for a function (38 MB) and from 1 to 54 MiB for a
    function through a comprehension (92 MB); `crates/eval/tests/
    memory_limit.rs`, `deep_recursions_count_their_frames`. The native
    hosts' probe (`crates/cli/src/memory.rs`) measured them all along.
  - Closed: the evaluator's start, `Unit::add_scope`
    (`crates/eval/src/eval.rs`), recurses on source nesting, like the
    parser and the lowering, and the parser's nesting limit
    (`lang::syntax::parser::NESTING_LIMIT`: then 5,000 nodes natively,
    320 on wasm32; since October 2026 a depth weighted by kind, which
    allows about 2,380 levels of `translate()` natively and 119 on wasm32)
    bounds all three. At the limit it fits with room: natively,
    `Unit::new` on 4,990 levels of `translate()` overflowed a 1 MiB
    thread and fit in 2 MiB (release), against the evaluator's 80 MiB
    (`crates/eval/tests/deep_source.rs` evaluates every kind of nesting at
    the limit on it); in node 18, `scripts/wasm-check.sh --depths` parses
    and evaluates seven kinds of source nesting up to the limit (315
    levels of `translate()` evaluate, 316 end in "Parser error: memory
    exhausted") with no trap. By the native
    measure `add_scope` would take about 10 MiB at 26,000 levels, so what
    overflowed 80 MiB there was the evaluation, not `add_scope`. In
    WebKit the parser overflowed first (about 200 levels; the WebKit
    entry under the web core has the weighted limit sized for it since),
    so `add_scope` is not what any host's
    limit is sized by. Iterative, it would only matter together with an
    iterative parser and lowering;
  - which `--trace-usermodule-parameters` lines print `...` near the
    recursion limit, and how much of a nested value a trace prints deep
    in a module recursion, now follow the counted depth
    (`print.rs`, `MODULE_LEVEL_STACK`) rather than the native stack.
- **The geometry pool's stack stays at the evaluator's 80 MiB**
  (`geom/src/evaluate.rs`, `pool()`), measured: the walk needs little
  (with 128 KiB a thread the conformance suite passes and the bench
  models' STL is unchanged; 64 KiB crashed BOSL2's `fractal_tree` and the
  `module_recursion` tests), but Clipper2 polytrees are built and read
  recursively per level of polygon nesting (`clipper::from_tree`,
  Clipper2's `recursive_check_owners` and `poly_tree_to_paths64`'s
  helpers), so a union of 3,000 / 6,000 / 12,000 concentric rings needed
  over 512 KiB / 1 MiB / 2 MiB. Making those walks iterative (ours, and
  a vendor patch for Clipper2's) is what would let the pool shrink to
  about 1 MiB; the size is only reserved address space today. Not
  measured: a chain is walked on the calling thread, and its kernels'
  data-parallel loops run on rayon's global pool (`is_chain`), whose
  threads have rayon's default stack size.
- **Call reuse (`eval::callmemo`) gains little from calls with
  children.** They are keyed now (the children's scope, and the variables
  their mentioned names reach in every context around them), and the
  BOSL2 corpus replays 4.5% more calls (45,321 against 43,373), but
  nothing measurable got faster: the bench's BOSL2 models spend their
  evaluation in functions (`gears`, `isosurface`, `spring_handle` make
  under 30 module lookups) or already replay at the recursive module
  (`fractal_tree`), and the corpus's examples are small. What limits it:
  a call with children is recorded only at its third call and never
  inside another recording, since its nodes include its children's (each
  wrapper of a `recolor() cylinder() attach()` chain copied the same
  subtree, and fractal_tree's evaluation took 17% longer before both
  rules); a traced sample of 600 corpus files had 9.4k lookups whose key
  repeated against 24k whose call site and frame did, the rest differing
  in a variable the children mention or in a `$` value; and the
  per-definition give-up rule (`DefStats::disabled`) counts calls with
  and without children together. Also not covered: modules
  defined inside other modules (their definition context is a module
  frame), and `$` names read by the `$v = $v * e` pattern anywhere but
  a statement assignment (a `let` or a named argument `f($v = $v * m)`
  keys on the value). The memo lives for one evaluation; carrying
  entries across edits would need `Memo`'s positional anchors. A replay
  is refused at more native stack or frames than its recording had (a
  call recorded at the top level never replays inside a `let` or
  `for`); recording the peak at each recursion check instead cost 5x on
  fractal_tree's evaluation, so a cheaper hook would be needed. Calls
  whose arguments hold more than 4,096 values are never keyed.
- `PolySet::tessellate` with the libtess2 port, against the ear clipper
  it replaced (ff714c3), in-process with mimalloc as the binaries link
  it: 600k flat quads 0.020 s against 0.038, 600k non-planar quads 0.031
  against 0.038, 60k 8-64-gons 0.028 against 0.029, 60k stars 0.63 s
  against 0.80, a `$fn=2048` sphere 0.118 against 0.134. Whole models are
  at parity: median of 21-41 interleaved runs within ±1%, except
  `csg_spheres` (+1.2% wall with 0.7% fewer instructions; its profile is
  Manifold's booleans, now on OpenSCAD's triangles, and the tessellator is
  not among its top functions).
  Instructions are lower for quads but higher for n-gons (+9%) and stars
  (+48%). The margin on n-gons is thin: caps flat on an axis take
  `Convex::tessellate_axis_aligned`, rotated ones two more passes, and
  non-convex ones the full sweep, whose O(n^2) `make_face` walks are
  upstream's. (libtess2 port)
- **`panic = "unwind"` costs 5–9% on evaluation-bound models** after D1
  (`docs/audits/unwind.md`; D2's `extern "C"` drop shims measured 3–7%
  slower and were reverted). Winning it back for the one-shot CLI means
  an `abort` build shipped beside the unwinding one, since `serve`,
  `mcp` and `lsp` are subcommands of the same binary; a product
  decision. Related: timings move ±2–5% with the layout of `Evaluator`,
  more than many micro-optimisations. Group its hot scalar fields
  (`frames`, `pending`, stack bounds, `limit_ticks`, `hard`/`limit`) in
  one 64-byte-aligned block, and replace the `placeholder` context
  (`eval_call` clones it to reserve a frame slot, `call.rs:480`); removing
  it measured about −2.5% instructions on fib and −1.4% on isosurface.
- The level-4 Menger sponge is no longer slower than the nightly: the
  parallel boolean patch in `vendor/manifold-rust` (`vendor/README.md`)
  took it from 2.62 s to 1.59 s against the nightly's 2.29 s, byte for
  byte the same output. (The earlier diagnosis here, that the nightly's
  lazy operators flatten nested unions, was wrong:
  `docs/audits/slow-cases.md` §1.) What is still serial in the last big
  difference, and what taking it would need, is the plan at the end of
  that section: the edge collapses themselves (serial in C++ too), the
  per-face ear clipping's CPU cost, and `intersect12`'s per-edge result
  vectors. Deep unions were not slower when re-measured (§3).
- The parallel boolean patch costs CPU (hero +34%, `csg_spheres` +70%,
  `csg_deep_union` 0.08 to 0.19 s) and, with every core busy with other
  work, made the hero and `csg_deep_union` slower than before (4.6
  against 3.5 s; 0.118 against 0.061 s), not faster. (`slow-cases.md`
  §1.1) **Partly done:** `batch_boolean` rounds now go parallel only
  above 10,000 vertices (`vendor/patches/manifold-rust/0006-*`, output
  unchanged, a determinism test in `crates/geom/tests/parallel_kernels.rs`).
  Measured interleaved against the build before it, exporting STL (best
  of 5 to 11, identical bytes every time): unloaded, all four models
  within noise (`csg_deep_union` 0.076/0.082 s, hero 2.05/2.07 s, Menger
  4 1.63/1.63 s); with one busy loop per core (load about 25-40), mixed
  and within noise; with five per core (load to 150), `csg_spheres` 3.70
  to 2.76 s, hero 11.5 to 10.5 s, `csg_deep_union` unchanged (0.138/0.143
  s). The original 4.6-against-3.5 s regression was not reproduced (that
  run had load 80-120 from one many-threaded process), so the pairs were
  at most part of it. With manifold-rust 0.16.0 (upstream has the parallel
  rounds without a threshold) the threshold was dropped: re-applied to
  0.16.0 it measured no better, unloaded or with five busy loops per core
  (`vendor/README.md`, "Moving from 0.15.0 to 0.16.0"). Left: a profile
  under that kind of load, and rayon's spin-waiting (the rounds, and
  `geom`'s own `rayon::join`s, have no size threshold).
- The ear clipper's bridge searches still scan every outer ring's box
  once per hole (the ring boxes, upstream since manifold-rust 0.16.0,
  `vendor/README.md`, only skip the walk), and `find_closer_bridge`'s wedge test admits rings up and to the
  right of the hole. On 200 lines of extruded text, now 2.7 s against the
  nightly's 32.3 s (`docs/audits/slow-cases.md` §2.1), that search is
  still about 1 s. An index of the boxes sorted by y, and a tighter bound
  on the wedge, would cut most of it. Low priority: the nightly is 12×
  slower here.
- The banded 2D union (`union_by_bands`, `crates/geom/src/clipper.rs`)
  only splits children whose y-ranges are separate. Children separate in
  x but sharing y (a row of shapes, one line of text's glyphs) still run
  one serial union, because their output records interleave by y in
  Clipper's sweep and byte parity with OpenSCAD's order would be lost.
  Accepting a canonical, different order there (an owner decision; SVG
  and DXF parity tests compare order) would let any disjoint clusters run
  in parallel. It also falls back to the full union, having done the
  bands' work, when a band other than the lowest splits a record after
  its sweep; no model tried so far does, but such a model pays about
  twice. (slow-cases §2)
- Resolved variable lookups (O4, `crates/eval/src/resolve.rs`) still walk
  the context chain, comparing each context's region with the reference's
  candidates, rather than hopping a fixed (depth, slot). Fixed addressing
  needs a static frame layout, and four things get in the way: a scope
  assignment not yet made falls through to an outer binding, a builtin
  that binds (`intersection_for`) can be redefined by a user module, a
  C-style `for` has two iteration contexts in the chain while it
  increments, and function literals capture whatever chain they were made
  in. In the hero, 17.5M of 25M single-candidate lookups stop at the first
  context, but 2.2M walk 7 or 8. (O4)
- Each evaluation that resolves a function body or literal scans every
  expression of every unit once for named-argument names
  (`resolve::named_arguments`, about 0.25-0.4 ms over BOSL2), because a
  named argument that is not a parameter binds in any callee. Lowering
  could record the names with the program (and its fragments), so the
  session's edit loop would not rescan them. (O4)
- Keeping up to three slots inline in each context, to save the slot
  vector's allocation per call, `let` and loop iteration, measured 2-4%
  slower on the BOSL2 models: every context grows. Worth retrying with a
  smaller `Value` or a slab of contexts. (O4)
- CLI cold start is 2.9-3.0 ms (`f89bf0e`, `docs/audits/unwind.md`).
  What is left to take: the delay-init GPU frameworks are still mapped
  and bound (about 0.3 ms, measured on a C program linking the same
  ones), which only a `dlopen`ed renderer or a helper binary would save
  (O12); mimalloc's start-up, about 0.1-0.2 ms; and the rayon pool, which
  a render that is not a small chain still starts. (O1, R2)
- Identical parallel siblings are each computed (performance audit
  O11): an in-flight map from key to a shared result would compute them
  once. Needs a determinism test for message replay and ID blocks.
- Geometry keys are still recomputed for the whole tree on every render,
  now in parallel (`perf-opportunities.md` P4(a)): about 20 ms for
  `fractal_tree`'s 290k nodes, whose single-child spine stays serial.
  Carrying each memo entry's per-node hashes and shifting them by the
  replay's index offset (P4(b)) would skip replayed subtrees entirely.
- **PGO in releases.** The cargo-dist release builds `neoscad` with PGO
  on macOS arm64, Linux x86_64 and aarch64 and Windows x86_64
  (`docs/release.md`, "PGO builds"; the v0.2.0 release ran it, and `pgo.yml`
  passed on all four after the bench fixes). `dist build` packs the
  step's optimised binary rather than rebuilding it: in v0.4.2's run
  the SHA-256 each of the four PGO targets logs equals the one
  `neoscad-executables.sha256sums` lists. The macOS DMG's core and CLI
  are PGO builds too (`release.sh --pgo`; docs/release.md, "PGO
  builds"): the core trains through its own `pgo_train` example, since
  the CLI's profile does not match the core's symbols, and the x86_64
  slices train under Rosetta. Left plain:
  1. `aarch64-pc-windows-msvc` ships a plain build: its instrumented
     binary crashed on every training run (`0xC0000005`) and
     `llvm-profdata` rejected the raw profile ("symbol name is empty"),
     the error rust-lang/rust#150123 reports. Retry when that issue
     moves, by putting the target back in `pgo.yml`'s matrix and then in
     `build-setup.yml`'s list.
  2. `x86_64-apple-darwin`'s cargo-dist archive ships a plain build: it
     is cross-built on the arm64 `macos-15` runner. The DMG now trains
     its x86_64 slices under Rosetta on `macos-26` with `pgo.sh
     --target x86_64-apple-darwin`; the same would work in
     `build-setup.yml` if `macos-15` has Rosetta (not checked), and the
     host check there (`$host = $target`) would have to allow it.
  The release step runs the recursion-depth guard (`conformance depth
  --binary PATH`) on the binary it ships, and `release.sh` runs it on
  both slices of the DMG's CLI.
- The web core gained nothing from `simd128` autovectorisation
  (`perf-opportunities.md` P7, within 2% on six kernel-bound models,
  identical output). A kernel gain there needs hand-written `v128` code;
  relaxed SIMD would give up bit-identical results.
- **Fixed: nested list literals took memory with the square of their
  depth to evaluate.** `x = [[[...1...]]];` peaked at 33 MB at 1,000
  levels, 495 MB at 4,000 and 1.1 GB at 6,000 (`neoscad -o x.echo`,
  release), and `--limit memory=1024` did not stop it. The cause was the
  evaluator's start building every list literal's constant value
  (`Unit::consts`, kept so hot loops do not rebuild literals) afresh,
  copying each nested level once per level above it. `const_values`
  now builds each once, sharing the inner lists, with a stack of its own:
  4,000 levels peak at 9 MB and 4,900 (the parser's limit) at 10 MB,
  the output unchanged.
- **Parallel renders of many-child unions peak higher than one thread's
  (BOSL2 `skin__042`), but not from holding more meshes.** The example
  sweeps a region and calls `show_anchors()`, which attaches an arrow and
  a text label to each anchor: 628 `text()` extrusions, 342 polyhedra and
  682 cylinders under one union. It is the render that grows, in the
  geometry: evaluation alone (`-o x.csg`) peaks at 237 MB and the preview
  PNG at 232 MB, without `show_anchors()` the sweep renders in 0.1 s,
  and `-o x.stl` passes 2 GB in both neoscad and the nightly
  (`--backend=manifold`, killed at 9.8 s), so the example itself needs
  more than 2 GB. A memory limit stops it cleanly (`--limit memory=1536`
  reports "the engine uses 1,552 MiB of memory, over the memory limit of
  1,536 MiB (measured)"), so this is about peak, not safety. Scaled
  down to `rgn1`'s first two circles (`d=[10:10:20]`, an 855k-facet
  result), the nightly finishes at 1.74 GB in 7.5 s. Neoscad, run
  without `--limit` (2026-10-06, load 4-9 from other processes,
  interleaved runs), takes 9.9 s on one thread and peaks at 1.62-1.69 GB
  of physical footprint (1.77-1.84 GB peak RSS), on two 6.2 s and
  1.73 GB, on four 4.1 s and 1.85-2.04 GB, and on the default 14
  2.7-2.9 s and 2.01-2.12 GB (2.15-2.27 GB RSS). The earlier reading, that each union level's
  branches (`kids_in_parallel`, `geom::shared`'s `level.par_iter()`) hold
  their intermediate meshes at once, does not hold here: a counting
  global allocator puts the peak of live heap at 1,258 MB on one thread,
  1,266 MB on four and 1,265 MB on fourteen (1,536 and 1,544 MB with a
  third circle), so a bound on how many unions run side by side has no
  live memory to save. The extra footprint is the allocator's: freed
  memory that mimalloc keeps in each worker's pages, and its purges
  waiting out `purge_delay` (1 s by default in mimalloc 3). Walking the
  tree serially while the kernels stay parallel shows what a bound could
  win: 270 MB less footprint for 60% more time (4.5 s against 2.8 s).
  `MIMALLOC_PURGE_DELAY=0` or `10` cuts the default-thread footprint to
  1.70-1.84 GB for about 10% more time on this model (peak RSS is
  unchanged, since macOS counts purged pages there until it reclaims
  them); whether to set it in `cli`'s and `ffi`'s allocator needs a
  benchmark run. The live peak itself, about ten times the result's
  cache estimate (116 MB), is the thing to cut for every thread count:
  the geometry cache is not it (a 1 MB budget, or not caching results
  over the budget, leaves it at 1,257 MB); a heap snapshot on one thread
  shows mostly deep copies in `transform()` and `to_manifold()` (the
  cache holds the child's `Arc`, so `Arc::unwrap_or_clone` copies) and
  the batch union's working memory.
- **Fixed: a memory limit made parallel renders 2.4 times slower.**
  Every kernel check calls `Guard::stopped` (`kernel_token` in
  `geom/src/manifold_geom.rs`), which reads the memory probe, and the
  probe's throttle (`read` in `session/src/memory.rs`) took one
  process-wide mutex on every call, even to return the cached reading:
  the scaled-down `skin__042` above took 6.7 s with `--limit memory=2900`
  against 2.7 s without (42.7 s of system time, mostly
  `__psynch_mutexwait`). The last reading and its time are now atomics,
  and when the reading is stale one thread probes again while the others
  use the last one. With the limit it now takes 2.8 s (1.5 s system), on
  one thread 10.7 s against 10.8 s; the fractal tree went from 1.86 s to
  0.66 s and `csg_spheres` from 0.89 s to 0.44 s under the same limit,
  with identical output.
- A time or memory limit still costs a render about 4% on fourteen
  threads and 8% on one (the scaled-down `skin__042`: 2.8 s against
  2.7 s, 10.6 s against 9.7 s; a `fragments` limit, which makes a guard
  but reads no clock, costs 1%). It is the clock read in every check:
  `par::maybe_par_map_ct` in `vendor/manifold-rust` polls the token once
  per element (per halfedge in `intersect12`), where C++ polls once per
  parallel chunk and per 1,024 elements (`kSeqCancelChunk`, Manifold's
  `parallel.h`). The port chose per element because its token was one
  relaxed load; NeoSCAD's `with_check` patch made each poll a call to
  `Guard::stopped`, which reads the clock (and, with a memory limit, the
  probe's cached reading). Polling the check once per 1,024 elements in
  that patch, while keeping the flag per element, would remove most of
  it; `kernel_token`'s documentation already says "every few thousand
  items". (2026-10-06)

- A `children()` chain takes memory with the square of its depth: a
  module that passes `children()` down 5,000 levels peaks at 510 MB, and
  a debug build of 99,990 levels passed 86 GB before it was killed. The
  memory limit did not stop it in time (found 2026-10-06).
- Text exports are not under the memory limit: the CSG export of a
  `translate()` tree 100,000 deep grows with the square of the depth
  (each level re-indents its subtree), built about 137 GB of text and
  wrote a 24 GB file. Charging the export's output to the limit, or
  capping the indent, would stop it. (2026-10-06)

## Parity
- `manifold-rust` 0.16.0 ports Manifold v3.5.0 (with a few later
  upstream fixes, and divergences listed in its
  `docs/CPP_DIVERGENCES.md`); OpenSCAD pins v3.5.2.
  (5a)
- `collapse_edge`'s clean-up after a boolean could slide a vertex across
  a crease and fill a concave corner (BOSL2 `cubetruss`, 7.3 mm³ too
  much; `vendor/README.md`). manifold-rust 0.15.0 fixed it (in
  `dedupe_edges`) and neoscad's patch was dropped. C++ Manifold 3.5.2
  fails the same way: report it to Manifold with the 35- and 28-vertex
  operands in `crates/geom/tests/data/collapse-crease-*.txt` (union
  328.29 instead of 314.49). Separately, C++
  3.5.2 built by hand (`-O2 -ffp-contract=off`, no TBB) crashed with
  SIGSEGV intersecting the larger cubetruss operand with some boxes; not
  investigated. (H1)
- Manifold's `MinkowskiSum` (C++ and the Rust port alike) is only right
  when the second operand contains the origin: it always unions the first
  operand, unmoved, into the result (`minkowski.cpp:84`,
  `composedHulls.push_back(a)`; manifold-rust `minkowski.rs`, the same
  statement).
  Repro for an upstream report:
  `Manifold::Cube({1,1,1}).MinkowskiSum(Manifold::Cube({1,1,1}).Translate({2,0,0}))`
  has volume 9; the sum is the cube [2,4]x[0,2]x[0,2], volume 8.
  manifold-rust gives 9 (checked); the C++ result is by reading the
  source, not run. The 5a note of 8.875 for two unit cubes did not
  reproduce (8, correct). neoscad does not use `MinkowskiSum`: OpenSCAD
  builds without `USE_MANIFOLD_MINKOWSKI` (`CMakeLists.txt:43`) and sums
  convex parts with hulls, which `geom::minkowski` ports. (5d)
- 3D `minkowski()` cuts non-convex operands into convex pieces with
  Manifold booleans (along each reflex edge's bisecting plane) rather than
  CGAL's `convex_decomposition_3`, and covers a solid with more than 48
  reflex edges through its boundary instead. The solid is the same, but
  the mesh differs from the nightly's because the pieces do and the union
  keeps vertices on flat faces (e.g. an L of `cube([20,5,5])` and
  `cube([5,20,5])` plus a 32-segment sphere: 828 vertices vs 772; that L
  plus itself: 32 vs 34). Images match; exported bytes do not. (5d)
- The boundary cover in `geom::minkowski` (`Operand::Boundary`, used past
  `MAX_REFLEX`) left a zero-volume sheet in one case:
  `minkowski() { difference() { cube([20,10,4], center = true); hull()
  for (x=[-4,4]) translate([x,0,0]) cylinder(r = 2, h = 6, center = true,
  $fn = 16); } sphere(0.5, $fn = 16); }` with `MAX_REFLEX` forced to 0
  gives the nightly's volume (982.6486) but 2.65 more area (806.0157 vs
  803.3664) and genus 0 instead of 1. With the default it is cut into
  pieces and matches; the same slot at `$fn = 64`, which does take the
  boundary path, matches too. Not looked into.
- 3D `hull()` matches the nightly's vertices and triangle order, but some
  triangles start at a different vertex (e.g. `hull() { cylinder(r=10,
  h=1); translate([0,0,10]) cube(5, center=true); }`: 11 of 190 OFF
  lines differ, all rotations). The QuickHull port's `build_mesh` reads
  the same as C++ `buildMesh`, so the rotation presumably comes from later
  in the kernel, like the boolean rotations above; not traced. (5d)
- QuickHull (Manifold C++ and manifold-rust) sometimes returns a folded,
  non-convex hull; `geom::hull::hull_3d` checks every 3D hull, including
  minkowski's, and rebuilds a folded one (H1). About 1 in 30 rounded-box
  hulls and minkowski sums fold. Where the nightly's own hull folds (e.g.
  `hull() for (x=[0,30], y=[0,30], z=[0,5]) translate([x,y,z])
  sphere(r=3, $fn=16);`, 13453.12 against CGAL's 13453.41) neoscad now
  differs from it, correctly. manifold-rust 0.16.0 decides "above a
  face" exactly (its `docs/CPP_DIVERGENCES.md` entry 11, the fix for one
  cause of the folds), and `minkowski(){cube([30,20,5],center=true);
  sphere(3,$fn=48);}` (9751.29 instead of 9751.42 in C++) no longer needs
  the rebuild: the sum is 3x faster and its volume is the nightly's to
  1e-10. Whether other inputs still fold is not measured; report the
  remaining cases to Manifold, which still has the float test. (H1)
- The nightly prints CGAL's own diagnostics for some minkowski operands
  (Nef assertion failures for cubes touching at an edge or a vertex,
  `minkowski-cubes-touch-*.scad`, `issue1137.scad`); they are not
  reproduced. Messages that come from OpenSCAD itself ("Minkowski
  hard-crashed, falling back to Nef operation.", then the fallback's
  conversion warnings) are. (5d)
- `PolySet::triangulate_faces`, which minkowski() reads its operands
  through (the CGAL-style read, no vertex merging), still splits faces by
  ear clipping; OpenSCAD's `createSurfaceMeshFromPolySet` hands CGAL the
  faces as they are, so there is no libtess2 order to match there.
  `PolySet::tessellate` (export, display, conversion to Manifold) uses
  the libtess2 port (`crates/geom/src/libtess2`). 2D shapes never went
  through libtess2: with `USE_MANIFOLD_TRIANGULATOR` (on by default,
  `CMakeLists.txt:42`) OpenSCAD triangulates them with Manifold's
  `Triangulate` as neoscad does. (5a, 5b)
- STL facet normals differ from the nightly's in the last bits on 42 of
  the 167 3D test models it exports (`tests/data/scad/3D`), with every
  other byte identical: `io::stl` computes `(p1 - p0) x (p2 - p0)` and its
  length unfused, where the arm64 nightly's Eigen code fuses some of the
  multiply-adds. Fusing the three cross-product components
  (`a1.mul_add(b2, -(a2 * b1))`, ...) and the squared norm
  (`n2.mul_add(n2, n1.mul_add(n1, n0 * n0))`) was tried: it fixes 11 of
  the 42 (58 to 69 identical), so Eigen's evaluation order for the rest
  is something else; find it (build `export_stl.cc`'s three lines against
  Homebrew's Eigen with Apple clang `-O3` and compare) before changing
  `io::stl`. (libtess2 port)
- Results of Manifold booleans can list the same triangles in a different
  order, or rotate a triangle's vertices, compared with the nightly (for
  example `rotate_extrude-tests.scad`, `issue1105.scad`); a few differ in
  vertex count (`example017.scad` assembled: 623 vs 626). The 2D shapes and
  extrusions going in are identical, so this is the kernel (manifold-rust
  v3.5.0 against v3.5.2, above). Images match. (5b)
- Fused multiply-adds follow the platform, as upstream does: OpenSCAD's
  arm64 build rounds `a * b + c` in one expression as an FMA and its
  x86_64 build does not (`[1, 0.1] * [-0.010000000000000002, 0.1]` is
  `-8.32667e-19` on the arm64 nightly, `0` on its x86_64 slice). neoscad
  routes every such site through `eval::fma` (`mul_add`, fused only on
  `aarch64`): vector and matrix products, `norm`, `cross`, `lookup`, range
  iteration, `rands`, the `rotate([x,y,z])` and `mirror` matrices, the DXF
  dimension functions, and in `geom` the extrusion and 2D transforms and
  the hull orientation test. With it, BOSL2's tests match the arm64
  nightly as `.csg` on 976/976 and its examples on 2,525/2,526 (the other
  uses unseeded `rands()`). Still plain: 3D transforms
  (`PolySet::transform`), so transformed meshes can differ in the last
  bit (e.g. `rotate([30,40,50]) cube(1)` STL); fusing `PolySet::transform`
  alone fixed a few coordinates, not all. A new port of C++ arithmetic
  should use `eval::fma` where the C++ multiplies and adds in one
  expression; which product clang fuses has to be checked against the
  nightly (`rotate` fuses the first, `mirror`'s `x*x + y*y + z*z` is
  `fma(z, z, fma(x, x, y*y))`). (5b, H1)
- `lang::number::fmt_g` prints a negative NaN as `-nan`, as glibc's
  `printf` does; macOS's `printf`, and so the nightly, always prints `nan`.
  The `.csg` export now drops the sign itself (`eval::dump`), since the
  fused `rotate` matrix made `transform-nan-inf-tests.scad` carry a
  negative NaN; the other `fmt_g` callers (the OFF, OBJ, WRL, DXF and SVG
  writers in `io`) still print `-nan`. Decide which platform to match.
  (H1)
- OpenSCAD pins Clipper2 2.0.1 (submodule `c7f820f`); clipper2-rust 1.2.0
  ports 1.5.4. Every 2D case compared so far (booleans, sanitizing, all
  three offset joins, fill, projection) is byte-identical in SVG, but an
  engine change between the versions would show up here first. (5b)
- A render warning for a duplicated sibling subtree is printed once;
  OpenSCAD prints it again, because of how it caches. (5a)
- `--hardwarnings` stops at the first warning, but a geometry warning is
  only acted on after the whole geometry is built (the output is the
  same; the time is not), and an evaluator warning after its check point
  rather than at its throw (messages in between are dropped). Against
  the nightly with `--hardwarnings`, the output and exit code match on
  259 of 263 test inputs as `.echo` (the 4 others are recursion-limit
  depths; see `eval::recursion` for the policy) and 235 of 237 as `.stl`
  (the 2 others differed without the flag too, in geometry error paths,
  which H2 fixed; not rechecked with the flag).
  Warnings OpenSCAD prints inside a `catch` never raise it; the CLI knows
  them by text (`printed_in_handler` in `crates/cli/src/run.rs`), and the
  DXF ones printed by `dxf_dim()`/`dxf_cross()` in the evaluator are not
  exempted. (5f)
- Upstream's `export-param-hardwarnings` and
  `export-paramset-hardwarnings` tests pass without `--hardwarnings`
  doing anything: `shouldfail.py` appends `--export-format=json` to
  arguments that already hold `--export-format param`, and Boost rejects
  the repeated option with exit 1. neoscad now exits 1 on usage errors as
  OpenSCAD does (clap's default was 2), so they pass for the same reason;
  `--hardwarnings` with a param export is checked against the nightly by
  hand (identical). Worth reporting upstream. (5f)
- PDF export draws what Cairo draws (checked by image on all six test
  PDFs, rasterised with poppler), but the file is hand-built: labels use
  the standard Helvetica font unembedded where Cairo embeds Liberation
  Sans (same metrics, slightly different glyphs), and Cairo's object
  layout and compression are not reproduced. Cairo's path simplification
  is modelled only as far as its single-rectangle `re` output. (5f)
- Fixed (post-0.2.0): the render summary reports the geometry cache's
  size from `geom::Renderer::stats()`: the stderr summary prints
  OpenSCAD's `Geometry cache size in bytes` line and the two `CGAL ...`
  lines (always 0), and the JSON's `bytes` and `max_size` are numbers.
  The byte count is neoscad's estimate (496 for `cube(1)` against the
  nightly's 856), and `max_size` is the one 200 MiB budget, with the
  CGAL cache's at 0 (`docs/cli-json.md`). (H3)
- `-d` lists dependencies in first-seen order where OpenSCAD uses hash
  order (same set). Files read by `dxf_dim()`/`dxf_cross()` are not
  listed, and `-m` does not run for them: the evaluator reads them
  itself. (H3)
- `.ast` export does not evaluate the program, so it prints no `ECHO:` or
  evaluation warnings; the nightly evaluates first (`do_export`) and
  prints them (`echo(1);`: `ECHO: 1` on stderr). The `.ast` file is the
  same. (H3)
- `--animate` with several `-o` files evaluates each frame once for all
  of them; OpenSCAD runs all frames for the first output, then all for
  the next, so the `Exporting ...` lines come in a different order. The
  files are the same. (H3)
- `--debug` prints OpenSCAD's `Debug on.` line and nothing more; neoscad
  has no `PRINTDB` output. (H3)
- OFF export writes no colour for a face whose colour is invalid
  (`color()` with no arguments), where the nightly writes `0 0 0 0`
  (`export_off.cc:72-74`); the warnings match. Matching the bytes would
  make tier 3's `render-manifold_issue5216` fail as it does with the
  nightly (the re-import draws the face transparent), so the harness
  limit for it was dropped instead and `conformance run --binary
  <nightly>` now reports that case as a failure. (H3)
- A `\r` inside `include<>`/`use<>` brackets doesn't count as a new line,
  as it does in OpenSCAD. (2)
- Malformed parameter-set JSON gives different error text from Boost. (2)
- SVG import: libsvg's arc math is not modelled with the nightly's fused
  multiply-adds, so an arc whose sweep lands within ~1e-6 of a step
  boundary gets one step fewer (`spec-paths-arcs01.svg`: 15 vs 16 steps at
  180 degrees). The other 110 SVG import cases compared are byte for byte
  the same; the images match. (5c)
- 3MF: lib3mf's error texts for malformed files are only reproduced for a
  missing file, a non-ZIP and an empty file; other problems get our own
  description in OpenSCAD's frame. Exported object and build UUIDs are
  hashes of the content rather than random, so files are reproducible.
  (5c) Every `-O export-3mf/...` option is implemented; against the
  nightly, 55 option and model combinations give the same model XML apart
  from UUIDs, the date and triangle order, which follows the tessellation
  differences above. (H3)
- libxml prints its own parser diagnostics to stderr for a broken SVG
  (`file:1: parser error : ...`) before OpenSCAD's "Error parsing file";
  only the latter is reproduced. Likewise the lines libsvg writes to
  stdout (an invalid transform, a `<use>` href that is not `#id`). (5c)
- OBJ: `f 1  2 3` (two separators in a row) crashes OpenSCAD with an
  uncaught `bad_lexical_cast`; the empty word is skipped here. (5c)
- `.nef3` import (`io::nef3`) meshes the file as the Manifold backend
  does, without CGAL. Facets, holed or not, go through the libtess2 port
  as upstream's `tessellatePolygonWithHoles` takes them; OFF exports of
  nine fixtures (holes, cavities, rotated text, a 27 MB sphere) match
  the nightly byte for byte. A failed read names CGAL's header as
  `CGAL/Nef_3/SNC_io_parser.h`, not the build machine's absolute path
  the nightly prints. Not done: CGAL's
  consistency checks beyond index ranges (a file whose pointers are
  wrong but in range imports as whatever its facet cycles say; OpenSCAD
  may crash or hang on it), and shalfloop facet cycles, which have no
  edges and give no polygon. CGAL's own stderr block ("CGAL warning:
  check violation!") before OpenSCAD's messages is not reproduced.
- The experimental `import()` function (JSON, `--enable
  import-function`) does not record the file for `-d` dependency output
  or `-m` (OpenSCAD's `handle_dep`); the file is read whole, so a huge
  one costs its size in memory before the limit stops the values it
  becomes. Paths in its messages are the calling file's directory joined
  with the name, as given (OpenSCAD makes them absolute). (experimental
  features)
- Experimental objects: OpenSCAD makes a method (a stored function with
  a `this` parameter) when an object is built, through a context cycle
  its garbage collector breaks; neoscad binds it when read
  (`value::Object`), with the same results in every case the nightly was
  checked on. (experimental features)
- The nightly's own OFF of `rotate_extrude-touch-edge.scad` (and
  `-touch-vertex.scad`), re-imported with `import()` and exported as OBJ,
  STL or 3MF: neoscad writes all 240 triangles, the nightly 120
  (touch-edge) or 180 (touch-vertex); OFF, WRL and POV agree. The file
  lists `-0` and `0` corners as separate vertices on the axis, so faces
  may collapse once they merge; whether the nightly drops them on import
  or in `tessellate_faces` was not investigated. Flag-independent (HEAD
  `7f6c216` does the same). Found checking `predictible-output`.
- `predictible-output` sorts coloured faces with a stable sort where
  upstream's `std::sort` leaves equal faces of different colours in an
  unspecified order; only a mesh with duplicate faces can show it.

## macOS app

- Updates (Sparkle; `docs/release.md`, "The macOS app's updates"), left
  after the first version:
  - Done since: the key exists (2026-10-02), the `appcast` job has run
    for v0.3.0-rc.1, v0.3.0 and v0.3.1 with `--require-notarized` on
    their notarized DMGs, and an installed 0.3.0 updated itself to 0.3.1
    (docs/audits/auto-update.md, step 3).
  - The update dialog shows a heading and a link to the GitHub release,
    not the notes themselves. Sparkle could show the release body as
    HTML, embedded in the signed appcast; that needs Markdown to HTML in
    the job.
  - No delta updates; each update is the whole DMG (the audit's "Later").
  - Key rotation is written up but untried. It relies on Sparkle
    accepting a new key when the new app's code signature matches the
    old app's team (`SUUpdateValidator.m` in Sparkle 2.10.0).
  - Sparkle's XPC services ship in the app but aren't used, because the
    app isn't sandboxed. Sparkle documents removing them for such apps;
    that would save a little size and two signatures.
  - `scripts/apple/test-updates.sh` builds the app twice with
    `release.sh` (about the cost of two releases' Swift and CLI builds)
    and runs it on the developer's Mac only. CI doesn't run it.

- Shared core, left after `docs/audits/shared-core.md` steps 1-6: the
  web page still keeps its own console filter list, printer presets,
  customizer snap/clamp (`web/src/model/customizer.js`), measure overlay
  and document loop, because the page cannot call the worker's core
  synchronously; `defaults.tables` and each run's `summary` now carry the
  core's versions, so switching them is page work. The page keeps its
  300 ms pause (the core default is 150 ms). The preview's unreadable
  files are still parsed out of message text: the loader's diagnostics
  have no structured path. The printer presets' build volumes are still
  unchecked against the makers' spec sheets. Two examples need BOSL2,
  which the macOS app does not bundle (`Example.libraries` says so; the
  menu lists them anyway). The C# bindings are generated in CI
  (`windows-app.yml` runs `scripts/windows/build-core.ps1`), from an
  unreleased uniffi-bindgen-cs for uniffi 0.32 (see "Windows").
- A viewport frame holds the main thread for about 2.6 ms (p50; p95
  3.3 ms) at 60 Hz, nearly all of it `-[CAMetalLayer nextDrawable]`
  waiting for a free drawable; encoding is 0.14 ms. `Immediate` present
  mode halved it against `Fifo` (whose wait was up to a whole refresh).
  A render thread would take the wait off the main thread, but wgpu-hal's
  acquire reads the window's `occlusionState`
  (`wgpu-hal-30.0.1/src/metal/surface.rs:353-369`), an AppKit property of
  the main thread; `CAMetalDisplayLink` hands out drawables itself, which
  wgpu cannot take. (8c)
- 120 Hz pacing is unverified: the only display online during 8c was a
  60 Hz external one. The display link asks for up to 120 Hz
  (`preferredFrameRateRange`); run `NEOSCAD_VIEWPORT_BENCH=8` on a
  ProMotion panel. (8c)
- View-option lines are one pixel and the scale markers' numbers are sized
  in pixels, as in OpenSCAD's offscreen export (DPI 1). OpenSCAD's GUI
  scales them by the screen's DPI (`glLineWidth(dpi)`), so on a 2x display
  NeoSCAD's are half as heavy. (8c)
- Switching between the light and dark scheme renders the last request
  again, because `session.render` bakes the scheme's face colours into the
  geometry (`geom::color::Scheme`); a slow render-mode model is recomputed.
  (8c)
- Memory (8f; `footprint`, Debug app, one window 1320x760 points on a 2x
  display). The 900-950 MB measured in 8d was six windows restored from
  the saved state, not one file. One window with `cube(10)` went from
  114 to 52 MB: its two 4x MSAA buffers (colour and depth, 61 MB at
  1280x1520 pixels) are memoryless now (`TRANSIENT_ATTACHMENT`). 125
  spheres: 834 to 121 MB, from the allocator's cache of freed large
  blocks (338 MB "Malloc Large (empty)"; 8f turned it off with
  `MallocLargeCache=0`, which mimalloc has since replaced, `6b64480`) and the upload's staging copy of the vertex data
  (57 MB, now freed after the upload). The six windows: 731 to 257 MB.
  What remains per window is mostly the model's vertex buffer, about
  20 MB of malloc (parse caches, the language server's index), the
  layer's drawables and WebKit's layers (10 MB IOSurface). Left open:
  - mimalloc (O1) replaced `MallocLargeCache=0`, but keeps more after a
    big preview: 125 spheres settle at about 210 MB 30 s after the preview
    (about 90 MB of it mimalloc's, tagged "IOAccelerator" by `footprint`,
    since mimalloc marks its memory with VM tag 100), where the system
    allocator without its large cache settled at about 125 MB.
    `MIMALLOC_PURGE_DELAY=0` gets there within a second instead of about
    30 but no lower, and costs 1-7% of render time. mimalloc v2 (the
    crate's `v2` feature) settled lower in `neoscad serve` with that
    variable (83 against 185 MB), but was 2-3% slower and peaked 65%
    higher on fractal_tree (2.52 against 1.53 GB); v3 is also what
    OpenSCAD ships (its `submodules/mimalloc` is the v3.3.2 tag). Worth
    trying: `mi_collect` on the pool's threads after a run (needs an
    `unsafe` call into `libmimalloc-sys`). (O1)
  - The web content processes were not measured again (56 MB and a 32 MB
    prewarmed one in 8d).
  - A preview's peak is far above its result: 125 spheres peaked at
    972 MB for 13 MB of live data afterwards; not broken down.
  - The snapshot renderer now shares the viewports' device
    (`Offscreen::on_gpu`); the app made no snapshot before, so the second
    device had not cost memory at idle yet.
- Mouse mapping covers orbit, pan and zoom; OpenSCAD's shift-drag
  (pitch/roll), middle-drag (forward/back), shift-wheel (field of view) and
  zoom-to-cursor are not mapped. A Magic Mouse's precise scroll pans, as a
  trackpad's does. (8c)
- The file's `$vpt`, `$vpr`, `$vpd` and `$vpf` move the view when they
  first appear and whenever their values change; OpenSCAD's GUI applies
  them after every evaluation (`Camera::updateView` from
  `MainWindow::instantiateRoot`). Live preview runs after each pause in
  typing, and snapping the view back after each keystroke would undo
  every orbit, hence the difference. The program also sees the view it
  is shown in as `$vp*` (`setRenderVariables`), not the command line's
  default camera. (8f)
- An untitled document's relative `include`s and `use`s resolve against
  the document controller's current directory (the last folder a file
  was opened from or saved to, else Documents), as if it were a file
  named "Untitled.scad" there. OpenSCAD resolves them against its
  working directory (`parser.y`, `fs::current_path()`), which is `/` for
  a Finder-launched app. (8f)
- Document loop gaps (8f):
  - Echo lines carry no location, so they do not jump: the evaluator
    emits them without one (OpenSCAD prints none), and giving the record
    one needs the call's span passed into `echo`.
  - A change to another open document's unsaved text does not re-run the
    documents that include it; only changes on disk to files a run read
    do (`FileWatcher`), and open documents are not watched (their
    buffers, not the disk, are what runs read).
  - Customizer values run as `-D` assignments after the text; OpenSCAD's
    GUI writes them into the parsed program (`applyParameters`). Top-level
    reassignment makes the two the same for a top-level assignment. Not
    done: parameter-name NFC normalisation when reading sets
    (`ParameterSets::readFile`), deleting a set, the description-only and
    other view styles, and the Animate panel.
  - Browse All Versions and Revert To are AppKit's for an
    `autosavesInPlace` document with the File menu's Revert item; the
    app test covers saving in place and reverting to other contents, not
    the versions browser: NSFileVersion keeps no versions in the
    temporary directory, and the browser needs a person. Checklist: open
    a saved file, edit, wait for the autosave, edit again; File > Revert
    To > Browse All Versions shows the earlier text; choosing one
    restores it in the editor and the view; File > Revert To > Last
    Saved (Opened) does the same for the opened version.
- Each editor keystroke costs the app about 1 ms on a 1.1 MB file (p50;
  `EditorBenchmark`), nearly all of it the offset conversion and the edit
  of the document's `String` copy, which are linear in the text; the
  core's `edit` is 0.07 ms. A rope or an incremental line index would make
  it logarithmic. The round trip from the page's change to the app's copy
  is 2.2 ms p50 (3.1 ms p95), CodeMirror's own work 1-2 ms. (8d)
- Editor checks that need a person (verified by test so far: IME's
  NSTextInputClient calls commit once, in `EditorTests`; keys through
  NSApp, in the opt-in `EditorKeyTests`):
  - Japanese input with the system input method: composing (underlined
    marked text, the candidate window placed at the caret), committing,
    cancelling with Escape, and reconversion; the same for Chinese
    (Pinyin) and Korean. The document must see only committed text.
  - Dead keys and the accent menu (hold `e`).
  - VoiceOver: the editor is announced as a text area labelled "OpenSCAD
    source"; reading by line, word and character; hearing typed and
    deleted text; lint markers and the search panel being reachable.
    An in-process query of the web view's accessibility tree found no text
    area without an assistive client attached, so this was not testable
    from `xcodebuild`.
  - Dictation, and Services (Edit > Services) on selected text.
  (8d)
- The key tests that go through NSApp (`EditorKeyTests`) run only with
  `NEOSCAD_EDITOR_KEYS=1` and the test host in front: macOS does not let a
  test host started in the background take focus. They passed once with the
  host brought forward (`open -a` on its bundle), but a later run could not
  get it forward. Whether the menu alone would take F5 and F6 from a
  focused web view is unverified; the editor forwards them itself
  (`appKeys` in `src/editor.js`). (8d)
- With the editor focused, CodeMirror's keymap takes ⌘[ and ⌘] (indent
  less and more), so the View menu's Zoom Out and Zoom In keys do not reach
  the 3D view; they still work with the view focused. Undo and Redo work
  only while the editor has focus: elsewhere the window's own undo manager
  answers the menu, and it has nothing to undo. (8d)
- The builtin names are coloured by name (OpenSCAD's editor's lists), so a
  user module called `cube` is coloured as the primitive; OpenSCAD's editor
  does the same. The grammar accepts non-ASCII identifiers, which OpenSCAD
  accepts only with the `unicode-identifiers` feature; the core's
  diagnostic marks them. (8d)
- The editor runs in the page's content world, not a dedicated
  `WKContentWorld` as `docs/audits/macos-prep.md` §4 suggested: the page
  holds only the bundle (the Content-Security-Policy admits no other
  script and no network), so there is nothing to isolate the bridge from.
  Revisit if the page ever shows content from elsewhere. (8d)
- Building the app needs node 18 or newer (`scripts/apple/build-editor.sh`
  finds nvm's and Homebrew's), and the network once, for `npm ci`. (8d)
- Release (8j, `docs/release.md`):
  - The Developer ID path (`-exportArchive`, notarization, stapling, an
    accepting Gatekeeper) has never run: no Developer ID identity or
    notary profile exists yet. The first signed release is its test, and
    the clean-machine checklist in `docs/release.md` is still open.
  - Ad-hoc release builds carry
    `com.apple.security.cs.disable-library-validation`, since the
    hardened runtime will not load an ad-hoc framework into an ad-hoc
    process; a Developer ID build must not, and the script checks.

- Check and measure panels, export and App Intents (8i):
  - The check panel marks findings in the view with numbered rings and
    the selected finding's box (`render::viewport::Annotations`); it does
    not paint thin-wall, overhang and floating faces as `snapshot
    --issues` does, which needs a second scene drawn over the model
    (`session::snapshot::marked_scene` is private and builds the whole
    model).
  - "Auto" checks after each render (F6), not each preview: `check`
    renders the model in full (`Session::check` calls `render_parts`),
    which after every pause in typing would cost a render. Check and
    measure evaluate the text again even when the last render was of the
    same text; the geometry cache makes the build cheap, but the
    evaluation is repeated.
  - The printer presets' bed sizes (`PrinterPreset.all`) were written
    from memory, not checked against the makers' spec sheets.
  - Measurements are of the text when Measure was pressed; an edit does
    not re-measure or mark them stale. Picking casts against the model's
    solid only, not a part's.
  - Export progress is by stage (parse, evaluate, geometry); encoding and
    writing come after the last check for cancellation, so Cancel stops
    evaluation and geometry but not a large file's encoding.
  - AMF: neither NeoSCAD nor OpenSCAD's current source exports it
    (`.reference/openscad/src/io/export.h:27-46` has no AMF format), so
    File > Export does not offer it. WRL and POV, which both have, are
    not in the popup either (`ExportFormat`); the core writes them.
  - Only the 3MF options (colour mode, colour, material type) are
    offered; SVG fill and stroke, PDF paper and 3MF unit and metadata
    are the core's defaults.
  - App Intents: the file parameters accept `public.plain-text`, not
    `org.openscad.scad`: the metadata processor refuses a type it cannot
    resolve at build time ("Could not determine the identifier of
    '.scad', please use a UTType defined by
    UniformTypeIdentifiers.framework"). Outputs go to
    `$TMPDIR/NeoSCAD-Shortcuts/<uuid>/` and are left to the system's
    temporary-file cleanup. A file handed over as data (no URL) runs
    from a temporary copy, so its relative includes do not resolve.
  - Registration was verified from the built app's
    `Contents/Resources/Metadata.appintents/extract.actionsdata` (three
    actions and three App Shortcuts). Not verified: that Shortcuts.app
    lists them and runs them (the `shortcuts` command only lists and
    runs the user's own shortcuts), nor Siri or Spotlight phrases. The
    test host logs `connection to service named
    com.apple.linkd.autoShortcut` errors at launch, as a test host that
    is not a registered app would.
  - The intents run on the app's shared core, whose limits are
    `Limits::AGENT` because nothing in the app changes them; a future
    limits preference would reach the intents too.
- Fixed: the customizer's parse ran on the caller's thread (a dispatch
  queue's 512 KiB in the macOS app), and a document at the parser's
  nesting limit overflowed it. `client`'s `customizer()` now parses on
  the evaluator's stack, for every app. The audit of the other entry
  points found three more that parse on the caller's stack, now on the
  evaluator's too: `LanguageServer::publish_diagnostics` and the
  diagnostics `run_document` hands the language server
  (`lsp::Server::supply`), both of which parse the document for its
  markers when `handle` has not (and the Linux app's `Language::supply`),
  and `neoscad serve`'s request threads, where `format`, `docs` and
  `test` parse (`format` overflowed 2 MiB at the limit). Freeing a
  parsed document recurses too: a release build freeing a language
  server at the limit needed between 256 and 512 KiB, so the core and
  the language server free what they hold on a thread of their own
  (`crates/ffi/src/lib.rs`, `FreedDeep`). Tested on a 512 KiB thread in
  `crates/client/src/tests.rs` and `crates/ffi/src/tests.rs`.

## Language server
- `neoscad lsp --stdio`'s diagnostics are the session's parse and
  evaluation, not the geometry stage: warnings only a render prints (the
  kernels', `render()`'s) reach the console, not the markers. The app's
  markers come from its runs instead (`Options::host_diagnostics`), the
  geometry stage's warnings included; a preview's geometry stage says
  less than a render's, so an F6 render's warnings show until the next
  edit's preview replaces them. (8e, 8f)
- Name resolution is lexical from the syntax tree (`crates/lsp/src/index.rs`,
  `world.rs`), not the evaluator's: an `include` inside a module body is
  treated as a top-level one; an empty `include <>` does not reuse the
  previous name as OpenSCAD's scanner does; among included files a module
  defined twice resolves to the file asked from, then the document, then
  the includes in the order they were found (OpenSCAD's last definition
  wins); `use`d libraries are searched last `use` first. (8e)
- The evaluator now resolves names statically (`crates/eval/src/resolve.rs`,
  O4), and some of its rules differ from the index's "assignments are
  visible throughout their scope": an assignment that reads a name
  assigned later in the same scope gets the outer binding
  (`a = 5; module m() { b = a; a = 1; }` sets `b` to 5); a function called
  while its scope is being initialised sees only the assignments made so
  far; a named argument that is not a parameter binds in the callee's body
  (`function f() = zz; f(zz = 5)` is 5); and parameter defaults are
  evaluated in the defining scope, so they never see other parameters.
  The index could share `resolve`'s region model, but the resolver works
  on a whole program's spliced AST and the index per file on the syntax
  tree, accepting broken code, so sharing means moving the model into
  `lang`. (O4)
- References and rename see the document and what it includes, not the
  files that include it (there is no workspace index): renaming a
  top-level name of a file other files include can break them. Rename
  refuses whenever an included file defines or uses the name, and renames
  a parameter's named arguments only in calls within the document. (8e)
- Completion: no path completion inside `include <...>` and `use <...>`;
  no `completionItem/resolve` (each item carries its one-line summary);
  more than 400 matches are cut and marked incomplete. (8e)
- Hover shows a top-level constant's value when it folds from the syntax
  (literals, vectors, arithmetic, conditionals, other constants); function
  calls and `$` variables show only the expression. (8e)
- Formatting follows `.neoscad-fmt.toml`, not the client's `tabSize` and
  `insertSpaces`. Range formatting formats the top-level statements the
  range touches, as a file of their own (the formatter lays out whole
  programs). (8e)
- Positions count lines at `\n` only, as the core and the app's editor do;
  a client that also breaks lines at a lone `\r` (VS Code) disagrees on a
  file with classic Mac line endings. UTF-16 is the only position
  encoding offered. (8e)
- `$/cancelRequest` is ignored: requests are answered synchronously in
  milliseconds; only diagnostics' evaluations stop (on a newer change or
  the host's cancel). (8e)
- The app has keys but no menu items for the language features: Format
  Document (⌥⇧F), Go to Definition (F12, ⌘-click), Rename (F2) and Find
  References (⇧F12). ⌘-click now goes to the definition and ⌥-click adds
  a cursor (in 8d ⌘-click added one), as in Xcode and VS Code. (8e)
- Library viewers (read-only tabs for BOSL2, MCAD and other library files)
  are not documents: they are not restored after a relaunch, and one
  showing the bundled MCAD, which exists only in memory, has no proxy
  icon. A library file changed on disk while shown is not reloaded. (8e)
- Keystroke to markers and to the view (`PipelineLatencyTests`, p50):
  CSG.scad 189 and 184 ms, a BOSL2 cuboid 195 and 192 ms; before 8f the
  markers took 167 and 180 ms and the view 431 and 433 ms (two
  evaluations per pause, the view's after a 400 ms pause). Nearly all of
  it is the 150 ms pause (`SCADDocument.previewDelay`). (8e, 8f)
- The release `wasm_check.wasm` is 46.3 MB with the language server in it
  (the WASM section's 38 MB is from H2); the language server's share was
  not measured. (8e)

## Rendering
- `vendor/wgpu-core` is crates.io wgpu-core 30.0.1 with gfx-rs/wgpu#9958
  applied, the fix for a panic when a timed `poll(Wait)` expires while
  another thread polls the same device (CI run 36632690616). It is what
  lets GPU readbacks and the staging release wait for at most 10 s again.
  When a wgpu release carries #9958, move to it and drop the copy (see
  `vendor/README.md`); until then, a wgpu upgrade means re-vendoring.
- Previews draw a CSG product's visible surface from real Manifold
  booleans (`geom::csg::product_meshes`) when every leaf bounds a solid
  (`PolySet::is_outward_solid`), so those products do not show OpenCSG's
  image-space artefacts: z-fighting where a positive and a negative face
  are coplanar, and holes from a `convexity` set too low. (6b)
- A product with a leaf that does not bound a solid (inside out, a face
  flipped, not closed) is drawn with OpenCSG's SCS algorithm on the GPU
  (`render::gpu`, "Image-space CSG"); `preview-manifold_polyhedron-tests`
  now matches pixel for pixel. Gaps: meshes do not carry `convexity`, so
  where OpenSCAD would pick Goldfeather (a primitive with convexity 2 or
  more) SCS is used; with more than 20 primitives OpenCSG repeats the
  subtractions until occlusion queries report no change, where this
  always runs the Schoenfield sequence; and `is_outward_solid` misses a
  separate inside-out shell next to a larger outward one (total volume
  still positive). Such a product's frame is several render passes, so
  the app's viewport keeps its MSAA and depth buffers in memory (not
  memoryless) while such a model is shown.
- `polyhedron-tests.scad` rendered to OFF differs from the nightly's: the
  same 47 vertices in another order, and the cut face triangulated
  differently (a different diagonal vertex). The render path was not
  touched by the SCS work; HEAD gives the same bytes, and the tier 3 case
  passes on its geometric comparison.
- A preview's `#` objects are drawn with a small depth offset
  (`DrawState::bias`, constant -2, slope -0.5) so that they show on the
  cut faces they make, whose triangles the boolean re-split (OpenCSG
  compares the very same triangles there). The values pass every
  highlight case; a `#` object within that offset behind a surface would
  show through it. (6b)
- With `--csglimit` exceeded, OpenSCAD's preview draws nothing (the
  normaliser gives up on the whole term), and so does neoscad's. For the
  GUI and snapshots the real boolean of the unnormalised term would be a
  better fallback; not done, to stay with OpenSCAD. (6b)
- The render summary after a PNG preview reports 0 geometry cache
  entries (`CsgTree::build` does not return the renderer's count). (6b)
- Not a gap (checked post-0.2.0): `.term` export prints "No top-level
  CSG object" for every input, and so does OpenSCAD's. `openscad.cc`
  builds the term with a `CSGTreeEvaluator` that has no geometry
  evaluator, so every leaf is a null term (`CSGTreeEvaluator.cc`,
  `visit(AbstractPolyNode)`); the nightly prints that line for
  `cube(1);` and for a model with booleans, `#` and `%`, and all three
  `csgterm` regression outputs are that line. Porting `CSGNode::dump`
  would diverge from it. (6b)
- Preview speed (best of 3, wall, this machine): at most `--render`'s
  time on the benchmark models, and 1.5-4.5x faster than the nightly's
  OpenCSG preview, except `csg_spheres` (380 ms against the nightly's
  273: one product of a cube minus 125 spheres is one big boolean,
  which OpenCSG never computes) and `text_30lines` (550 against 486, as
  in render mode). (6b)
- PNG export needs a GPU adapter (Metal, Vulkan, Direct3D 12). Without
  one it fails with "no GPU adapter"; a headless Linux CI runner would
  need a software Vulkan driver (lavapipe), or neoscad a CPU rasteriser.
  The PNG tests in `crates/render/tests/offscreen.rs` skip without one.
  (6a)
- Determinism: the same scene gives the same PNG bytes on one machine
  (checked with two devices on one GPU in `offscreen.rs`), but
  rasterisation rules differ between GPUs and drivers at the pixel level
  (edge pixels, depth ties between coplanar faces). Only Metal on an
  Apple M4 Pro has been measured: 318 of 320 render-mode images pass
  `image_compare` against OpenSCAD's goldens, 206 pixel-identical. Other
  GPUs are unverified. (6a)
- The first PNG export after a reboot or driver update pays for Metal's
  shader compilation (about 0.5 s on this machine; the system caches it
  after that, and a warm export costs about 18 ms over the geometry). (6a)
- Colour schemes are only the built-in and vendored ones; OpenSCAD also
  reads `color-schemes/render/*.json` from the user's configuration
  directory. The app can pass such files to `render::scheme::parse`. (6a)
- OpenSCAD's `PolySetRenderer` draws nothing (and logs an error) for a
  result holding both 3D and 2D parts; `geom` never returns such a
  result, so the case is not handled. (6a)

## Parts, check and measure
- A part's solid is its subtree's geometry: a part under a `difference()`
  that cuts it is measured uncut (its `context` is only set for the
  operations that change a part as a whole: subtracting it,
  intersecting, hull, minkowski, resize, 2D). Measuring "what of the
  model belongs to the part" would need the model's faces by part plus
  closing the cut. (7b-1)
- Face attribution survives booleans, transforms and `color()` (over
  several parts the IDs are kept instead of collapsed, which changes
  only how an export groups triangles); `hull()`, `minkowski()` and
  2D operations make new solids and drop the parts inside them. (7b-1)
- Wall thickness is sampled along face normals from fixed points per
  face (up to 16 on large faces, at most 400,000 rays); a wall whose
  sides are not parallel measures thicker than its narrowest point, and
  a narrow feature in the middle of a big face between samples can be
  missed. A medial-axis or sphere-probe estimate would be exact. Knife
  edges formed by two faces sharing a corner are skipped; the feather
  edges a `difference()` leaves where a curved cut meets a face are
  reported (correctly thin, but many). (7b-1)
- Thin walls are measured in the layer plane as well as along the
  normal, so a flat face counts only against a flat far side, and a
  leaning plate is judged by its width in the layer. A thin roof or
  floor that is sloped (a 0.4 mm shell at 5-30° from horizontal) is
  wide in each layer and is not reported, though it is only a few
  layers thick; a separate "too few layers" check (vertical thickness
  under flat and shallow faces, excluding the bed's first layers) would
  cover it. (CAD pilot fix)
- Fixed (post-0.2.0): a sealed hollow (`difference() { cube(20);
  translate([.5,.5,.5]) cube(19); }`) reported its cavity's inner
  surface as a `floating` piece. A component wound inward inside one
  wound outward is now a `cavity` (info) finding, counted in the model's
  `cavities`, and is neither `floating` nor `tiny-feature`. (found in the
  CAD pilot fix)
- The `not-manifold` finding for a pinched solid counts edges only; two
  pieces touching at a single point (tip to tip) are not found. A
  vertex whose faces form more than one fan after welding would be the
  test. (CAD pilot fix)
- Input mesh warnings (`session::orient`, the `polyhedron-*` codes)
  judge a closed shell's outside by its signed volume, and a shell whose
  bounding box lies strictly inside another's is taken for a cavity. An
  inside-out shell nested in a correct one therefore passes as a cavity,
  and a real cavity whose box pokes out of its host's box (never, for a
  true cavity) would be misjudged; a ray-parity test of one point would
  settle both. Corners weld by exact position, so a gap Manifold's merge
  closes (`issue5135-good.scad`, 2e-12) is reported as open; right for a
  lone polyhedron, which exports open, noise under a boolean. Points
  under `resize()` are in the child's own coordinates. Minkowski
  operands are reported although a convex one is only hulled (harmless
  inside out). `evaluate` (and the language server) re-analyses each
  polyhedron on every run, with no memo: about 30 ms for a 360,000-face
  polyhedron. The CLI's own `--format json` export runs report them;
  the plain console never does. (inside-out polyhedron diagnostics)
- Overhangs do not recognise bridges (a flat span supported at both
  ends); they are reported as overhangs. (7b-1)
- Corner samples of wall thickness (T3 audit fix) cover at most
  max(128, one in 128) faces, the most promising first, to stay within
  about 5% of `check`'s time (A/B over the examples and features:
  +3.8% in total, +4% on the run's adapter). A model with many faces
  that could each hide the thinnest wall can still report a
  `min_wall` above the truth, which is why it carries `sampled`. The
  corner readings also lower some thin-wall findings' values and add
  thin-wall findings near the ends of tapering walls (a `surface()`
  heightmap's base gained four at 0.6 mm): real, but a different list
  than before for the same model. (T3 audit fix)
- An overhang's "steeper than" split uses one fixed threshold,
  `max_overhang` + 15° (60° by default). A thread's flanks at exactly
  60° are not "steeper", so a ledge among 61° flanks would be reported
  together with them; a per-angle histogram would say more, in more
  text. (T3 audit fix)
- A profile's crests are refined with about 70 more cuts each (up to
  100 crests, then left at their samples): on a mesh where a cut takes
  a millisecond, 100 crests add about 7 s. The end-crest test (cut off,
  lower by a tenth of the crest height, or a top width off by a quarter)
  is tuned on the two T3 adapters and could drop a real end crest of
  an irregular profile from the pitch fit. (T3 audit fix)
- Checks run serially (about 150 ms for a 220k-triangle model). Rays are
  independent, so they could run on rayon with a deterministic merge.
  (7b-1)
- `snapshot --issues` uses the default check settings from the command
  line (the server's `issues` takes any). Markers are drawn whether or
  not the model hides the point from that view. (7b-1)
- `measure --section` cuts the model or one part; a per-part breakdown
  of a model section is not reported. (7b-1)
- `stl-precision` welds the whole model's mesh at 32-bit precision, not
  each part's; a part exported alone could break where the model does
  not (or the reverse). It models a reader that welds exactly by `f32`
  position; slicers that weld with a tolerance (PrusaSlicer's repair,
  admesh) merge more, so a clean result is not a guarantee for them.
  Nor does it catch faces that stay distinct in `f32` but flip or go
  near-zero in area (the grader counts those as degenerate: 4013 on the
  pilot's thread against 2998 collapsed). NeoSCAD's exporters write the
  mesh as is: snapping vertices to `f32` and re-welding before an STL
  export would fix these files, but changes output OpenSCAD's
  regression tests compare, so it would need to be opt-in.
- `measure between` works only on two `part()`s of one model. An
  assembly check wants two files, each rendered with its own top-level
  `$fn`/`$fa`/`$fs` and an optional transform (T2 audit #1, the full
  version; only the `use-special-variables` hint was done). Resting
  parts always read "touching, 0", so `between` should also give the
  smallest gap between faces *not* in contact (the lip clearance), and
  take a list of moves (a lift sweep: overlap volume at each offset,
  for snap engagement) (T2 audit #7).
- `check` reports every flat downward span as an `overhang`; a span
  between two walls prints as a bridge. Classify those as bridges, with
  the span's length, and warn only past a bridging limit (T2 audit #8).
- The `use-special-variables` hint is left out for a variable that a
  calling file assigns *anywhere*, which is conservative (a
  `cylinder($fn = 8)` elsewhere hides it); it covers module calls, not
  functions of the used file that read `$fn`; and it is given at the
  first call into each file only. (T2 audit fixes)
- The "the parts only touch" fix is given for a pinched zero-volume
  result. Parts that touch on whole faces usually intersect to nothing
  at all, and an empty result says only "empty": telling that apart
  from parts that are far apart would need the operands' distance.
  (T2 audit fixes)
- The MCP tools import a mesh `path`; `neoscad check out.stl` on the
  command line still parses it as OpenSCAD and fails with a syntax
  error. (T2 audit fixes)

## MCP and the agent eval
- "Using NeoSCAD with your agent" (`client::agent_setup::usage`, FFI
  `agent_setup_usage(host, client)`) is shown by all three apps, each
  under a client picker that shows one client's setup at a time (macOS:
  a segmented control; Windows: a `SelectorBar` and an `Expander`,
  `windows/NeoSCAD.App/MainWindow.Agents.cs`; Linux: linked toggle
  buttons, `crates/linux-app/src/app/agent.rs`). The Linux text was
  checked against the app: the smoke test sees the preview run after an
  agent's edit, the switch is on Preferences > Agents, and the marks
  chip shows and its button clears the marks (`TYPE=1`). On Windows it
  was checked by reading the code only; what is left to see on a Windows
  machine is under "Windows", the agent UI item. The web page's "Things
  to ask" (`web/src/ui/agent.js`, `IDEAS`) is the examples' source; a
  test keeps them in step.
- **Done: pass `--root` in Claude Desktop's setup.** Its entry is now
  `<cli> mcp --root <Documents>/NeoSCAD` (the owner's choice of folder),
  made by the setup, so its agent can export; relative paths resolve
  into it (docs/mcp.md, "Setup from the apps"). Left: the Windows
  dialog does not look before Add… (`agent_setup_claude_desktop_status`
  is there for it, as the macOS sheet uses it), so an earlier version's
  entry shows as nothing until Add… updates it, and its message then
  says "Added" rather than "Updated" (`Written.replaced_entry`). Also,
  `ClaudeDesktopsConfigIsMergedWithABackup` (windows/NeoSCAD.Tests)
  points `%APPDATA%` at a scratch folder, but the Documents known folder
  ignores the environment, so that test now makes the machine's real
  `Documents\NeoSCAD`; an override for tests (as `NEOSCAD_AGENT_DIR` is
  for the link) would keep it in the scratch folder.
- Another program's change to the open file (an agent through `neoscad
  mcp`; `docs/audits/agent-connection-desktop.md`, finding 1) is taken
  in or reported in all three apps (`crates/client/src/disk.rs`). Left:
  - The page's `agentEdit` takes no base version. A keystroke landing in
    the instant between the host working out a reload's edits and the
    page applying them makes the page apply them to text they were not
    worked out for: nothing on disk is lost and the document stays
    edited, but the editor can show a garbled merge until undone.
    Passing the editor version the edits are against, and refusing in
    the page when it moved, would close it.
  - macOS: the hosted tests run with the app in the background (they
    could not bring it forward), where AppKit did not take in a plain
    write or a rename-over at all within 8 s. Whether it reverts on
    becoming active, alongside the app's own reload, was not observed;
    `presentedItemDidChange` is overridden, and the reload brings
    NSDocument's `fileModificationDate` up to date, so a check after it
    should find nothing to revert.
- `neoscad mcp` implements MCP 2026-07-28 statelessly plus the legacy
  `initialize` handshake, and only the core: no `subscriptions/listen`,
  no progress notifications (a long render sends nothing until it
  ends), no logging, no MRTR (`input_required`), no completions. The
  client's `roots` capability is not read either: the roots are the
  working directory and `--root`s given at start. (7c)
- **Done: Claude Desktop's undefined working directory.** Claude Desktop
  starts servers in an undefined working directory ("like `/` on macOS").
  `neoscad mcp` no longer takes its working directory as a writable root
  when it is `/` or a drive root, the home folder or one containing it, a
  dot folder, `~/Library`/`~/AppData`, a system tree, or exactly a shared
  folder such as `/tmp`; explicit `--root`s always count, and an attached
  app's document folder is readable (docs/mcp.md, "Safety"). The user's
  temp directory is excepted from the `~/AppData` rule: on Windows it is
  `~\AppData\Local\Temp`, and refusing it broke every MCP test there.
- The CLI the apps bundle (macOS `Contents/Helpers/neoscad`, Windows
  `bin\neoscad.exe`, the Flatpak's `/app/bin/neoscad`) is built without
  the PGO profile cargo-dist's CLI builds use (`release.yml`), so it is
  that much slower than the standalone CLI. Feeding the same profile to
  `scripts/apple/build-cli.sh`, `build-msi.ps1` and the Flatpak needs the
  profile as a build input.
- Claude Code (2.1.283) shows the model the JSON of `structuredContent`
  instead of the text summary when a result has both, so the text is
  what other clients see. If a client shows both, a result costs about
  twice its tokens; a flag to send only one would fix that. (7c)
- `crates/cli/tests/memory.rs` times each MCP render only after the reply
  arrives, and `Mcp::tool` reads it with no deadline: a server that spun
  without growing (so the 1 GB watch never kills it) would hang the test
  rather than fail it. Its time bound is 60 s since the Linux x86_64 CI
  runner took 11 and 14 s where an M-series Mac takes 2 s.
- Inline `source` is one document per `base_dir` (`inline.scad`), so
  inline calls take turns rather than running in parallel, and while
  one runs it shadows a real `inline.scad` in that directory. (7c)
- `notifications/cancelled` does not reach an MCP `snapshot` call: the
  tool calls the session directly, not through the server's request
  table that `$/cancelRequest` looks up. The end of input still stops
  it (`Session::cancel_all`). (H4)
- `neoscad serve --socket PATH` in a shared directory binds and then
  makes the socket 0600, a short window harmless under the default
  umask 022 (connecting needs write permission); binding under a
  tightened umask needs nix's `fs` feature. (H4)
- The snapshot sheet's header line runs under the legend at the MCP
  default size (768 px) when `issues` adds check counts. (7c)
- Tool-description token counts are estimates from byte counts (6,059
  bytes of compact JSON as a client receives the list, 5,488 as the
  test measures it); no tokenizer was run. The test's 5,500-byte guard
  has 12 bytes to spare. (7c, H4, CAD pilot fix)
- `crates/cli/tests/mcp.rs`'s `the_end_of_input_cancels_running_calls_and_exits`
  fails at 583b466 and at the docs-only commit before it (and with the CAD
  pilot fix) when run
  alone or with the other MCP tests: the server exits after about
  2.47 s, the 2 s grace, rather than cancelling the evaluation within
  the 1.5 s the test allows. It passed once inside a full `cargo test`.
  (found during the CAD pilot fix)
- The agent eval has so far been run once per task and condition
  (`docs/agent-eval.md`); a real comparison needs several runs per
  task and condition, more tasks, and a second model. (7c)
- The agent eval's graders can only express geometry through `@expect`
  on derived solids (intersections with probes plus a 1 mm³ marker, so
  "no overlap" measures 1 instead of failing as an empty model). An
  empty model now measures volume 0 (T2 audit fixes), so the marker is
  no longer needed; an `@expect volume-between` would still make them
  plainer. (7c)

- The desktop apps' agent link (`docs/agent-bridge.md`, "Desktop apps")
  has its UI in all three apps (`apple/App/Agents`,
  `crates/linux-app/src/app/agent.rs`, `windows/NeoSCAD.Host/Agent*.cs`).
  Open points:
  - That a host `neoscad mcp` reaches a socket the Flatpak'd app makes
    in `$XDG_RUNTIME_DIR/app/org.neoscad.NeoSCAD/` is unverified (no
    Linux machine with Flatpak was used), and so is `FLATPAK_ID` being
    set in the sandbox (`/.flatpak-info` is checked too).
  - Windows: `transport::Closer` now cancels a pipe's pending I/O
    (`CancelIoEx`, `transport/win.rs`'s `cancellable`) on both the
    client's pipe and interprocess's server halves, so Disconnect and
    stop no longer rely on the agent closing its end after `bye`. Written
    on macOS and checked only with `cargo clippy --target
    x86_64-pc-windows-msvc`; the transport tests that exercise it
    (`a_line_goes_both_ways_and_the_closer_ends_a_blocked_read`,
    `the_clients_closer_ends_its_blocked_read`) first run on Windows CI.
    Not done: the closer leaves the handle open until both halves drop
    (closing it under another thread's operation could cancel a reused
    handle's I/O), and a dropped server half that still has unflushed
    data goes to interprocess's "limbo" thread, which waits for the
    client to read it.
  - The app's version check and the editor's `agentEdit` meet on two
    threads: the host compares its revision on the main thread, but a
    keystroke still on its way from the web view is not counted yet.
    Done in all three apps: `agentEdit` takes the editor version the
    app checked against and refuses (`stale: true`) when the editor has
    moved past it, and each document keeps its own revision counter
    (`SCADDocument.agentRevision`, `DocumentSession.Revision`,
    `Document::revision`).
  - macOS: the toolbar control was only checked in screenshots of an
    inactive window (drawn by `cacheDisplay`, since the build machine
    gives no screen-recording permission), where AppKit dims toolbar
    content; its look in a key window is unchecked.
  - Versions restart with the app's counters: after an app restart a
    `version` read before it can equal the new document's. The document
    gets a new number, `{old, new}` edits match the current text anyway,
    but an `at` edit with no `document` could land on shifted text.
  - Whether Cursor, VS Code and Claude Desktop honour
    `notifications/tools/list_changed` is unchecked. A client that
    ignores it sees the app's tools only in a session that starts while
    the app is connected.
  - The model tools on the app's document evaluate it in the command
    line's own session, a second time; routing them to the app's warm
    core needs the app and the command line to be one version.

## Tooling: fmt, test, docs
- `mcp::bridge::tests::a_request_waits_briefly_for_a_tab_to_connect`
  failed once on CI's Linux aarch64 runner (19ad621; it passed on
  re-run): the request got "no NeoSCAD web page is connected", although
  the fake tab connects after 200 ms and the grace is 5 s. Either the
  runner stalled past `TAB_GRACE`, or the tab connected and its writer
  closed before the send. If it recurs, log which of `wait_for_socket`
  and `tab.out.send` returned the error.
- The builtin index's footer (`crates/docs/src/lib.rs`, "--in FILE for
  a file's own modules and functions") still names the command line's
  flag when the MCP `docs` tool returns the index; the not-found hint
  names the caller's argument (`DocsRequest::file_arg`) but the index
  does not. (T3 audit fix)
- `NOTICE` (the SGI Free Software License B for the libtess2 port) ships
  beside the binaries: the CLI tarball and the DMG's `Licenses` folder
  (`scripts/release/licenses.sh`, from `scripts/apple/release.sh`) and
  the Windows installer, whose workflow checks for it. Inside
  `NeoSCAD.app` itself there is still only the editor's
  `THIRD-PARTY-LICENSES.txt`, so an app copied out of the DMG alone
  leaves `NOTICE` behind. (libtess2 port)
- The libtess2 port was checked against an oracle that is not in the
  repository: OpenSCAD's `src/ext/libtess2/Source/*.c` built with Apple
  clang `-O3 -DNDEBUG`, and a C++17 harness (`-O3`, Homebrew's Boost and
  Eigen) that includes `GeometryUtils.cc`'s tessellation code verbatim
  (from `stdAlloc` to the end of `tessellatePolygonWithHoles`), replays
  `PolySetUtils::tessellate_faces`, flags reads of `vindices[TESS_UNDEF]`,
  and `_exit`s on a signal or after 5 s per case. Cases where upstream
  crashes, hangs, or answers differently under `MallocPreScribble` are
  excluded. The expected triangles in `crates/geom/src/libtess2/tests.rs`
  came from it. Checking it in (under `scripts/`, built on demand) would
  let the next change to `libtess2` rerun the comparison instead of
  rebuilding it. (libtess2 port)
- `session::diag`'s "did you mean" pools are hand-copied lists of
  OpenSCAD's builtin modules and functions; `eval::builtins()` (7b-2)
  now lists the evaluator's own tables and could replace them. (7b-2)
- `neoscad fmt` keeps what OpenSCAD's customizer reads at the top of a
  file (before the first `{`): there an assignment with a trailing `//`
  comment is never wrapped (it can run past the width), assignments
  sharing a line keep sharing it, and indented `//` comments keep their
  indent. A narrower rule (only lines whose annotations would change)
  would format more of those headers. (7b-2)
- Formatter layout limits: binary chains break all or nothing (no
  filling); only a lone vector argument hugs its parentheses (no
  "last argument" hugging of a trailing vector or function literal); a
  `//` comment inside an expression ends the line there, and block
  comments are kept verbatim, not re-indented; blank lines are kept
  between list items as between statements. (7b-2)
- `neoscad fmt` refuses files that need `--enable` to parse (the
  unicode-identifier tests); it has no `--enable`. It rewrites files in
  place (no temporary file and rename) and never goes through a running
  server. (7b-2)
- `neoscad test` runs in-process; unlike `check` and `measure` it does
  not hand its work to a running `neoscad serve` (a `cli.test`), so a
  command-line run starts cold. Each test re-parses its file (a test's
  program is changed, so it skips the parse cache; included files still
  come from the lex cache). (7b-2)
- `@expect parts` checks that the named parts exist, not that they are
  the only ones; there are no expectations on echo output (tests use
  `assert()`), on 2D contour counts, or on `measure --between`
  distances. (7b-2)
- `neoscad docs --in` prints a user definition's parameters as the
  `.ast` dump does (`r = 1`), builtins as written in `builtins.toml`
  (`r=1`); it follows `use`d libraries one level, not the libraries they
  use. Experimental builtins (`roof`, `textmetrics`, ...) have no
  entries, only a note naming the `--enable` flag that turns them on
  (or that neoscad lacks it). (7b-2)
- `neoscad fmt` on deeply nested source needs memory with the square of
  the depth, from the indentation. That is the output's own size, so it
  cannot be linear: at 4,990 levels (the parser's limit) the output is
  96 MB for blocks and 240 MB for `translate()` chains, each line
  indented by its level, and formatting peaked at 358 MB and 859 MB
  (release, macOS arm64). Where the rest of the peak (3.6 times the
  output) goes was not measured; `fmt` also parses its output again to
  check the program is unchanged. It runs on the main thread (8 MiB), not the
  evaluator's: at the limit that held in a release build (a 2 MiB thread
  overflowed), but without the limit it overflowed between 8,000 and
  10,000 levels of `(`.

## Fonts
- Fontconfig's system configuration is not consulted, so names the
  nightly resolves to installed system fonts render in the matching
  Liberation font instead (on this Mac `Arial`, `Helvetica`, `Courier
  New` and `Times New Roman` are system fonts for the nightly; here they
  are the metric-compatible Liberation Sans, Mono and Serif). No test
  depends on it. (5e)
- Fixed (post-0.2.0): `use <font.ttf>` (or `.otf`, any case) is no
  longer loaded as a library by `lang::deps` or the session's loader
  (it stays in `uses`, where hosts register fonts from), and a missing
  font prints OpenSCAD's `ERROR: Can't read font with path '...'` after
  the `Can't open library` warning (`font-not-found`;
  `SourceFile::registerUse`). Byte-identical to the nightly's `.echo`
  and stderr. (5e)
- The font-name matcher (`crates/text/src/pattern.rs`) ranks on charset,
  family, style, slant, weight and width. It leaves out fontconfig's
  language coverage and every value after the first for weight, slant and
  width, and matches a weight range by its midpoint. Every font name in
  the test suite resolves as in the nightly. (5e)
- `textmetrics()` and `fontmetrics()` (`--enable textmetrics`) do not
  print OpenSCAD's `FONT-WARNING: Could not parse font '...'` line for an
  unparseable font name (the evaluator's messages have no severity for
  that message group); its "Can't get font" warning does print. The
  names `fontmetrics()` reports follow FreeType's `tt_face_get_name`
  (English Windows names, else Apple, else Unicode; non-ASCII as `?`),
  checked on the Liberation fonts only. `--enable textmetrics` also marks
  the statements that call them as not reusable across edits (fonts are
  files no fingerprint covers). (experimental features)
- Cubic glyph segments (CFF fonts) are flattened with `powf(3.0)` like
  the C++ `std::pow`; that matches on macOS because both call the system
  libm, but a WASM libm may round a cube differently in the last bit. No
  test font is CFF. (5e)

## Constrained sketches
- The apps drop `INFO:` diagnostics (an under-constrained sketch):
  `client::types::diagnostic_of` knows only error, warning and
  deprecated, and `client::Severity`/the ffi's have no info variant. Stage
  4 should add one, with the editor marker and the LSP's
  `DiagnosticSeverity::Information`.
- The diagnosis after a solve (`Solver::diagnose`'s rank analysis,
  O(equations × n²)) does not poll the interrupt; only the iterations
  and `Sketch::completion` do. Under `Limits::sketch_unknowns` (5,000)
  it is bounded, but at that size it is tens of seconds that a
  cancellation cannot cut short.
- Solve time is dense O(n³) per iteration: a regular polygon drawn
  roughly (angles, equal sides, one length; free to rotate) of 100
  unknowns takes 24 ms, 200 take 190 ms and 400 take 1.6 s (about 40
  iterations each, release build, Apple silicon). Corpus sketches (10–40
  unknowns) take 20–330 µs. A rejected
  Levenberg–Marquardt step recomputes the Jacobian and refactors the
  whole stacked [J; √μI]. Reusing J across rejected steps, factoring J
  once per iteration and folding in each μ with Givens rotations
  (MINPACK's `lmpar`), a better initial μ (iteration counts look high),
  or sparse QR would each help.
- An under-constrained arc whose radius grows keeps its end points near
  where they were drawn, so its sweep can cross 180°, reported as a flip
  (FreeCAD's `testCircleLineTangentOriented` case allows it, see
  `crates/sketch/tests/corpus/translate.py`). FreeCAD keeps an arc's
  angles as unknowns and so keeps its extent. Weighting the minimal-norm
  steps, or arc parameters in angle form, would avoid it.
- From very rough drawings (noise of 10% of the sketch size) the solver
  sometimes lands on another branch and reports the flip, where
  SolveSpace stays: in `scripts/sketch-oracle.py --generate 300`, 3–9 of
  roughly 170 fully constrained sketches per seed end away from the
  generating truth (SolveSpace: 7–15), and 9–14 of 300 report a flip.
  Continuation only moves dimensions; it could also relax the geometric
  constraints from the drawing.
- The differential oracle cannot check some paths, because SolveSpace
  (the `slvs` 3.2 wheel) has no equivalent: tangency away from a shared
  end point, circle tangents, sweeps, symmetry about a point, signed
  axis distances (emulated with a helper point), line–line distance.
  The `slvs` 3.2 wheel also predates SolveSpace's fix for issue #1354,
  so it reports `perpendicular_4m` (that issue's own test file) as not
  converging. A newer wheel, or the solver built from source, would
  remove that noise.
- `distance(l1, l2, d)` between lines measures from `l2`'s start and
  does not make the lines parallel (`Constraint::Distance`). Settled in
  stage 2: the language's `distance()` adds the parallel constraint
  (`docs/language-extensions.md`, section 11.1); the solver crate's
  constraint stays as it is, for hosts that want FreeCAD's form.
- Exactify snaps coincident, horizontal and vertical classes, fixed
  coordinates and circle radii; midpoints and symmetric points are not
  snapped to their exact formulas.
- The under-constrained message lists every coordinate with a mobility
  over 0.01, so it can name more coordinates than there are free degrees
  of freedom (a point sliding along a 45° line is listed in x and in y).
  The suggested constraints (stage 3) are exactly as many as needed.
- Suggested constraints (stage 3, `docs/language-extensions.md` section
  11.2) name only entities the outermost sketch body names: a sketch
  built by a helper module gets advice, not an edit, and so does a
  sketch over 400 unknowns. Inserting into the helper's own body would
  constrain every use of it.
- One flip often shows at several places (a triangle drawn the other way
  round turns all three corners): one warning each, with the same "pin
  the drawing" edit. Grouping them would read better.
- The angle a suggested `angle()` states is measured with `io::trig`'s
  `atan2`, whose last bit may differ by platform; it only reaches the
  hint's text, at 6 digits.
- `sketch-self-intersection` tests the tessellated loops, so two arcs
  that nearly touch can cross only between their chords (or the
  reverse), and curves that overlap along a stretch, or touch, are not
  reported. The sweep stops after 20 million segment pairs.
- An entity made in a statement's arguments (`fix(point([1, 2]))`) has no
  name: it prints as `<sketch point>` and is `point #3` in messages.
  Stage 3 names entities by the variable that holds them, list elements
  included (`pts[0]`).
- Fillets and chamfers cut corners between two lines only; line–arc and
  arc–arc fillets are stage 7.
- The goldens in `conformance/extensions/sketch` run as a cargo test
  (`crates/session/tests/sketch.rs`), not as `conformance run --tier ext`
  (section 9 of the design), and their `.csg` exports have been rendered
  by the stock nightly by hand only; a tier in the harness would compare
  those meshes on every run.
- LSP completion leaves the sketch vocabulary out everywhere, until
  stage 4 offers it inside sketch bodies only; hover shows its docs.
- Moving the crate to its own repository needs its own CI (the wasm32
  build and the cross-platform digest now in `crates/wasm-check`), and
  the corpus (GPL/LGPL-derived) stays in NeoSCAD or goes to a separate
  test-data repository.

## Determinism
- Console output of two reference models varies between runs of one
  build (seen at `6d73727` and after): `svg/id-layer-selection-test.scad`
  prints a different set of `import() filter ... did not match` warnings
  each run, and `misc/empty-shape-tests.scad` sometimes omits its
  `Unsupported file format` error for `import("")`. The exported files are
  identical. Probably diagnostics from parallel child evaluation being
  deduplicated or collected in scheduling order. (slow-cases §2 sweep)
  Also `BOSL2/examples_x/shapes2d__122.scad`: its render summary says
  "Geometries in cache: 46" in about four runs of five and 45 otherwise
  (seen at `3b977e7`; the SVG is identical). (P1 sweep)
- The libtess2 port's broken-mesh path (`arena.rs`) has no known input
  where multiply-adds are not fused (x86_64, wasm32): 3 million random
  polygons searched under Rosetta broke none, so
  `a_broken_polygon_does_not_affect_the_next` sets the flag by hand there
  and only aarch64 runs a real break end to end. A longer search (or
  upstream libtess2's own crash reports) could supply an x86_64 input.
- Tests whose expected numbers came from arm64 runs fail on x86_64 when
  those numbers sit on a rounding boundary (`textmetrics` advance,
  `stl-precision` counts); the Linux x86_64 CI job is the only place that
  shows it. Where the nightly has an x86_64 slice, take the x86_64
  expectation from it (`arch -x86_64`), as `tests/experimental.rs` does.

## WASM
- Superseded by the heap evaluator for user recursion, which now stops at
  the counted depth limit on wasm32 too (`scripts/wasm-check.sh --depths`:
  99,999 for both recursions below); the budget only meets the shapes
  that still recurse natively. The rest of this entry is history.
  Recursion on wasm32 stopped at a frame budget calibrated for V8's default
  stack in node 18 (`eval::recursion`): function depth 498 and module
  depth 249 for the simplest recursions, against 110,361 and 16,842
  natively and the nightly's 9,192 and 7,052. Without the budget V8
  overflows at 1,076 and 527. Raising it needs smaller wasm frames: per
  level, rendering's walk over the node tree costs about four times an
  expression's stack, and list comprehensions twice. Only node 18 was
  measured; browsers (and workers, which may have less stack) are
  unverified. (H2)

  The rendering-walk part is stale. Since heap-evaluator stage 0 no walk
  over the finished tree recurses per level, and V8's overflow depths did
  not move (node 22, no budget: module 1,611 and function 1,712 levels,
  before and after), so a module level's stack is instantiation's. Per
  level a module recursion costs about what a function recursion does
  (1,712 / 1,611 = 1.06), yet the default weights charge it 8 frames
  against 4. `STATEMENT_FRAMES` could be halved for node and other wasm
  hosts; browsers set their own weights from the worker's probe.
- The frame budget's calibration in `crates/eval/src/recursion.rs`
  (budget depths at most 63% of where V8 overflows) is stale: at
  `83aa1af` `module-children` reaches 206 of V8's 214 and `function-lc`
  199 of 326 (`scripts/wasm-check.sh --depths --all-programs`, with and
  without `--frames=4000000000`). After O4, V8 overflows `function-lc`
  at 353 and `module-children` still at 214. (O4)
- A wasm32 build must be linked with `-C link-arg=-zstack-size=8388608`
  (`eval::recursion::WASM_STACK_SIZE`; `crates/wasm-check/build.rs` does
  this). With rustc's default 1 MiB, recursion stops earlier, still
  cleanly. The release `wasm_check.wasm` is 38 MB, of which all but
  9.0 MB are DWARF line tables (the release profile keeps them); 4.3 MB
  of the rest is the bundled fonts and MCAD. (H2)
- Rust's wasm32 maths functions differ from macOS libm in the last bit
  (engine milestone audit, finding 6.5), so WASM output is not
  byte-identical to native. Decide whether to accept that or use one libm
  everywhere. (H2)

- Fixed (post-0.2.0): clippy for wasm32 failed on the unread `sizes`
  field (`crates/eval/src/dump.rs`), a dead `small_axes_clip` without
  the `gpu` feature (`crates/render/src/overlay.rs`) and a needless
  `return` (`crates/session/src/parse.rs`). `scripts/wasm-check.sh` now
  runs `clippy -D warnings` for wasm32 on each package it builds
  (`neoscad-wasm-check`, `-render`, `-web`, `-web-view`), so CI's wasm
  job catches the next one.

## Web demo

- **Done: deep user recursion ends cleanly in WebKit.** JavaScriptCore's
  baseline wasm tier (BBQ) gives every frame of the evaluator about a
  kilobyte however little it holds, and a WebKit worker on macOS has
  about 512 KiB of stack (the jsc shell at `--maxPerThreadStackUsage=
  524288` reproduces its depths), so the V8-calibrated budget let `m(40)`
  overflow. Now:
  - The web worker probes its engine at start-up (`crates/web/js/
    worker.js`, 30-60 ms): throwaway instances recurse through a
    function, a list comprehension, a `children()` chain and a module
    through `translate` until the stack overflows, each run counting one
    kind of frame, and the worker sets per-kind weights
    (`eval::recursion::FrameWeights`, wasm32-only process-wide setters)
    under a budget of 1,000,000 so each kind stops at half the depth that
    overflowed, never deeper than the defaults. A geometry module's
    children got a weight of their own (0 by default), since in JSC a
    `translate()` level costs about twice a user module's.
  - Fewer wasm frames per level: `instantiate_scope`,
    `instantiate_children`, `with_children`, `eval_element`,
    `for_each_reg` and `iterate_over`'s closure are inlined, and two
    `and_then` closures on the statement path are `match`es. Per level in
    JSC: a `children()` chain 3+ frames to 2, a module through `translate`
    10 to 4, a comprehension 12 to 8, a function 3 (unchanged).
  - Depths now (first n that stops with the recursion error; module
    through `translate`, plain module, `children()`, function,
    comprehension): WebKit 40/100/80/80/30, Chromium 200/300/300/500/150,
    Firefox 200/300/300/500/200. All 8 /try examples render in all three;
    the BOSL2 gearbox needs 77% of WebKit's calibrated budget.
  Since then the heap evaluator made recursion depth a count: function,
  module, comprehension and `children()` recursion reach 99,999 levels
  in all three browsers, and the probe and its per-kind weights are gone
  from `crates/web`. What still recurses natively is held to the frame
  budget with a constant weight (`eval::recursion::HEAP_LOOP_FRAMES`)
  sized for WebKit's stack; printing a nested value holds no native
  stack per level any more, and stops at a counted depth.
  Source nesting is now bounded by a weighted depth
  (`lang::syntax::parser::nesting_weight`, `NESTING_LIMIT`: 2,590 on
  wasm32), each kind weighed by the stack a level of it took in WebKit's
  worst case (a worker part-way through tiering up), and every kind
  stops with at least 30% to spare: in `wasm-check.sh --depths` and the
  browsers, 55 levels of `[` parse, 68 of `(`, 46 of `max(`, 119 of
  `translate()`, 120 of `else if` and 252 of `{`, and one more ends in
  "Parser error: memory exhausted". Measured October 2026 in Playwright's
  WebKit on macOS arm64 with the limit lifted (the table is in
  `nesting_weight`'s comment). OpenSCAD's
  `issue4172-echo-vector-stack-exhaust.scad` (144 levels of `[`) is
  refused in browsers, where WebKit would overflow on it; natively it is
  unchanged.
  - Done: `let`, `assert` and `echo` expressions keep 30% too. The
    native evaluator recursed into each one's body (`eval`, `eval_expr`,
    `eval_cold` a link), so 123 links of `echo(assert(true) ... 1)`
    overflowed WebKit, and they weighed 26 with 20% to spare. Now
    `Evaluator::eval_chain` walks a chain of them in a loop, and
    `eval_expr` runs `assert` and `echo` links itself: a chain reaches
    372 levels or more, as `{` does, and how they nest with other kinds
    decides their weights, `assert` and `echo` 12 and `let` 17 (the
    lowest that keeps 30% for a comprehension's `let`, which must weigh
    the same; `let` alone needs 15). BOSL2's `nurbs.scad` (66 chained
    `assert`s) weighs 1,311, down from 2,289; MCAD's `bitmap.scad`
    (2,005) is now the deepest file by weight.
  - Done: a `?:` whose branch is another `?:` is walked in a loop
    (`Evaluator::ternary_chain`), so a chain of them costs no native
    frame a level. Output is unchanged (4,028 of 4,029 OpenSCAD and
    BOSL2 files echo byte for byte; the other differs only in the bundled
    library's path, which follows the binary). Taking every `?:`'s
    branch in `eval_expr`'s own loop instead made BOSL2's isosurface
    benchmark 7-8% slower. `TernaryExpr` keeps its weight, 17: with the
    limit lifted, WebKit's worst case for a run is still 220 levels of
    `x ? ` (222 before), and the parameters request, which does not
    evaluate, overflows at 237, so evaluation was not what overflowed
    first. With other kinds: 119 levels alternating with `let` (117
    before), 144 with `assert`, 146 with `echo`, 61 of `[x ? ` (45 + 17
    = 62 needs 60.7), 72 of `(x ? `, 60 of `max(x ? `. Left: lowering its
    weight needs the stages before evaluation to go deeper on `?:` too.
- **Done: the preview's product booleans run under the limits.**
  `geom::csg::product_meshes_until` checks a `geom::csg::Stop` (the
  request's interrupt flag and limits guard) before every kernel
  operation, counts the leaves and partial unions it holds against the
  memory limit, and stops with the limit recorded on the guard.
  `session::Rendered::stop` carries the request's flag and guard to the
  host, and `client::run_scene` (web, the macOS app, the Linux app) draws
  the preview under it: a limit is a `Failed` error with the limit's
  message and hint, a cancel is `Cancelled`. In the web core, the Menger
  example at depth 4 under a 3 s limit now stops after 4.4 s (it ran
  about 28 s). Left: once `Session::render` returns, a newer request on
  the document no longer sets the flag (only the host's own
  `Run::interrupt` and the limits stop the preview), so a superseding
  edit in the macOS app still waits for the old preview; `neoscad`'s
  PNG export (`cli/src/png.rs`) and `session`'s snapshots
  (`session/src/snapshot.rs`) still call the unlimited
  `render::preview::scene`. A single kernel operation is interrupted
  too: `geom::manifold_geom::kernel_token` gives manifold-rust a
  `CancelToken` over the request's flag (`from_flag`, `with_check`;
  `vendor/patches/manifold-rust`).
- **Done: every e2e spec runs in Chromium, Firefox and WebKit**
  (`web/playwright.config.js`; Playwright 1.63: Chromium 1243, Firefox
  155, WebKit 2359). On macOS arm64 against a release bundle all pass:
  37 Chromium, 35 Firefox, 36 WebKit, 2 phone, with skips only for what
  cannot apply (the agent's Local Network Access test is Chromium's; the
  WebGPU splitter test in Firefox). WebKit's view is WebGPU (Apple
  adapter); Playwright's Firefox has `navigator.gpu` but "WebGPU is
  disabled by blocklist", also with `dom.webgpu.enabled` and
  `gfx.webgpu.ignore-blocklist`, so Firefox runs the page's fallback to
  the WebGL build, which is what it tests. `real.spec.js` now checks deep
  recursion through the page with a cold worker in each engine (function,
  module, comprehension and `children()` reach 99,999 levels, and the
  100,000th is the recursion error with no restart), and the lazy fonts
  (below). CI's new `web` job (`.github/workflows/ci.yml`, Playwright's
  container image) runs the unit tests and the suite against the mock
  build in all three, about a minute of tests; with the mock,
  `real.spec.js` and `agent.spec.js` skip, as they need the wasm core and
  `neoscad`. Checked in that image locally: 56 passed three times over.
  The page needed no fix. One test flake did show: at 0 ms between keys,
  Linux Chromium typed a key behind later ones about once in ten runs
  (`share.spec.js`: "// shred on purposea"); the spec now types with
  20 ms between keys after `settleCursor`. Left: whether that is
  CodeMirror's input handling under synthetic keys or something the page
  does on the first edit is not known. Firefox's WebGPU path is untested
  until Playwright's Firefox gets an adapter.
- **Done: the core is 2.34 MB gzipped** (6,865,928 bytes raw), down from
  4.61 MB (4,612,645 bytes gzipped at `-9`; it had grown since the 4.41
  MB this entry first said), under the 4 MB target. Its data section was
  4.8 MB raw, and the twelve Liberation fonts were 2.28 MB of the gzipped
  module. They are now a lazy `fonts.tar.gz` beside the bundle (2.25 MB,
  `scripts/web/build.sh`), fetched like `bosl2.tar.gz`: before a run
  whose source calls `text(`, `textmetrics(` or `fontmetrics(`, or when
  the core reports `fontsWanted` (text drawn inside a library: the
  `text::FontDb::on_index` hook in `crates/web`), after which the page
  runs again. Adding files under `/neoscad/fonts` clears the session's
  caches. Models without text never download them. No evaluation speed
  changes; the first text model waits for the fetch. What else was
  measured (`gzip -9` of the module; speed as medians of 9 interleaved
  cold runs of a node bench of 15 models, with the machine at load
  15-35):
  - `wasm-opt -O3` (binaryen 132): -20 KB (0.4%); `-Oz` -18 KB, `-O2`
    -11 KB. Not installed natively; `build-core.sh` already runs it when
    it is on `PATH` (or `WASM_OPT=docker`).
  - `opt-level = "z"` for the text stack (harfrust, skrifa, read-fonts,
    font-types), `neoscad-io`, quick-xml, zip, png, lsp, fmt and docs:
    -131 KB, but ASCII STL import 2.7x slower, 3MF import 1.8x, text
    +8%. `"s"` for the text stack alone: -33 KB, text +3%. `"z"` for lsp,
    fmt and docs alone: -23 KB, LSP queries +28% and formatting +43%. None
    applied.
  - Already minimal: no debug info, symbols stripped (the name section is
    68 bytes), `panic = "abort"`, fat LTO, one codegen unit.
  - Brotli would be 3.09 MB against gzip's 4.61 MB for the old module, but
    GitHub Pages serves `.wasm` gzipped on the fly (`Content-Encoding:
    gzip`, about `gzip -6`: 4,657,759 bytes for the live 10.9 MB module)
    and never brotli, even to `Accept-Encoding: br` alone (checked
    October 2026 with `curl -sI` on neoscad.org/try). The `.tar.gz`
    archives are served as they are.
  The WebGPU viewer is 190 KB and the lazy WebGL build 1.10 MB gzipped.
  Left: of the remaining code, `core::slice::sort` instantiations are
  about 400 KB raw and rayon's about 420 KB (on single-threaded wasm32);
  those are in `geom` and `render`.
- **One boolean overshoots the memory limit by about 700 MiB.**
  `crates/web/test/run.mjs`'s "one boolean past the memory limit" test
  (restored in October 2026; `34d69e2` had dropped it) passes, but the
  Menger depth-5 render stops at "the engine uses 1,706 MiB of memory,
  over the memory limit of 1,024 MiB (measured)" with wasm memory at
  1,889 MiB, close to the test's 2 GiB guard. The kernel's check
  (`geom::manifold_geom::kernel_token`) runs too rarely inside that
  union to stop it near the limit.
- **The canvas fallback draws only colour-writing draws**: a preview's
  image-space CSG primitives (subtracted and intersected shapes) are left
  out, so previews of differences show only what is kept. It shows only
  when neither WebGPU nor WebGL2 starts.
- **The heavy example uses 937 MB of wasm memory** (`stats` after
  preview and render), close to the worker's 1 GiB limit; the page
  respawns the worker when leaving it. Its render reports "this
  polyhedron is not closed: 238364 edges are used by only one face" in
  BOSL2's `vnf.scad` line 1615 (`session::orient`, a NeoSCAD diagnostic
  that is not in the CLI's console); check whether the app reports it
  natively too and whether it is a false positive on BOSL2's isosurface
  VNF. wasm gives 269,960 triangles against the CLI's 269,948 (wasm32
  maths, see "WASM").
- ~~**A flaky e2e:** "examples switch, and edits persist" lost a typed
  space~~ Fixed: under 6x CPU throttling a keystroke right after the click
  and Cmd/Ctrl-Home landed at a stale cursor about once in 20 runs
  ("/ edited" with a "/" in front, or the original "//edited"). The
  editor's own `changes` messages show CodeMirror inserting at the stale
  position with no page call in between, so the bridge is not losing keys;
  the race is CodeMirror's reading of the DOM selection, and it needs the
  next key within milliseconds of the cursor key under heavy load (it
  persisted at 40 ms between keys only with the throttle). The specs now
  wait for the cursor and two frames (`settleCursor` in `web/e2e/helpers.js`)
  before typing: 0 of 40 throttled runs lost a key, and the full suite
  passed 10 runs in a row. If users report lost keys, look at CodeMirror's
  `DOMObserver` selection reads after `view.setState`.
- **The threaded ring's preview takes 3.8 s to show for 1.8 s of engine
  time** (`timings.totalMs`); the render shows in 3.8 s for 3.7 s. The
  difference is outside `timings`: packing the preview scene (image-CSG
  products) and the reply's JSON and transfer. Measure it in the worker.
- **Echo lines have no source location** (OpenSCAD prints none), so
  clicking one does nothing; the console-jump e2e uses a warning.
- **No crash hook in the core:** the crash e2e patches the glue
  (`core/neoscad_web.js`) so a run throws the RuntimeError a trap throws.
  A real panic or OOM trap in a browser is not exercised.
- **`build.sh` packages, it does not build** the core and viewers
  (`dist/web-core`, `dist/web-view/{webgpu,webgl}`), and it does not
  check that they are newer than the Rust source.
- **THIRD-PARTY-LICENSES.txt lists build-time crates too**
  (`scripts/web/rust-licenses.mjs` walks normal dependencies, which
  includes proc macros such as `proc-macro2` and `quote` that are not in
  the modules): over-inclusive, not missing anything.
- **Language requests queue behind a run** in the single worker (hover
  and completion wait for a long render); a separate LSP worker is in the
  plan's deferred list.
- **wasm-opt** was run through a Docker wrapper (binaryen is not
  installed here); `build-view.sh` takes only a binary in `WASM_OPT`.
- The render crate's `overlay::small_axes_clip` is dead code in the
  wasm32 builds (a compiler warning in `build-core.sh`'s output).
- **The agent bridge in real Safari and Edge is unverified**
  (`docs/agent-bridge.md`, "Browsers"): Playwright's WebKit 26.6 and
  Chrome 154 were tested as neoscad.org, but not Safari 27 (automating it
  needs `safaridriver --enable`) or Edge (not installed). Check the relay
  window in Safari, and whether Chrome's LNA prompt text matches the
  dialog's hint ("reach apps or devices on this computer"). The https
  emulation is a scratch script; making it an opt-in e2e (a self-signed
  cert and a CONNECT proxy in `serve.mjs`) would keep the matrix checked.
- **The bridge's link changes with every `neoscad mcp` start** (a new
  port and token), so a bookmarked or reloaded link from an earlier
  session fails and the user asks the agent again. A `--browser-port` and
  a token kept per user (in the user's config directory) would make one
  link last, at the cost of a long-lived key on disk.
- **The model tools on the page's text run natively**, with the files
  the CLI can read. A page example that uses BOSL2 evaluates only if
  BOSL2 is on the native library path, while the page fetches its own
  copy. Shipping BOSL2 with the CLI (as MCAD is), or falling back to the
  page's own engine for those runs, would close the gap.
- **The agent's edits and the user's typing race only at version
  granularity:** an edit made on version N is refused once the user has
  typed. Rebasing a small edit over concurrent typing (as the editor's
  collaborative extensions do) would refuse less, at the cost of edits
  the agent did not see.

## Windows
- `windows-installer.yml`'s `winget` job (fill the winget manifests from
  the attached MSIs, attach `neoscad-winget-manifests.tar.gz`) has not
  run yet; `fill-manifests.sh --winget` was checked locally on v0.4.2's
  MSIs only. The filled manifests are not run through `winget validate`,
  which needs a Windows machine with winget.

Found fixing the first CI run's Windows failures, which removed the
verbatim `\\?\` form (`lang::paths`) and made relative paths in messages,
`-d` files and doc indexes `/`-separated.

- 8.3 short names (`C:\Users\RUNNER~1`) are not expanded. A working
  directory given in short form makes files named relative to the input
  (imports) print short while included files (canonicalised) print long,
  so a `-d` file can name one directory both ways; OpenSCAD does the same
  (`lookup_file` uses `fs::absolute`, not `canonical`). `neoscad mcp`'s
  roots are canonical, so an absolute path a client spells in short form
  is refused (`roots::resolve` follows links but does not expand short
  names).
- `-m` runs its command through `sh -c` on every host; OpenSCAD's
  `system()` uses `cmd.exe` on Windows. Without `sh` on `PATH` (Git for
  Windows puts one there) `-m` fails, and `tests/flags.rs` skips its `-m`
  check.
- The language server refuses `file://server/share/...` (UNC) URIs, on
  Windows too.
- `cargo test -p neoscad-cli -p neoscad-lang` is only run on Windows by
  CI; the other crates' tests (`session`, `lsp`, `eval`) are not run there.
- `neoscad mcp`'s `roots::resolve` makes a verbatim path and a verbatim
  symlink target plain (Rust's `read_link` answers `\\?\` for absolute
  targets), so both are judged like their plain spelling. The path case
  is in `roots_refuse_escapes`; the symlink case has no Windows test
  (creating a symlink there needs Developer Mode or an elevated token).
- `crates/cli/tests/memory.rs` runs on Windows but reads no memory there
  (it uses `/proc` or `ps`), so its resident bounds and its 1 GB kill
  guard do nothing on that job; only the limits' errors are checked.
  `tasklist`/`taskkill` or `GetProcessMemoryInfo` would cover it.
- The Windows app (milestones 1 and 2, `docs/windows-app.md`) has only run on CI
  runners (`.github/workflows/windows-app.yml`, screenshots and a
  `--log` as artifacts). High-DPI sharpness, the shortcuts forwarded from
  the editor, the panels' interaction (dragging a slider, picking points
  in the view, selecting a finding), the export progress dialog and its
  Cancel, and the file dialogs need a real Windows machine. The rest of its milestone 2 list is in that document ("Next"); the
  generator pin (`$BindgenRev` in `scripts/windows/build-core.ps1`, an
  open uniffi-bindgen-cs pull request) should move to a release.
- The Windows panels lack parts of the macOS ones: the measure panel's
  sections and part-to-part distances (`Measurement.section`/`between`,
  already in `crates/ffi`), the check panel's bed size and a stored
  printer (macOS keeps it in UserDefaults), 3MF's colour options in
  export, the parts toggle, and Cut/Copy/Paste in the Edit menu (the
  editor's context menu has them; a page script cannot paste without a
  user gesture). Each is host work only.
- The Windows app's update install (`docs/windows-app.md`, "Updates")
  has never run on Windows: the download, the hidden PowerShell helper
  (`-EncodedCommand`), the UAC prompt for `msiexec /qn`, the major
  upgrade replacing the running copy's files after it exits, the
  restart, and the InfoBar. Check them against an older installed MSI
  and a test-signed local feed, and whether SmartScreen says anything
  about the downloaded MSI (expected not: no Mark of the Web). A
  declined UAC prompt starts the old version again; a failed install is
  only in `install.log` beside the downloaded MSI in `%TEMP%`, and the
  app says nothing about it after the restart.
- The Windows app's AI agent UI (`docs/windows-app.md`, "AI agents") was
  built and checked off Windows only: the host and setup tests and the
  real `neoscad mcp` against the C# host ran on Linux over a Unix socket
  (`docker-test.sh --with-cli`), and the app type-checks. CI's
  `windows-app.yml` runs the same end-to-end test over a named pipe, with
  a fake editor and no 3D view. Unchecked until a Windows machine runs
  them: the XAML (the menu row's control, the `InfoBadge` and
  `ProgressRing`, the approval bar, the marks chip over the
  `SwapChainPanel` and its clear button), the dialog's look and
  scrolling at 720 px (with its client `SelectorBar`, whether five
  items fit its width, the usage `Expander` and the Segoe Fluent glyphs
  beside its items), that the preview follows an agent's edit and that "ask first"
  is in the flyout and the Help menu as the usage text says,
  `Launcher.LaunchUriAsync` with Cursor's and VS Code's install
  links (and its answer when neither is installed), the clipboard, a
  real `claude.cmd` from npm run through `claude mcp add`, `agentEdit`'s
  `expectVersion` refusal in WebView2, and a capture from a real
  `SwapChainPanel` view. Also:
  - Disconnect acts on one window's link: an agent connected to three
    windows (three processes) needs three. The consent switch acts on
    all of them, through `agents.json`.
  - The dialog does not remember a client it set up (the core has no
    `claude mcp get`), so Claude Code's card offers Add again each time,
    and a second Add asks to Replace.
  - The Help menu's two agent items are the app's only agent settings
    besides the dialog; a settings window would be their natural home.
  - File > Open and New keep the agent's marks (and so the marks chip)
    from the document before; the Linux app clears them with a new
    document, and macOS opens one in a new window. The chip clears them
    by hand meanwhile.

## Linux

- The Linux app's update notice (`docs/linux-app.md`, "Updates") offers
  a Flatpak install the new bundle to download, because the bundles
  carry no repository. Once the GPG-signed Flatpak repository exists
  (`docs/audits/auto-update.md`, next step 4), installs from it should
  be told to use `flatpak update` or Software instead (the origin is in
  `/.flatpak-info`), and bundles built with `--repo-url` would move
  bundle users there too. Until then the Flatpak has `--share=network`
  for the check alone; the owner may prefer to drop it once the
  repository makes the in-app check redundant for Flatpak users.
- The Linux update check ran only in Docker (a faked `/.flatpak-info`,
  a loopback feed, Xvfb): Download Bundle through the OpenURI portal and
  Software opening the bundle were not tried in a real Flatpak.
- The Linux app (milestone 1, `docs/linux-app.md`) was run only on
  Ubuntu 24.04 in Docker (arm64, Xvfb, Mesa lavapipe), never on a
  Wayland session or a real GPU, and never with the portal's file
  chooser: check fractional scaling (`GdkSurface::scale`), Wayland input
  and the dialogs on a GNOME desktop. The rest of its milestone 2 list
  (dmabuf view, GSettings, packaging) is in that document ("Next").
- Linux app panels (milestone 2, `docs/linux-app.md` "What milestone 2
  adds"), what the macOS panels have and these do not yet:
  - Measure: point-to-point distance and the model's volume, area and
    size only. The section (axis, offset slider, outline) and the
    distance between two parts (`Measurement::section`, `between`,
    `client::section_range`) are in the core and drawn by the overlay
    already; they need the panel's controls.
  - Check: a printer preset or check's defaults; no custom numbers (nozzle,
    walls, overhang, bed) and no "Auto" re-check after each render.
    The choice is not kept between runs of the app (GSettings, "Next" 5).
  - Export: no options dialog (3MF colour mode and material, the image
    and snapshot sizes: the view image is the view's size, the snapshot
    1024 × 1024). The core offers no AMF, so neither does the app.
  - The document itself changing on disk (another editor saving it) is
    not noticed: only the files its runs read are watched
    (`Client::run_files` leaves the document out, since the app writes
    it). A "reload?" banner, as GNOME Text Editor shows, is the fix.
  - The parts toggle (`DocumentLoop::set_parts`) has no control yet;
    only examples that need parts turn it on.
  - linux/smoke.sh drives the customizer, check, measure and Export
    Again from the keyboard, but not picking points (a click at a
    position in the view) or parameter sets; both are unit-tested
    (`linux_app::inspect`, `linux_app::customizer`).
- The Flatpak (`linux/flatpak/`, `docs/linux-app.md` "Flatpak") builds
  on x86_64 in `flatpak.yml`, which is now blocking there and a release
  publish job. Its aarch64 build (`ubuntu-24.04-arm`, native) is
  `continue-on-error` until it has passed: once it has, drop that and
  the release job's missing-aarch64 warning, so a release cannot ship
  without it. The release path (bundle naming, attestation, upload) runs
  for the first time on the v0.2.0 tag. Install a bundle on a GNOME
  desktop and try the sandbox: the file chooser portal, includes beside a model
  (`--filesystem=home`), WebKit's own sandbox inside Flatpak, and the
  GPU view through `--device=dri`. Flathub is not an option for this
  manifest: its rules forbid AI-generated manifests and AI-written
  submissions (docs/linux-app.md, "Flathub: not submitted").
- The Flatpak builds with the SDK's rust-stable (1.98.0 in 26.08), not
  the pinned 1.98.1 (`rust-toolchain.toml`), since the extension has no
  rustup. If output must match the other builds exactly, install the
  pinned toolchain from static.rust-lang.org archives in the manifest
  instead (a source per architecture).
- Language server, Linux app: go to definition is F12 only; the page's
  mouse binding is Command+click (macOS). A Ctrl+click on Linux would
  have to be added to `apple/Editor/web/src/language.js` for all hosts.
  A library viewer's own "open" of a user's file works, but nothing
  closes a viewer when the window that opened it closes (the macOS app
  shows viewers as tabs of that window).
- Its view copies each changed frame from the GPU (`view.rs`); a large
  window at 4K on a slow bus may show it. `GdkDmabufTexture` is the fix.
- The viewport stripes are fixed (`c3690f7`,
  `docs/audits/viewport-stripes.md`), but DX12 and hardware Vulkan were
  never run in the failing orbit, and the Mesa defect is unreported.
- The Linux app's AI agents (`docs/linux-app.md`, "AI agents") ran only
  in Docker (Xvfb, no window manager, no portal, no GNOME session). Not
  tried: the OpenURI portal opening `cursor://` and `vscode:` links from
  the Flatpak (in Docker, with no handler, the launch fails and the row
  falls back to its JSON, as designed); a host `neoscad mcp` reaching the
  Flatpak'd app's socket (the agent link's item above); the real
  `claude` (a stand-in that answers as Claude Code 2.1.288 does was
  used); Wayland. linux/smoke.sh's popover, dialog and Disconnect clicks
  are at fixed coordinates (a 1280 × 800 window at the screen's corner),
  so they run only with `SHOTS`. "Ask before applying" (Apply, Reject)
  was driven once by hand in Docker, not by the smoke test. The agent
  link's open point about `agentEdit` and keystrokes in flight is done
  for Linux too (`expectVersion`).

## Exact geometry (STEP)
Stage 1a of `docs/audits/exact-geometry-rust.md` is `crates/meshbrep`.
These are its leftovers. Stage 1b (the `face_id` plumbing in `geom`, the
export render with aligned tessellation, `-o x.step` behind
`--enable exact`) is the next task, not a follow-up.
- **Faceted regions cut by exact surfaces can mismatch at coarse
  tagging.** x07 (a 12-segment polyhedral sphere minus an exact skew hole)
  gives folded sliver faces at 8, 12 and 16 hole segments. The inscribed
  polygonal hole leaves facet pieces that the exact hole removes.
  `reconstruct` reports `TopologyMismatch`; the tests retry at twice the
  segments, as the binding must (32 and 40 pass). Aligned tessellation
  does not help here: the faceted planes are arbitrary. A fix in the
  crate would drop faces whose exact region is empty and re-link their
  neighbours.
- **Parameter-space curves are large on spheres.** A non-parallel circle
  on a sphere needs 257–513 cubic control points to stay within 1e-7, so
  c02 (7 faces) writes 209 KB and the 400-hole plate 2.17 MB (the spike
  wrote 1.09 MB without pcurves; OCCT's own file is 2.68 MB). Fitting with
  end derivatives, or at a higher degree, would cut this several-fold.
  The 3D B-spline edges have the same issue at a smaller scale (c12: 257
  points).
- **Closed forms not implemented:** plane–cone sections that are not
  circles (ellipse, parabola, hyperbola: B-spline today); tangency of
  plane–cone along a generator and of cone–cylinder (not classified, so
  they fall back to numeric tangent points, then arc merging). Arc merging
  is not reached by any test model; a unit test reaches it by turning
  analytic contacts off.
- **The OCCT oracle is not in CI.** `crates/meshbrep/oracle/build.sh`
  downloads cadrum's prebuilt OCCT 8.0.1 (about 140 MB) and builds
  `check`; `MESHBREP_OCCT_CHECK=… cargo test -p meshbrep --release --test
  occt` reads back the 28 models at six resolutions. A CI job (cached
  OCCT) would keep it honest.
- **The declared MSRV is unverified.** `rust-version = "1.85"` (edition
  2024's minimum), tested only on 1.98.1.
- **Publishing:** move `crates/meshbrep` to `github.com/neoscad/meshbrep`,
  publish it, and make NeoSCAD depend on the published version. The tests'
  `manifold-rust` dev-dependency then resolves to crates.io rather than
  `vendor/`.
- `measure` integrates in closed form along `u` (exact for planes,
  cylinders, cones and spheres). A torus or extrusion surface needs the
  quadrature it keeps for its own test (`inner_quadrature`).
- `validate`'s self-crossing check samples loops (8× finer to confirm),
  so two arcs closer than the sampling's sagitta can be misread. It has
  not happened in the test models.

## Structure
- **A failed release CI gate still publishes an empty release.** The
  v0.4.3 tag's first run failed `custom-ci` (the `plan-jobs` gate,
  Cargo.toml's `[workspace.metadata.dist]`), so both build jobs were
  skipped; cargo-dist's generated `host` job accepts "skipped" builds,
  so it and `announce` published v0.4.3 as Latest with only
  `dist-manifest.json`. The empty release was deleted and the tag moved
  to the fix (owner's call, 2026-10-06). Make `host` require
  `needs.custom-ci.result == 'success'` (release.yml is generated:
  either `allow-dirty = ["ci"]` and keep the edit, or a gate step that
  fails `plan` instead). Until then, a red `custom-ci` on a release run
  means: delete the release before anything else.
- The tier 3 baseline needs the pinned nightly installed as its renderer.
  CI would need it too. (5a)
- The six PDF cases need a PDF rasteriser (Ghostscript or poppler), which
  CI would need too. (5f)
- Nothing in this repository runs `vendor/manifold-rust`'s own tests.
  Upstream's CI runs them, with and without `--features parallel`, on
  everything but NeoSCAD's cancel token patch (`src/cancel.rs`). They
  cannot run from the vendored tree as it is: the `.crate` leaves out
  `src/robust/testdata/*.stl`, which other test modules `include_bytes!`,
  and the whole suite peaked over 4 GB here before it was stopped. For
  the move to 0.16.0 they were run by hand: the patched tree plus the
  test data from upstream's `f43ec62`, release build, `cancel`,
  `polygon_earclip`, `edge_op`, `par::`, `csg_tree`, `compose` and
  `quickhull`, with and without `parallel`, all passed. A script could do
  the same. One of them,
  `cancel_from_another_thread_interrupts_a_boolean_in_flight`, has a
  timing precondition (its boolean must take 20 ms) that a fast machine
  misses now and then, upstream as well.

- Printing checks the string limit per value, so separate arguments of one `echo` can each reach the limit; string-limit errors raised inside `assert`/messages carry no location (the assertion error right after does).

## Rewritten history

- `progress/` records pre-rewrite commit ids (the history went through
  `git filter-repo` before publication). `conformance video` translates
  them with `--commit-map`; `conformance grid` and anything else that
  reads a manifest at a recorded commit (`record::manifest_at`) does not,
  and falls back to the working tree's manifest only when its hash
  matches. Bench records made before the rewrite can no longer seed the
  reference cache: `merge-base --is-ancestor METHOD_SINCE <sha>` fails
  for an id that no longer exists. Either translate ids in those paths
  too or rewrite the records once with the map.
