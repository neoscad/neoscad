# Design spike: a stack-independent (heap) evaluator

Status: design only, no code. Written against `215abc2` (0.2.1). It
answers the followup "A stack-independent evaluator (owner decision,
2026-10-01)" in `docs/followups.md` ("Performance").

## Summary

**Recommendation: option (a).** Put an explicit continuation stack around
the existing tree-walker. Do statements and instantiation first; that
stage is option (b), and it is shippable alone. Then do calls,
comprehensions and expressions that may reach a user call. Expressions
that cannot reach a user call stay on today's recursive fast path. Keep
the same `Ctx`, registers, `Value`, binding, builtins and memo, behind a
compile-time switch. Reject the full VM (c) for this goal.

Before any evaluator work, two things in the brief need correcting
(details in §0):

1. **No heap call stack has been measured in this codebase.** The audit's
   0.95–1.08× measured bytecode dispatch, and its prototype still recursed
   natively at every non-tail call. "No regression" is a hope, not a
   measured property.
2. **A heap evaluator alone does not lift the limit.** The tree the
   evaluator builds is walked recursively after evaluation (CSG dump,
   cache keys, `Node`'s derived `Clone`/`PartialEq`/`Drop`, geometry's
   parallel path). These walks are why a statement weighs 4 frames on
   wasm. Unbounded depth would move the overflow into them, so making them
   iterative is stage 0.

On firm ground:
- the recursion cycles and where native state lives (§1);
- OpenSCAD's limit is a measured 8 MiB stack whose depth its expected
  outputs hide (§0.3);
- today's native depths, re-measured for this document;
- the existing tools for A/B and benchmarking.

---

## 0. What the brief got wrong or left open

### 0.1 The 0.95–1.08× is not an explicit-stack measurement

The followup says "the audit measured the explicit stack alone at
0.95-1.08×". The audit's row (`docs/audits/bytecode-vm.md`, §3.4, first
row: 1.08×, 1.08×, 0.95×, 1.00×) is "bytecode, every region a real
context". In that prototype, calls were not heap frames:

- `eval_call` handed each call to `vm_eval_call`, which drove one callee
  body (§1, "Entry"). A nested call recursed natively.
- Under V8 the VM overflowed at 1,051 levels of function recursion
  (§4.5), so it was using native stack.
- "Calls without native recursion: a VM frame stack" is listed as unbuilt
  work, 1 week (§6).

So that number says **dispatch over an operand stack is worth nothing
here**. It says nothing about what moving the call stack to the heap
costs. The only estimate is §6's "fib is 1.2× now; about 2× expected",
which is an extrapolation. The only heap walk in the tree is geometry's
(`6c50a57`), and its commit gives no timing. Treat the cost of option (a)
as unknown until stage 3 is measured.

### 0.2 PGO on Windows arm64 and the DMG is blocked by something else

- **Windows arm64.** It ships a plain build because its *instrumented*
  binary crashed on every training run, and `llvm-profdata` rejected the
  profile (rust-lang/rust#150123; `docs/release.md`, "PGO builds",
  lines 371–376). The note there says "The plain build passed the depth
  guard there". A heap evaluator does not change this.
- **The DMG core (`neoscad-ffi`).** It is not PGO'd because it is "a
  different crate graph, and whether a CLI profile matches its functions
  is untested" (`docs/release.md:430-432`).

The heap evaluator removes the *depth guard* from both (§5), but neither
target gets PGO from it. The DMG needs an ffi training run (§4.3).
Windows arm64 needs the toolchain bug fixed.

### 0.3 "No recursion-depth limit" still needs a counted limit

OpenSCAD's conformance suite *requires* a "Recursion detected" error on
infinite recursion. Six expected outputs depend on it:
`tests/regression/echo/recursion-test-{module,vector,function,function2,function3}-expected.echo`
and `issue3118-recur-limit-expected.echo`.

OpenSCAD's limit is a measured stack:
- `StackCheck` compares the stack pointer against
  `PlatformUtils::stackLimit()` (`src/utils/StackCheck.h:12-35`);
- it is checked in `UserModule.cc:65,94`, `Expression.cc:596` and
  `Value.cc:415,432`.

The expected outputs cut the depth to `*** Excluding 1 frames ***`, which
`crates/conformance/src/normalize.rs:163` normalises away. The one-shot
CLI has no memory limit by default (`docs/architecture.md`, "Resource
limits"), so with no limit at all, `module crash() crash();` would run
until the OS killed it.

What can go is the dependence on the *machine's* stack. A heap evaluator
still needs a **deterministic counted depth limit**, the same on every
target, build and browser. That is a gain in itself: `Excluding N` and
the depth would no longer move with PGO, LTO or the engine. The default
is an owner decision (§6, Q1).

### 0.4 Cancelling already exists natively

`Options::interrupt` is polled at every call, statement and loop
iteration (`crates/eval/src/eval.rs:538-552`), and the app and `serve`
use it. The new capability is on the web only: a request there "runs to
the end on the worker's one thread; a stale or runaway one is stopped by
the page terminating the worker" (`crates/web/src/lib.rs:23-25`), which
throws away the warm memo, fragments and geometry cache.

A heap driver can **suspend**: return after N steps with its state
intact, so the worker can yield to its event loop and see a cancel.
Geometry kernels still cannot yield, so one long boolean still runs to
its end.

### 0.5 A stale table

`crates/eval/src/recursion.rs:52-58` gives native module depth as 16,842.
`conformance depth` on today's `target/release/neoscad` (0.2.0, built at
`34d69e2`'s time) reports:

| Test | Depth | OpenSCAD | Ratio |
|---|---|---|---|
| `recursion-test-module` | 65,507 | 30,261 | 2.16× |
| `module-if` | 21,842 | 7,052 | |
| `function-add` | 110,361 | | matches the table |

This was run on 2026-10-01 with load 2.17 (it uses little CPU). The table
predates `34d69e2`'s frame shrinking.

The same run gives the native stack per level, derived and not
instrumented: 64 MiB / 65,507 ≈ **1.0 KiB per module level** and
64 MiB / 110,339 ≈ **0.6 KiB per function level**.

---

## 1. Today's recursion structure

All references are to `crates/eval/src` unless noted. The evaluator is a
recursive tree-walker. Its *dynamic* state is already mostly on the heap.
What the native stack holds is the **Rust frames and their locals**: the
resume points.

### 1.1 The cycles

**Statements and instantiation.** Every nested statement is a native
level:

```
instantiate (inst.rs:239)  -> instantiate_frame (253)
  -> user_module (310) -> user_module_inner (340)
       -> eval_args (call.rs:106)          [expressions]
       -> bind_module (414) -> bind_user   [defaults: expressions]
       -> init_scope (inst.rs:49)          [assignments: expressions]
       -> instantiate_scope (148) -> instantiate ...
  -> builtin_module (builtins/modules.rs:381)
       children  -> children_module_inner (541) -> instantiate_children (inst.rs:178)
                                                -> init_scope + instantiate_scope
       echo/assert/let/part -> with_children (366) -> instantiate_children
       for/intersection_for -> for_each (eval.rs:2188, closure) -> instantiate_children
       if        -> eval_args, then instantiate_children
       geometry  -> geometry_module (624) -> children (weight `geometry`, modules.rs:1100)
```

**Function calls.** `eval` (`eval.rs:1523`) → `eval_expr` (1535) or
`eval_cold` (1590) → `eval_call` (`call.rs:469`):

- **Tail calls do not nest.** They are already a trampoline:
  `eval_call`'s loop calls `simplify` (679) and `simplify_call` (748),
  which return `Step::Next`/`Step::Pure` for the tail of a ternary, `let`,
  `assert`, `echo` or call. The loop swaps the step's context into one
  stack slot, recycles the old one, and counts to 1,000,000. That is
  OpenSCAD's `Expression.cc:626`.
- **A call that is not in tail position nests natively.** Examples are
  `1 + f(n-1)`, an argument, an index, a vector element, a ternary's
  condition (`simplify`, 693), and `let` assignments.

**Expressions.** These recurse through `eval_binary`, `Index`, `Unary`,
`Member`, `Vector` and `eval_range`:

- `let` goes through `assign_regs` or `sequential_assign` (2341, 2390),
  then the body.
- `assert` and `echo` go through `perform_assert` (2487) and `echo`
  (2440), then the body.
- Builtin calls go through `call_builtin` (`builtins/functions.rs:219`),
  which evaluates its own arguments, lazily for `is_undef` and `object`
  (244, 698).

**List comprehensions.**
- `eval_lc` (2021) → `eval_lc_frame` (2028) → `eval_element` (1903) and
  `for_each` (2188), which goes through `iterate_over` (2277) with a
  `&mut dyn FnMut` closure and recurses once per `for` variable.
- `for_each_reg` (2237), `lc_for_c` (2105), and `each_then` /
  `each_copied` (1938, 1975).
- Each comprehension level is about 4 wasm frames
  (`recursion.rs:293-301`).

**`use` and `include`.**
- `include` is spliced at parse time (`lang::fragment`), so it does not
  recurse at run time.
- `use` does: a lookup that reaches a used library builds a fresh file
  context and re-runs its top-level assignments on *every lookup*.
  `library_context` (`call.rs:1555`) → `init_scope` → `eval`. This is
  re-entry from inside a *name lookup* (`find_function`, `find_module`,
  `lookup_module`), so the lookups themselves would have to suspend, or
  run a nested driver.

**Function literals and methods.** These go through `simplify_call` →
`Callable::Literal` (807) and `method_call` (857), with the same loop as
user functions.

**The call memo and replay.**
- `call_enter` (`callmemo.rs:819`) runs after binding, before the body.
- `call_end` runs after it (`inst.rs:404`). A recording is identified by
  its index in `self.stack` (`recording_at(mark)`), which is the heap
  context stack, not native state.
- Replay copies a recorded subtree.
- `replay_fits` (`callmemo.rs:1120-1124`) refuses a replay deeper than
  the recording, by **native stack bytes** (`stack_used()`) and by
  `frames`.

**Statement reuse (`memo.rs`).** This applies to top-level statements
only, so it is not a depth concern. Its fingerprint hashes
`o.stack_limit` and `o.frame_limit` (`memo.rs:384-385`).

### 1.2 Where state lives

| State | Where | Notes |
|---|---|---|
| Live contexts for `$` lookup | `Evaluator::stack: Vec<Rc<Ctx>>` (eval.rs:172), heap | `push` and `truncate(mark)` bracket every scope. The marks are native locals. |
| Children chain | `CtxKind::Module(body, Children{scope, ctx})`, heap | `children()` reads `ctx.module_children()` (modules.rs:549). Nothing is on the native stack except the walk. |
| `let`, `for` and pure-frame variables | `regs`, `reg_base`, `reg_saves` (eval.rs:246-257), heap | Opened and closed around native frames. The saved old base is a native local (`reg_open` → `old`). |
| Parent module names | `module_names: Vec<Sym>`, heap | Pushed and popped around `user_module_inner`. |
| `part()` names | `part_stack`, heap | |
| Moved accumulators | `moved: Vec<Moved>`, heap | |
| Frame budget | `frames: u32`, plus `weights` (eval.rs:184-188) | Incremented and decremented around each native level. |
| **Partial results** | **native locals** | An operand already evaluated (`a` in `a + f()`), the `out: Vec<Value>` of a vector or comprehension, the `Vec<Node>` being filled, `ArgVal` vectors mid-evaluation, `for` iterator position (inside `iterate_over`'s closure), the tail loop's `cur`, `slot`, `regs`, `saves`, `call`, `depth` |
| **Trace attribution** | **native frames on the error path** | Each level adds its line as the `Err` passes up: `instantiate_frame` "called by" (290), `trace_call`, `init_scope` "assignment to" (104), `--trace-usermodule-parameters` (389) |
| Native stack measure | `stack_base` against an address in the current frame (eval.rs:508) | Decides the limit natively and caps it on wasm32 (`recursion.rs:326-338`) |

The heap version must move the last three rows into frame records. Making
the first rows' brackets (`mark`, the old register base) part of a frame
is mechanical.

### 1.3 Recursion outside the evaluator that scales with user depth

| Walk | Where | Recursive in |
|---|---|---|
| CSG text | `dump.rs:92` `csg_node` | tree depth |
| Geometry keys | `dump.rs:716` `max_index`, `Survey::walk`, `KeyBuilder::hash` (dump.rs:930ff, parallel at branch points) | tree depth |
| `Node` | `#[derive(Clone, PartialEq)]` (node.rs:205), default `Drop` | tree depth |
| Geometry render, parallel | `geom/src/evaluate.rs:1353` `par_iter().map(self.node)`: one native level per *branching* ancestor | branching depth (chains are iterative since `6c50a57`) |
| Printing values | `print.rs:76-90`, capped at OpenSCAD's 8 MiB measured stack | value depth |
| Syntax-tree drop | `session/src/parse.rs:479-497`, on a thread with `DEFAULT_THREAD_STACK` | source nesting |
| Parser | no depth limit (followup) | source nesting |

`STATEMENT_FRAMES = 4` exists because of the first four rows
(`recursion.rs:303-313`). The geometry pool is sized from
`eval::DEFAULT_THREAD_STACK` (`geom/src/evaluate.rs:1043`), and so are
sessions' per-request evaluation threads (`session/src/lib.rs:1726-1957`,
`eval::with_stack`).

---

## 2. Design options

### (a) An explicit continuation stack around the tree-walker (recommended)

There is one driver loop, and it owns a `Vec<Frame>`. `Frame` is an enum
of resume points:
- `Inst { sr, i, phase }`, `UserModule { ..., mark, node }`,
  `Children { ... }`, `ForStmt { iter, ... }`;
- `InitScope { k }`;
- `Call { cur, slot, regs, saves, depth, call }`, which is today's
  tail-loop locals;
- `Binary { op, lhs }`, `Args { k, vals }`, `Vector { out }`,
  `LcFor { iter, ... }`, `LetRegs { old }`, and the rest.

The driver works the way the geometry walk does: push on the way down,
finish on the way up (`6c50a57`, `Ctx::node`).

The key choice is that **expressions that cannot reach a user call stay
recursive**. A static `may_call` bit per `ExprId`, computed in `resolve`,
is false for subtrees whose calls are all `static_builtin`s and that
contain no comprehension invoking calls, no function-value call and no
`let` whose body may call. Those subtrees run through today's `eval`
unchanged, and their native depth is bounded by the AST nesting of one
body, not by call depth. Only `may_call` nodes go through frames. Both
the hot arithmetic and indexing path and the builtin fast path
(T1–T5, `d137102`) keep their code and inlining.

| | |
|---|---|
| **Effort** | My estimate, not measured: 5–8 engineer-weeks. Stage 0 is 3–5 days. Statements are 1.5–2.5 weeks. Calls and comprehensions are 2–3 weeks. Parity for limits, traces and the memo plus tests is 1 week. About 95 recursive call sites to convert (`self.eval(`, 38; `eval_args*`, 15; `instantiate*`, 12; `for_each`/`eval_element`/`eval_lc`, 22; `init_scope`, 5). The closure-based `for_each` and `iterate_over` become explicit iterator frames. For comparison, §6's VM rows: statements 2–3 weeks, heap calls 1 week. |
| **Risk** | Medium. Leaf semantics (ops, builtins, binding, lookups, registers) are shared, not duplicated. The risk is in ordering: node indices (`next_node_index`), message order, `rands` order, when `check_hard` runs, and trace lines. |
| **Performance** | Unmeasured (§0.1). The likely costs are a frame push and pop per `may_call` node, and an enum match, where today there is a direct call. The likely gains are no per-request 80 MiB thread spawn in sessions (`with_stack`), smaller I-cache pressure from un-inlined recursion, and possibly more memo replays (below). Bar: no regression. Stage 3 is where it is decided. |
| **What stays recursive** | Call-free expression subtrees (bounded by source nesting), the printer (value depth), the parser, and parse-tree drop. |
| **Call memo** | `call_enter` and `call_end` move to `UserModule` frame push and pop, and the recording base stays the `self.stack` index. `replay_fits` replaces native bytes with the frame-stack depth: an exact, cheap counter. The followup notes that recording the peak at every recursion check "cost 5x on fractal_tree". With a counter, the peak is one `max` per push, so a replay could be allowed whenever `depth + recorded_rel_peak ≤ limit`, not only at or above its recording depth. This is a likely gain; measure the replay count on the corpus. |
| **Rayon** | Evaluation stays single-threaded. Stage 0 keeps `Keys::hash`'s per-node slots, which are order-independent, when making it iterative. Parallelism stays at branch points, with an iterative fall-back past a depth. Same for `geom`'s `kids_in_parallel`. Determinism tests at 1, 2 and 8 threads, as in `geom/tests/render.rs`. |
| **Determinism** | Nothing new is parallel. The counted limit makes `Excluding N` *more* deterministic: the same on every build and target. |
| **WASM** | Plain Rust, no new dependency, no `unsafe`. The limit becomes a count, independent of the engine. |
| **Probe and weights** | Removable once stage 0 and stage 3 land, apart from the parser (§5). |

### (b) Statements and instantiation only, expressions recursive

This is the first half of (a): `instantiate`, `user_module_inner`,
`init_scope`'s loop, the control builtins and geometry children become
frames. A function call from a statement still nests natively.

| | |
|---|---|
| **Effort** | 2–3 weeks (plus stage 0). |
| **Risk** | Lower. The call and expression code, the most tuned code in the crate, is untouched. |
| **Performance** | Module instantiation is not hot: `modules100k` 60 ms, 1.00× under the VM (audit §3.1). Expect it to be neutral. |
| **What stays recursive** | All function recursion and comprehensions. WebKit still stops functions at about 60 levels and comprehensions at about 20 (followup baseline). |
| **Call memo** | As (a) for modules. |
| **Probe and weights** | Only `statement` and `geometry` go. `call`, `expression` and `comprehension` stay, and so do the probe, the 64 MiB stack and the PGO depth guard for functions. |

It does not meet the owner's goals alone, but it is the right first
milestone. Module chains through transforms are what failed on /try:
WebKit stops `m()` through `translate` at about 30 levels.

### (c) Towards the full bytecode VM (§6)

| | |
|---|---|
| **Effort** | 6–9 engineer-weeks for coverage (audit §6), plus the work of deleting the tree-walker. The prototype on `mr/vm-spike` (`d619f1c`) did not have heap calls or statements. |
| **Risk** | High. The audit's reasons for "no-go on a second engine" hold (Recommendation; §7): every warning, limit, trace, frame charge and memo rule twice, a fuzzer to stay exact, and 0–3% cost to the tree-walker even when switched off (§3.6). |
| **Performance** | 1.4–1.8× on BOSL2 evaluation, extrapolated and unmeasured (§6). Most of the measurable part (registers, pure frames) already landed in `37ca8eb` (1.08–1.16×). |
| **Call memo, rayon, WASM** | All have to be re-done in the VM. Same answers as (a). |
| **Probe and weights** | Removable at the end. |

This is not justified by the robustness goal. Revisit it only if
evaluation dominates the edit loop after (a), as the audit said.

### Not an option: segmented stacks (`stacker`/`psm`)

Growing the native stack on demand would fix native depth with little
code. On wasm the stack that overflows is the *engine's* machine stack,
and wasm code cannot switch or grow it (`recursion.rs:21-25`). So this
cannot meet the browser goal. I did not verify `psm`'s wasm32 support.

---

## 3. Staged migration behind a switch

**Switch.** A cargo feature on `neoscad-eval`, `heap-eval`, selects the
driver at compile time. A runtime `Options` flag would keep both paths in
one binary. The audit measured that costing the default engine 0–3%
through inlining (§3.6), and this crate is layout-sensitive
(`docs/followups.md`, "Evaluator layout sensitivity", 2–3%).

- The VM spike's precedent was an `Options::vm` field plus a
  `vm-default` feature for test runs.
- Here, the feature alone suffices. Workspace tests run twice
  (`cargo test` and `cargo test --features neoscad-eval/heap-eval`), and
  A/B runs compare two binaries.

Every stage's gate is all of:

- conformance 1773/0 at default threads and at 1 thread, both builds;
- the BOSL2 corpus A/B as the call-memo and VM work did it:
  - `conformance diff --format echo` and `--format csg` with
    `--binary-ref <current>` and `--binary <heap>`, and
    `--library-path .reference --library-path .reference/BOSL2`;
  - identical except unseeded `rands()` files and recursion-limit traces,
    with `Excluding N` normalised and the count of such files reported.
    The memo work saw 8; from stage 2 these differ by design;
- the bench models' STLs, byte for byte, from `conformance bench` outputs
  of both builds;
- `crates/eval/tests/{semantics,incremental,call_memo}.rs` under both
  builds;
- `scripts/wasm-check.sh --depths`;
- `node crates/web/test/run.mjs`.

| Stage | What | Testable alone because |
|---|---|---|
| **0** | Make the user-depth walks of §1.3 iterative: `csg_node`, `Keys::new` (`max_index`, `Survey::walk`, `hash` with parallel retained at branch points), manual iterative `Drop`/`Clone`/`PartialEq` for `Node`, node digests in `memo`/`callmemo`, `geom`'s parallel path past a depth. Independent of the evaluator. | Output identical by construction. Tests on a 96 KiB thread, as `geom/src/evaluate/small_stack_tests.rs` does. Determinism at 1, 2 and 8 threads. |
| **1** | The skeleton: the `heap-eval` feature, the `Frame` enum, a depth counter kept *alongside* today's checks (not enforced), and `resolve`'s `may_call` bit with counts in `resolve::Stats`. | Nothing observable changes. Report the `may_call` share on the corpus (`examples/resolve_stats.rs`). |
| **2** | Option (b): statements and instantiation on the driver, with the memo hooks on frame push and pop. Statement depth is counted under the feature. | The gate above. WebKit module chains go deep, while function depth is unchanged. |
| **3** | Calls: the tail loop becomes the driver's `Call` frame, and non-tail calls, comprehensions, `let`/`assert`/`echo` and function literals become frames. `library_context` runs a nested, counted driver. The counted limit replaces `recursion_exhausted`'s stack measure under the feature. | The gate, plus deep-recursion tests at the counted limit with identical output on native and wasm32. |
| **4** (owner's choice) | Suspension: the driver returns `Suspended` after N steps, and the web core yields between slices and honours a cancel message. | Web unit test: cancel a long evaluation, then a warm re-run hits the memo. |
| **5** | Make it the default. Keep the old driver for one release behind `--features recursive-eval`, then delete it and retire §5. | The gate, with roles swapped. |

---

## 4. Benchmark harness and protocol

### 4.1 Matrix

These are the build columns as the owner asked for them. Build every one
for **both** evaluators, each into its own `CARGO_TARGET_DIR` (`CLAUDE.md`).

| Column | CLI (`neoscad-cli`) | App core (`neoscad-ffi`) |
|---|---|---|
| Plain release (thin LTO, as shipped) | `cargo build --release` | `scripts/apple/build-core.sh` |
| PGO + thin LTO | `scripts/pgo.sh` (retrained per evaluator) | needs an ffi trainer (§4.3) |
| PGO + fat LTO | `CARGO_PROFILE_RELEASE_LTO=fat scripts/pgo.sh` (Cargo reads the environment override; `pgo.sh` passes it to both builds) | same, with the trainer |

That is 6 CLI builds and 6 ffi builds per stage. Record each build's wall
time and peak memory with `/usr/bin/time -l`.

- The only build-cost figures on record are CI's: `pgo.sh` 5–14 min
  against 2.5–6.5 min for a plain release (`docs/release.md:409-412`).
- Fat LTO's cost is unmeasured here. It adds a serial link step on every
  release target.
- Retrain PGO per evaluator, since a profile is valid only for the code
  that made it.

Owner decision: whether stage 0 and stage 1, which do not touch the hot
paths, may run only the plain and PGO + thin columns.

### 4.2 Workloads

| Workload | Tool | New code |
|---|---|---|
| Bench models (wall time, best-of-N) and STLs | `conformance bench` (full set, not `--quick`, at gates) | none |
| Ratios between two runs | `scripts/pgo-compare.py A.json B.json` (generic A/B despite its labels; 30 ms floor, geometric mean) | optional: relabel columns |
| BOSL2 `eval_only` (976 tests, summed wall time) | `conformance bench --only eval_only` | none |
| Served edit loop | `conformance bench --only edit_loop` (`serve`, `cli_via_serve`, `cli_cold`) | none |
| BOSL2 corpus wall time | the `conformance diff` A/B runs above, timed per binary (audit §4.2: 405 s against 354 s) | a `--time` summary in `diff`, or wrap in `/usr/bin/time` |
| Micro: fib, tail loop, list comprehension, nested `for`, `modules100k`, `let` in calls | `crates/eval/examples/eval_bench.rs` (in-process) | add deep non-tail cases (module 10k, `1+f(n-1)` 50k, comprehension 5k) |
| Depth, every build | `conformance depth --binary` | after stage 3: assert that `Excluding N` is identical across all 6 builds |
| App core edit to preview | none today | **new**: a Rust bench binary that drives `ffi::DocumentController` (`crates/ffi/src/controller.rs`) and `Core::run_document` (`crates/ffi/src/document.rs:213`) through the `edit_loop` cases. Time edit → result, and the mesh copy into host buffers separately. |
| Web worker edit to preview (node) | `crates/web/test/run.mjs` times add, run and warm run already (lines 164–184) | **new**: `crates/web/test/bench.mjs`, which loads the built worker core in node, replays the `edit_loop` edits, and times request → result and the mesh `ArrayBuffer` copy separately. Run on the `profile.web` (fat LTO) build. |
| Browser depth | manual: the probe programs at the counted limit in WebKit, Chromium and Firefox | none (the probe itself is the oracle until retired) |

### 4.3 PGO for the app core

`pgo.sh` builds `-p neoscad-cli` only (`scripts/pgo.sh:91,103`), and
whether a CLI profile matches `neoscad-ffi`'s functions is untested
(`docs/release.md:430-432`). Needed:

- a `--package` option;
- a trainer that exercises the ffi crate graph. The bench binary above,
  run over `pgo-train.py`'s model list, is the natural one.

Compare an ffi-trained profile with the CLI profile applied to ffi before
choosing.

### 4.4 Protocol

- Interleave: A1 B1 A2 B2 … over every column. At least 11 rounds for
  models of 30 ms or more, and 21–41 for parity claims (the libtess
  followup used 21–41).
- Before and after each block, record `uptime` load averages and
  `pmset -g therm`. Discard a block whose 1-minute load exceeds an agreed
  threshold. At the time of writing the machine sat at about 2.1 with
  nothing of mine running.
- Report the median and best, the instruction count where available
  (`/usr/bin/time -l` gives instructions retired on macOS), the machine
  and the commit. A ratio inside the layout-sensitivity band (±3%) is
  "parity", not a win or a loss.
- Kill criterion, an owner decision: if stage 3 under PGO + thin is more
  than 2% slower on evaluation-bound models (isosurface, hero,
  fractal_tree, screws, spring_handle) with no fix in sight, ship stage 2
  and keep functions on the native stack.

### 4.5 Fat LTO for releases, independent of the evaluator

This can be decided now, on the current evaluator, before any heap work.
- `[profile.release]` is `lto = "thin"` and `codegen-units = 1`
  (`Cargo.toml:62-67`).
- `[profile.dist]` inherits it.
- `[profile.web]` is already `lto = "fat"` (`Cargo.toml:76-82`).

To measure it:

1. Build `{thin, fat} × {plain, PGO}` CLI binaries:
   `CARGO_PROFILE_RELEASE_LTO=fat`, separate target directories, and
   `scripts/pgo.sh` under the same variable for the PGO pair. Record build
   time and peak memory per build.
2. Run conformance 1773/0 on each.
3. Run `conformance depth --binary` on each. **Today this is a gate.**
   Cross-crate inlining changes the evaluator's frame sizes, which changes
   the depth. The precedent: PGO's extra inlining grew frames by half and
   forced the stack limit from 48 to 64 MiB (`recursion.rs:74-82`). Fat
   LTO can do the same. With a heap evaluator, frame size stops mattering
   to depth, and this step becomes a check that `Excluding N` is
   identical.
4. Run the interleaved `conformance bench` (full), `eval_only` and
   `edit_loop` of §4.4, with `pgo-compare.py` for each pair: fat/thin
   plain, fat/thin PGO, and PGO + fat/plain thin.
5. Do the same for `neoscad-ffi` through `build-core.sh` with the
   override, measured with the new ffi bench.

The decision weighs the gain on evaluation- and geometry-bound models
against the extra CI time on six targets.

---

## 5. What gets retired, and how each is re-verified

| Item | Where | Retire when | Re-verify |
|---|---|---|---|
| Native stack measure: `stack_base`, `stack_used`, the stack half of `recursion_exhausted` | eval.rs:178-181, 508-521 | stage 5 | deep recursion of each kind ends at the counted limit with the same output on thin, PGO and fat builds and on wasm32 |
| `DEFAULT_STACK_LIMIT` (64 MiB) and its PGO rationale | recursion.rs:70-95 | stage 5 | as above; `conformance depth` reports identical N across builds |
| `DEFAULT_THREAD_STACK` and `with_stack` per request | session lib.rs:1726-1957, cli main.rs:422, lsp.rs:87/153, ffi language.rs:71 | stage 5, **except** for the parser and the parse-drop thread (`session/src/parse.rs:496`), which still recurse on source nesting | `edit_loop` latency (one less thread spawn per request); a 96 KiB-thread test of a deep model end to end |
| Geometry pool stack sized from eval | geom evaluate.rs:1043 | after stage 0 | `geom/tests/render.rs` at 1, 2 and 8 threads on a deep branching tree |
| Frame budget and weights: `FrameWeights`, `*_FRAMES`, `set_frame_weights`, `set_default_frame_limit`, `frames_at_last_check`, `note_frames`, the builtin-module ¼ margin (modules.rs:396-400) | recursion.rs, wasm.rs:52-97 | stages 0 and 3 | `scripts/wasm-check.sh --depths --all-programs`: every kind reaches the counted limit with no trap in node; the probe programs run by hand in WebKit, Chromium and Firefox at the limit |
| The web probe | `crates/web/js/worker.js` 82-177 | with the weights | worker start-up time (the probe's `ms`) disappears; the same manual browser run |
| The PGO depth guard in CI | `.github/build-setup.yml:45-60`, step 4 of `docs/release.md` "PGO builds" | stage 5 | keep `conformance depth`, re-purposed to assert identical N; PGO builds keep their conformance check |
| PGO exclusions | `docs/release.md:366-376, 428-436` | Windows arm64 is **not** retired by this (§0.2). The DMG needs §4.3's ffi trainer. | ffi PGO bench (§4.1); the depth check on the DMG's core |
| Memo's stack condition | callmemo.rs:1120-1124 | stage 3 | `crates/eval/tests/call_memo.rs`; corpus replay count before and after |
| Memo key fields `stack_limit`, `frame_limit` | memo.rs:384-385 | stage 5 (replace with the counted limit) | `crates/eval/tests/incremental.rs` |
| `PRINT_STACK_LIMIT` | print.rs:76 | optional: replace with a level count matching OpenSCAD's depth | `issue4172` (only the error line is expected) |
| Docs | architecture.md "Resource limits" (Recursion) and "Determinism"; recursion.rs module docs; the stale table (§0.5) | stage 5 | review |

---

## 6. Risks and open questions for the owner

**Decisions:**

1. **The counted limit's default, and where it lives.** It could be
   `Options` or `Limits` (and so `Limits::AGENT`). One number for every
   target? Today native modules reach 65,507 and functions 110,361. A
   heap frame plus its `Ctx` is perhaps 100–300 B (my estimate, not
   measured), so 1,000,000 levels is hundreds of MB. Under
   `Limits::AGENT` the 4 GiB memory limit would also apply, and the heap
   frames must be charged to the live-bytes estimate (`limits::live`) or
   a deep recursion escapes it.
2. **Suspension on the web (stage 4).** Is it worth it, given that the
   page can terminate the worker today? The gain is keeping warm caches
   across a cancel.
3. **The kill criterion (§4.4).** Is shipping stage 2 alone acceptable
   if stage 3 regresses?
4. **The reduced matrix** for stages that do not touch hot paths (§4.1).
5. **Fat LTO** for releases (§4.5), decided separately.

**Risks:**

- **Performance is unmeasured** (§0.1). The `may_call` split is the main
  mitigation. Measure its share on the corpus at stage 1, before
  committing to stage 3. If most BOSL2 expression nodes are `may_call`,
  the fast path covers little.
- **Ordering drift.** Node indices, message and trace order, `rands`,
  and `check_hard` points. Mitigations: the switch, the corpus A/B, and
  `call_memo.rs`'s on/off comparisons run under both builds.
- **A second driver during the transition.** This is the audit's
  standing tax, smaller here because the leaves are shared. Time-box it:
  delete the old driver one release after stage 5.
- **Source nesting.** The parser has no depth limit (followup), so
  deeply nested brackets still overflow natively and in browsers. "No
  limit in any browser" holds for *recursion*, not for nested source,
  until that followup is done.
- **Value depth.** Operators, comparisons and printing on deeply nested
  vectors recurse on value depth. Tail recursion can already build a
  million-level vector (value.rs:458-463), so this predates the heap
  work. It is not made worse, but it is not fixed either.
- **Memory-limit timing.** Heap frames change when live bytes peak. The
  call memo's 2× margin rule (`replay_fits`) assumes fresh evaluations
  allocate about what recordings did. Re-run `crates/eval/tests/memory_limit.rs`
  under both builds.

## Not verified

- Every effort estimate in §2. They are mine and extrapolated from the
  audit's §6 rows.
- Every expected speed or cost in §2(a). No measurement exists (§0.1).
- `psm`/`stacker` on wasm32 (§2). I did not retrieve its source.
- Fat LTO's build cost and run-time effect in this workspace.
- Whether a CLI PGO profile applies usefully to `neoscad-ffi`.
- The heap frame size. The native per-level sizes in §0.5 are derived
  from depth, not instrumented.

## Decisions (owner, 2026-10-01)

- **The bar for stage 2:** a slowdown of up to about 5% under PGO + thin
  LTO is acceptable for a fully stack-independent evaluator. Past that,
  stages 0-1 ship alone and calls stay native, bounded by the counted
  limit.
- **The counted limit** lives in `Limits`, as `--limit depth=N` alongside
  memory and time. Its default is set high enough to stay above today's
  native depths (2.16x OpenSCAD's for modules), and it's identical in
  every build and browser.
- **Resumable evaluation on the web** (cancel without restarting the
  worker) comes later, as its own step after stage 2.
- **Fat LTO for releases** is decided separately, from a measurement on
  today's evaluator.

## Fat LTO measured (v0.2.1 code)

The §4.5 measurement, run on 2026-10-01 at `f136256` (the v0.2.1 code)
on an Apple M4 Pro (14 cores, 48 GB, macOS 27.0, on AC power). It
replaces the "Fat LTO's build cost and run-time effect" line under
"Not verified".

**Builds.** Each variant was built cold into its own target directory,
one at a time, with the release profile and `CARGO_PROFILE_RELEASE_LTO`
set to `thin` or `fat`. The PGO variants ran `scripts/pgo.sh` under the
same variable, so each **retrained**: a profile is valid only for the IR
that was instrumented, and with fat LTO in `Cargo.toml` CI would also
train a fat instrumented build. The two profiles did differ (7.8 MB thin,
8.2 MB fat), so one profile for both would not have measured what CI
would ship. Build time is one wall-clock run under `/usr/bin/time -l`.
Peak memory is the largest single process (`maximum resident set
size`). Sizes are the `release` binaries as built, with `strip -S` in
brackets (close to `dist`'s `strip = "debuginfo"`).

**Checks.**
- `conformance run --binary`: 1773/0 for all four variants.
- `conformance depth --binary`: every variant passed the 1.25× guard.

**Speed.** `conformance bench --refs neoscad --binary`, full model set,
5 interleaved rounds (thin, fat, PGO + thin, PGO + fat per round). Each
run started only when the 1-minute load was below 3; the actual range
was 1.65–2.92, with no thermal warnings. Each model's speed is the best
of the 5 rounds' bests (each of those is best of 3), divided by thin's.
The geomean is taken over the 11 models of 30 ms or more, the same as
`scripts/pgo-compare.py`. The range in brackets is the per-round paired
geomean.

| Variant | Build | Peak mem | Binary (stripped) | Depth: module / function | Geomean vs thin |
|---|---:|---:|---:|---:|---:|
| thin (as shipped) | 43 s | 2.3 GB | 21.2 MB (20.2) | 65,507 (2.16×) / 110,361 (12.01×) | 1 |
| fat | 98 s | 3.9 GB | 19.2 MB (18.5) | 65,507 (2.16×) / 107,531 (11.70×) | **0.995** (0.992–1.009) |
| PGO + thin | 116 s (49 instr. + 27 train + 40 opt.) | 3.1 GB | 20.3 MB (19.3) | 39,919 (1.32×) / 55,181 (6.00×) | **0.932** (0.928–0.938) |
| PGO + fat | 266 s (147 instr. + 26 train + 93 opt.) | 6.7 GB | 18.7 MB (17.9) | 39,919 (1.32×) / 52,422 (5.70×) | **0.935** (0.928–0.955) |

The depth columns are `recursion-test-module` and `function-add`.
OpenSCAD's depths there are 30,261 and 9,192.

**Per model.**
- Fat against thin, plain: every model is between 0.973 and 1.011,
  inside the ±3% layout band of §4.4.
- PGO + fat against PGO + thin: 1.004 overall. The largest differences
  are `bosl_isosurface__006` (1.013), `bosl_fractal_tree` (0.979) and
  `mink_convex` (1.057; 22 ms, below the floor).
- The PGO gain is all PGO's. Both PGO variants gain the most on
  `bosl_isosurface__006` (0.878 thin, 0.890 fat) and `ex_menger` (0.890,
  0.896), then `text_30lines` and `bosl_screws__001` (about 0.91–0.93).
- `eval_only` (BOSL2's 976 tests, summed): thin 30.35–31.03 s, fat
  30.01–30.39 s, PGO + thin 28.83–31.42 s, PGO + fat 28.82–31.88 s.
  Fat is about 1% faster than thin, PGO about 5%, and fat adds nothing
  under PGO.
- The served edit loop (BOSL2 render 9.1 ms, PGO 8.7 ms) and cold start
  (2.7–2.9 ms) are the same under thin and fat.

**Depth.** Fat LTO leaves module recursion where it was. Function
recursion is about 3% shallower plain (110,361 to 107,531) and 5% under
PGO (55,181 to 52,422), still more than 5.7× OpenSCAD's. The gating
case is PGO's module depth: 1.32× on this machine either way, against
1.42–1.55× in CI's PGO run (`docs/release.md`, "PGO builds").

**App core.** `neoscad-ffi` was measured with a throwaway harness
outside the workspace. It is a binary depending on `neoscad-ffi` by
path, with the workspace's `[patch.crates-io]`, `Cargo.lock` and release
profile. It times `Core::render` (`RenderMode::Render`) per bench model,
with a fresh `Core` for every run, best of 3, over 3 interleaved rounds.
- Build: 46 s and 2.0 GB thin, 93 s and 3.4 GB fat.
- Geomean, fat against thin: **0.998** over 7 models of 30 ms or more
  (rounds 1.002, 1.000, 0.997).
- Not measured: the two BOSL2 `file` models opened from disk rendered
  in under 1 ms. Their `include <BOSL2/...>` did not resolve without
  the CLI bench's library path, so they fall under the floor and are
  excluded.
- Not measured: `import_stl`. It needs generated inputs and was
  skipped.

**Cost in CI terms.** These are scaled from this machine, not measured
on runners. Fat LTO made each build about 2.3× as long and used
1.7–2.1× the peak memory. Most of the extra is the serial fat-LTO link.
The worst case is the instrumented build: 49 s became 147 s, because
fat LTO there optimises the instrumented IR as one module.

Release jobs take about 8–18 min per target. The `pgo.sh` part is 5–14
min of that, and a plain build is 2.5–6.5 min. Scaling the build share
by 2.3× gives:
- about +3–9 min on each plain target (`x86_64-apple-darwin`,
  `aarch64-pc-windows-msvc`);
- about +7–18 min on each of the four PGO targets, about 15–30 min per
  job.

Runners with fewer cores lose less of thin LTO's parallelism, so the
real ratio there may be lower. That is unverified. The fat
instrumented build's 6.7 GB peak would need checking against each
runner's memory; that was not checked here.

**Recommendation: keep thin LTO for releases, plain and PGO.**
- On the current evaluator fat LTO is parity: 0.995 plain and 1.004
  under PGO, with the app core at 0.998.
- Its only clear gain is size: about 2 MB (8–9%) off each binary.
- Against that it costs 2.3× the build time, which is minutes per target
  and up to a doubling of the PGO jobs. It also needs up to twice the
  peak memory, and costs a little function-recursion depth.
- PGO stays the lever, at 0.93.
- Untested: fat optimised builds using a profile trained on a thin
  instrumented build. That would avoid the 147 s instrumented build, but
  the two profiles differ, and the speed result above gives no reason to
  try.
- `[profile.web]` stays fat. There size is the point, and no PGO is
  involved.
