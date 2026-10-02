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

## Stage 0 done

Measured on 2026-10-01 against `83882f6` (base) on the same Apple M4
Pro, plain release (thin LTO) and PGO + thin LTO (`scripts/pgo.sh`,
retrained per side), as the owner's reduced matrix allows for stage 0.
No `heap-eval` feature: stage 0 is unconditional.

**What changed.** Every walk over the finished node tree keeps its
pending nodes on the heap:
- `Node`'s `Clone`, `PartialEq` and `Drop` are hand-written and
  iterative (`node.rs`), as is `find_root_tag`. `Debug` stays derived
  (a followup).
- The `.csg` dump (`dump.rs`, `csg_node`) and the keys: `max_index`, the
  survey (pre-order for the file stats, reverse pre-order for counts and
  sizes) and the hashing (post-order).
- The parallel walks. The key hashing and the render walk
  (`geom/src/evaluate.rs`) still split at branch points, but at most
  `PARALLEL_MAX_NESTING` (64) splits deep; past that a subtree is walked
  serially, which changes no key and no result. Before, a tree that
  branches at every level nested a rayon split per level.
- Also: the preview's `CsgTree::build` (`collect_leaves`, `visit`),
  `session`'s `parts::find` and the `use` hint's walk.
- The memo and call-memo node walks (`memo.rs`, `callmemo.rs`) were
  already iterative.

**Tests.** `crates/eval/tests/deep_tree.rs` builds 100,000-level trees
directly (a chain, and a comb that branches at every level). On a
128 KiB thread they clone, compare, drop, find the root tag and are keyed,
and a 5,000-level chain dumps. The comb's keys are the same on 1 and 8
threads. `geom`'s small-stack tests render both shapes at 100,000 levels
on a 96 KiB thread and match a parallel render, and the chain previews.
On the base code each of the five eval tests overflows. With the render
nesting cap removed, the parallel render of the comb overflows too.

**Output.** All identical:
- conformance 1773/0, at default threads and with `RAYON_NUM_THREADS=1`;
- `conformance diff --binary-ref <base> --binary <stage 0>` with BOSL2 as
  a library path, echo and csg:
  - BOSL2 `examples_x`, `tests_x` and `examples`: 3512 files each, all
    identical except `isosurface__022` in csg, which calls unseeded
    `rands()` and differs between two runs of the base binary too;
  - the OpenSCAD examples: 50/50;
  - the bench model files: 9/9;
- the bench models' STL and console, base against stage 0, at 1 and 8
  threads: 13 models, all identical (`import_stl` skipped), and each
  model's STL is the same at 1 and 8 threads.

**Checks.** `cargo fmt`, `clippy -D warnings`, `cargo test` (workspace),
`scripts/wasm-check.sh --depths` and `node crates/web/test/run.mjs` pass.

**Speed.** `conformance bench --refs neoscad`, full set, 8 interleaved
rounds of base plain, stage 0 plain, base PGO, stage 0 PGO. Each run
started at a 1-minute load below 3; the load seen across runs was
1.6–4.0, with no thermal warnings. The ratio is stage 0's best over
base's best across all rounds; the geomean is over the 11 models of 30 ms
or more.

| Model | Plain | PGO + thin |
|---|---:|---:|
| bosl_fractal_tree | 1.022 | 0.979 |
| bosl_gears__003 | 0.996 | 0.980 |
| bosl_isosurface__006 | 1.022 | 1.005 |
| bosl_screws__001 | 1.001 | 0.988 |
| bosl_spring_handle | 1.002 | 1.008 |
| csg_deep_union | 0.984 | 1.030 |
| csg_spheres | 0.983 | 1.011 |
| ex_menger | 1.001 | 0.996 |
| extrude_twist | 0.979 | 0.994 |
| import_stl | 1.015 | 0.986 |
| text_30lines | 0.986 | 1.028 |
| **Geomean** | **0.999** | **1.000** |
| Per-round paired geomean | 0.986–1.025 | 0.981–1.027 |

- Every model is inside the ±3% layout band (§4.4): parity.
- Over the first 3 rounds alone, the geomeans were 0.997 plain and 1.015
  PGO. The PGO rounds then spread on both sides of 1 (0.981–1.017).
- `eval_only` (BOSL2's 976 tests, summed), median of 8: plain 31.50 s
  base, 31.72 s stage 0 (best 31.03, 31.10). PGO: 29.60 s and 29.67 s
  (best 29.11, 29.26).
- Served edit loop, BOSL2 render, best: plain 9.3 and 9.2 ms, PGO 8.6
  and 8.7 ms. Cold start: 2.8 ms in all four.

**Depth.** `conformance depth`:
- Plain: modules went from 65,507 to 66,021 (2.18×), `module-if` from
  21,842 to 22,072, and functions stayed at 110,361.
- PGO: unchanged at 39,919 (1.32×) and 55,181.

The evaluator's own frames decide these. They moved a little in the
plain build because inlining changed around the new `Drop`.

**Web.** The wasm core and the `/try` bundle were built with the default
scripts (no `wasm-opt`), and served locally.
- The probe programs (`modT`, `mod`, `fn`, `child`, `lc`, `expr`, `nest`)
  reach the same depths before and after in Chromium (166, 249, 327, 206,
  135→137, 924→925, 623) and Firefox (166, 249, 498, 206, 165, 1749,
  623). In WebKit they are within one or two levels: 39→38, 81, 66→68,
  65→67, 24, 105, 93→96.
- In WebKit the worker's probed statement weight is 6080 both times.
- All 8 `/try` examples preview and render in all three browsers. So do
  the deep probe examples, which end in a result or OpenSCAD's recursion
  error and never crash.

**The gain on wasm is zero, not the one §1.3 and the summary expected.**
- In node 22 with no frame budget (`run.js --depths --all-programs
  --frames=1000000000`), V8 overflows at the same depth before and after:
  module 1,611, function 1,712, `module-transforms` 489,
  `module-children` 793. `function-lc` ends in the recursion error at
  523 and 521.
- The walks over the finished tree run after the evaluation has
  unwound, and per level they cost less than instantiation, so they
  never set the limit. The "4 frames per statement" is instantiation's
  weight, and a conservative one: a module level costs about what a
  function level does (1,611 against 1,712 levels), yet it is charged 8
  frames to a function's 4. That is a followup, not stage 0.
- What stage 0 does buy is the point of §1.3. Once stages 1–2 lift the
  evaluator's limit, the tree it builds can be dumped, keyed, rendered,
  previewed, copied and freed at any depth, on any thread.

## Stage 1 done

Measured on 2026-10-01 against `fa896a6` on the same Apple M4 Pro. This
step is the plan's stages 1 and 2 (§3) together: the `heap-eval` switch
and statements on the heap. The `may_call` bit of §3's stage 1 is left for
the calls stage. "Base" below is this change built without the feature;
"main" is `fa896a6`.

**What changed.**
- `crates/eval/src/heap.rs`, compiled only with the `heap-eval` cargo
  feature of `neoscad-eval`. It covers module instantiation and its
  scopes, user modules, `children()`, the control builtins (`echo`,
  `assert`, `let`, `for`, `intersection_for`, `if`, `part`) and geometry
  modules' children. They run in one loop over a stack of frames, and
  each frame is the part of a native function that runs after its callee
  returns. Expressions and function calls stay native: an expression
  starts from the loop's own native frame at any module depth, so
  statements add nothing to the frame budget.
- The leaves are shared rather than copied: arguments, scope assignments,
  binding, lookups, node construction and the call memo. A few helpers
  were split out of `builtins/modules.rs` so both paths can call them
  (`geometry_kind`, `geometry_params`, `is_leaf`, `children_select`,
  `part_name`); so was `Range::iter_at`. They are `#[inline(always)]`, so
  the default build's code is unchanged in shape.
- The first version of the driver ran 10–20% more instructions than the
  recursive evaluator on statement-heavy models. A sample showed the time
  in the driver loop itself: a 248-byte node passed back in every result,
  and a boxed frame per statement. Now:
  - results are 32 bytes;
  - finished nodes go onto one node stack, and each scope or loop takes
    its own off the top, as one exactly sized vector;
  - frames are at most 40 bytes and unboxed (a `for` loop's state
    excepted);
  - scopes run in place on the top of the stack.

  Instructions retired against the base build, from an `echo` export, so
  evaluation only:

  | Model | Change |
  |---|---:|
  | 2.25 million `for` iterations of `let`, `if` and `translate` | −1.7% (wall −19%) |
  | a binary tree of 131,000 module calls | +0.6% |
  | 200 `children()` chains, 200 deep | −2.6% |
  | BOSL2 `fractal_tree` | +1.4% |
- **The counted limit.**
  - `Limits::depth` (`--limit depth=N`, the JSON key `depth`) counts
    nested user module calls. It always applies: `None` means
    `DEFAULT_DEPTH`, which is 100,000, and it cannot be `off`.
  - On the heap it is what stops a module recursion. The default build
    checks it too, but there the 64 MiB stack stops first, at about
    33,000 levels.
  - `conformance depth` gives 199,977 trace lines (two per level) for
    `recursion-test-module`, 6.61× OpenSCAD's, in both heap builds.
    `module-if` reaches 99,999 levels. The default builds are unchanged,
    the same as main: 66,021 plain and 39,919 PGO. Functions are where
    they were: 110,355 plain and 55,177 PGO.
  - A recursion to the limit holds about 100 MB: recursion-test-module's
    peak RSS is 100 MB, against 88 MB natively.
- **The call memo.** Under the feature, its replay rule (`replay_fits`)
  compares module depth (`memo_depth`), not native stack bytes, since
  every statement starts on the same native stack.
- **Printing.** Under the feature, `print.rs` charges each nested module
  level 2 KiB of virtual stack (`MODULE_LEVEL_STACK`, what a level holds
  natively). Without that, printing deep in a module recursion had the
  whole 8 MiB at every level. `recursion-test-vector` then traced each
  level's ever deeper vector in full and took 76 s instead of 0.35 s
  (0.44 s with the charge).
- **Web.** A core built with the feature skips the worker's
  module-recursion probes (`heapStatements()` in `crates/web/src/wasm.rs`):
  - those probes only ran to the counted limit;
  - in WebKit they took about 40 s at start-up, and /try requests timed
    out.
- The feature is forwarded by `neoscad-cli`, `neoscad-ffi`, `neoscad-web`
  and `neoscad-wasm-check`. `scripts/pgo.sh`, `scripts/wasm-check.sh`,
  `scripts/web/build-core.sh` and `scripts/apple/build-core.sh` take
  `NEOSCAD_FEATURES=heap-eval`.

**Tests.**
- `crates/eval/tests/statements.rs` compares 20 programs' messages, `.csg`
  dumps and node indices with expected files that the recursive
  evaluator wrote. It covers:
  - `$` variables through `children()`, `for` and `let`;
  - children indices and chains;
  - every kind of `for` value;
  - errors in arguments, scope assignments, `for` ranges, `if`
    conditions and `let`;
  - `--hardwarnings`;
  - the call memo on and off;
  - `part()`;
  - the depth limit with and without parameter traces;
  - an interrupt in the middle of a recursion.

  Both builds match all 20.
- Under the feature, four kinds of module recursion reach 99,999 levels
  and stop at 100,000 on a 128 KiB thread: plain, through `translate`,
  through `children()` and through `for`/`let`.
- `call_memo.rs` and `semantics.rs` keep their frame-budget tests for the
  default build. Under the feature they check that statements leave the
  budget to calls.

**Output.** All with the feature on against base:
- conformance 1773/0 for both builds, at default threads and with
  `RAYON_NUM_THREADS=1`;
- `conformance diff`, echo and csg, BOSL2 as a library path:
  - the BOSL2 corpus: 3512/3512 echo, and 3511/3512 csg, where the one is
    `isosurface__022` (unseeded `rands()`);
  - the OpenSCAD examples: 50/50;
  - the bench files: 9/9;
- the bench models' STL and console at 1 and 8 threads: 13 models
  identical, and each the same at both thread counts.

**Checks.** All pass with the feature off and on:
- `cargo fmt`;
- `clippy -D warnings`;
- `cargo test --workspace`;
- `scripts/wasm-check.sh --depths`, which lints for wasm32 too;
- `node crates/web/test/run.mjs`.

**Speed.** Five interleaved rounds of main, base and heap, each plain and
PGO + thin (each side retrained), with each run started at a 1-minute load
below 3. The table is `conformance bench --refs neoscad`, full set: the
best of the five rounds' bests, as a ratio.

| Model | heap/base plain | heap/base PGO | base/main plain | base/main PGO |
|---|---:|---:|---:|---:|
| bosl_fractal_tree | 1.044 | 0.971 | 0.980 | 0.994 |
| bosl_gears__003 | 1.004 | 0.987 | 0.994 | 1.000 |
| bosl_isosurface__006 | 0.999 | 1.001 | 0.998 | 1.001 |
| bosl_screws__001 | 0.991 | 0.988 | 1.004 | 1.004 |
| bosl_spring_handle | 0.998 | 0.987 | 0.998 | 1.007 |
| csg_deep_union | 1.010 | 1.013 | 1.005 | 1.021 |
| csg_spheres | 0.996 | 1.017 | 0.991 | 0.997 |
| ex_menger | 0.989 | 0.988 | 0.998 | 1.014 |
| extrude_twist | 1.019 | 1.026 | 0.989 | 1.009 |
| import_stl | 1.021 | 0.988 | 0.986 | 0.999 |
| text_30lines | 1.010 | 0.995 | 1.021 | 0.993 |
| **Geomean (11 models ≥ 30 ms)** | **1.007** | **0.996** | **0.997** | **1.004** |
| Per-round paired geomean | 0.992–1.014 | 0.986–1.021 | 0.988–1.014 | 1.000–1.010 |

- Every geomean is within ±2%: parity, for the heap build and for the
  default build against main.
- Three models are outside ±2% on one side only:
  - `fractal_tree` plain, 1.044. It is 0.971 under PGO, and its
    evaluation, measured alone, runs +1.4% more instructions. Its render
    time moves by about ±3% between runs of one binary (stage 0 saw
    0.979–1.022).
  - `extrude_twist` under PGO, 1.026, and `import_stl` plain, 1.021.
    Their time is geometry.
- `eval_only` (BOSL2's 976 tests, summed), median of five:

  | Build | Base | Heap |
  |---|---:|---:|
  | Plain | 30.59 s | 30.53 s |
  | PGO | 29.03 s | 29.11 s |

  Main: 30.66 s plain, 28.95 s PGO.
- **The BOSL2 corpus**, echo, process time summed over its 3512 files,
  one A/B run each:

  | Run | Times |
  |---|---|
  | Plain | base 271.6 s, heap 270.3 s |
  | PGO | base 259.8 s, heap 257.8 s |
  | Main against base, plain | 276.9 s and 276.5 s |
  | Main against base, PGO | 256.7 s and 256.2 s |
- **The served edit loop** (`serve`, best and median):

  | Build | BOSL2 render, base | BOSL2 render, heap |
  |---|---|---|
  | Plain | 9.3 / 9.8 ms | 9.2 / 9.9 ms |
  | PGO | 8.8 / 9.3 ms | 8.7 / 9.4 ms |

  Snapshots and the CSG case agree within 0.3 ms.
- **The app core.** A throwaway harness drives `DocumentController` and
  `Core::run_document` with no viewport, through the edit loop's cases:
  20 edits per round, best of rounds and median of the round medians.
  BOSL2 preview: 8.56/9.17 ms base, 8.55/9.07 ms heap. BOSL2 render:
  8.58/9.00 ms base, 8.61/9.00 ms heap. CSG is under 1 ms on both.
- **The web worker in node** (`profile.web`, fat LTO), timed from request
  to result, with the copy of the mesh buffers timed separately:
  - CSG preview: 3.85 ms base, 3.83 ms heap (median).
  - A tree of 1,023 module calls, preview: 10.77 ms base, 10.72 ms heap.
    Render: 150.1 ms base, 151.3 ms heap.
  - The mesh copies: 0.01–0.13 ms either way.
  - The first version of the driver was 7.5% slower on the tree preview.

**Browsers.** The heap core, served locally with the default scripts:
- Module recursion through `translate`, plain and through `children()`
  evaluates at 99,999 levels and stops with OpenSCAD's recursion error
  at 100,000. That holds in Chromium, WebKit and Firefox. In WebKit the
  default core stops at 31, 54 and 44 levels.
- Functions and comprehensions are where they were: WebKit 67/24,
  Chromium 327/137, Firefox 499/166.
- All 8 `/try` examples preview and render in all three browsers, and so
  does the deep example.
- **Source nesting does not improve.** `translate() translate() …
  cube()` still overflows the engine at the same depth with and without
  the feature: 187 in WebKit and 1,313 in Chromium. The cause is
  `Unit::add_scope` and the parser, which recurse over the source. With
  the feature, depths that the frame budget stopped early with a clean
  error now evaluate: in WebKit from 150, in Chromium from 475.

**Left for stage 2 (calls), and followups.**
- Function calls, comprehensions and expressions on the heap, with the
  `may_call` bit; the counted limit for functions; `library_context`.
  WebKit's function depth (about 67) is still the browser limit.
- The memory estimate does not charge the frames: about 100 MB at the
  default depth.
- The apps' `ResourceLimits` record does not carry `depth`.
- `Unit::add_scope` recurses on source nesting (above).
- Under the feature the frame budget's `statement` and `geometry`
  weights, and the probes for them, are unused. They are deleted at
  stage 5, with the default switched.

## Stage 2 done

Measured on 2026-10-01 against `8f0fe78` on the same Apple M4 Pro. This
step is the plan's stage 3 (§3): function calls, comprehensions and the
expressions around them on the heap. "Base" below is this change built
without the feature; "main" is `8f0fe78`.

**What changed.**
- `crates/eval/src/heap_expr.rs`, compiled only with `heap-eval`. User
  function calls run on a loop over an explicit stack: their tail-call
  loop (pure frames, register `let`s, accumulator moves), function
  literals, list comprehensions (`for` over every kind of value, `if`,
  `each`, `let`), `let`, `assert` and `echo` expressions, and every
  expression on the way to a call (operators, `&&`/`||`, ternaries,
  indexing, member lookups, vectors and `[each x, ...]`). A frame is
  again the part of a native function that runs after its callee
  returns. A call's loop state, the argument vectors, the lists being
  filled and a comprehension `for`'s and a `let`'s state wait on side
  stacks, so a frame is at most 48 bytes; a call's state is boxed and
  pooled, so it moves as a pointer whenever it waits.
- **The `may_call` bit** (`Evaluator::may_call`): per expression, whether
  its subtree holds a call that `static_builtin` cannot pin to a
  builtin, not looking inside function literals. It is worked out on
  first use per expression, with an explicit-stack walk, and kept per
  unit (`Unit::may_call`), not in `resolve`: the resolver resolves a
  function's body when it is first called, and the bit needs those
  resolutions, which are always in place before a body's expressions
  run. On the heap, a subtree without it runs through the recursive
  evaluator unchanged, and so does every operand that cannot call.
- **Native first; the heap past 8 call levels** (`NATIVE_CALLS`). The
  first version ran every user call on the heap. It was byte-identical
  (conformance, the whole A/B sweep below), but it ran BOSL2's examples
  26% slower and the corpus 17% slower. A profile put the cost in the
  loop itself: every node on the way to a call was a frame and a trip
  through the loop. A leaf call (`sq(i)` in a comprehension) cost about
  600 more instructions than natively.
  - Now `eval_call` counts the user calls running natively
    (`Evaluator::native_calls`) and hands the ninth nested one to the
    loop. Everything that call reaches runs on the heap until it
    returns.
  - The native stack holds at most 8 call levels at any depth.
  - Most work is done at shallower depths, where the cost is one
    compare per call.
  - A debug build uses 0, so every call is on the heap. Its frames are
    many times larger (8 levels overflowed a 128 KiB test thread), and
    the test suite then covers the heap path everywhere.
- The leaves are the recursive evaluator's: the heap's frames call the
  same lookups, binding, builtins, accumulator moves and register code,
  in the same order. The native code gained four `#[inline(always)]`
  splits so both can call the second halves: `pure_frame`/`pure_bind`,
  `call_frame`/`frame_bind`, `echo`/`echo_values` and
  `perform_assert`/`assert_values`. It also gained `call_exhausted`.
- **The counted limit counts calls.** `Limits::depth` now counts the
  user modules being instantiated plus the user function calls in
  progress (`Evaluator::depth_used`): non-tail calls, native or heap,
  not counting builtins. So one limit stops a recursion through modules,
  functions or both, with OpenSCAD's `Recursion detected calling
  function 'f'` and one `called by` trace line per level.
  - Natively at the default 100,000, `function-add` reaches 99,999
    levels in 0.02 s and 41 MB, and a recursion through a comprehension
    takes 0.03 s and 100 MB.
  - The default build does not count function calls: its native stack
    still decides.
- **The call memo** is unchanged. A module call starts only from a
  statement, when no function call is in progress, so `memo_depth`
  stays the module depth.
- **Printing** charges each level of function recursion 600 bytes of
  virtual stack (`FUNCTION_LEVEL_STACK`, what a level holds natively),
  as stage 1 charged module levels.
- **Web.** A heap core skips the worker's stack probes entirely
  (`heapStatements()`), not only the module ones. Every probe would only
  recurse to the counted limit. The default weights and budget stand.

**Tests.**
- `crates/eval/tests/functions.rs` compares 16 programs' messages,
  `.csg` dumps and node indices with expected files that the recursive
  evaluator wrote. The harness is now shared with `statements.rs`
  (`tests/support`). The programs cover:
  - calls in every expression position, with `echo` side effects and
    short-circuits showing the order;
  - closures and function literals: recursive literals, literals
    returned and called, `fs[i](x)`, compose, defaults that call;
  - `$` variables across calls, including `$fn` defaults;
  - tail calls with `concat` and `[each acc, n]` accumulators, tail
    `let`, `assert` and `echo`, and the 1,000,000-step tail limit;
  - comprehensions: nested and multi-variable `for`, every kind of `for`
    value, `if`/`else`, `let`, `each` over values and comprehensions,
    C-style `for`, recursion through comprehensions, the 2e6-element
    range warning;
  - `assert` failing at the bottom of a recursion, in a comprehension, a
    `let` and an argument, with their traces;
  - unknown functions and variables;
  - `--hardwarnings` deep in a recursion;
  - the list limit inside a comprehension;
  - the call memo on and off for modules that call recursive functions;
  - an interrupt in the middle of a function recursion.

  Both builds match all 16, the debug build with every call on the heap
  and the release build with the mix.
- Under the feature, on a 128 KiB thread, six recursions reach 99,990
  levels and stop past 100,000 with the error:
  - `1 + f(n - 1)`;
  - through a comprehension;
  - through `let` and `max()`;
  - through an index;
  - a function literal;
  - modules and functions half and half.
- With `--limit depth=50`, a function recursion stops at 50 levels with
  every level traced, and module levels count towards the limit.
- `semantics.rs`'s frame-budget test now checks the counted limit for
  functions under the feature.

**Output.** All with the feature on against base:
- conformance 1773/0 for base and heap, plain and PGO, at default
  threads and with `RAYON_NUM_THREADS=1`;
- `conformance diff`, echo and csg, BOSL2 as a library path:
  - the BOSL2 corpus: 3512/3512 echo, and 3511/3512 csg, where the one is
    `isosurface__022` (unseeded `rands()`);
  - the OpenSCAD examples: 50/50;
  - the bench files: 9/9.

  This held both for the first version (every call on the heap) and for
  the final one.
- the bench models' STL and console at 1 and 8 threads, plain and PGO
  pairs: all identical, and each the same at both thread counts.

**Checks.** All pass with the feature off and on:
- `cargo fmt`;
- `clippy -D warnings`;
- `cargo test --workspace`;
- `scripts/wasm-check.sh --depths`: off, functions 498 and modules 249;
  on, 99,999 for both;
- `node crates/web/test/run.mjs` on both cores.

**Browsers.** The heap core and the `/try` bundle with it, served
locally:
- In Chromium, WebKit and Firefox, every kind of recursion evaluates at
  99,990 levels and stops with OpenSCAD's recursion error at 100,010:
  - `1 + f(n - 1)`;
  - through a comprehension;
  - through `let` and `max()`;
  - a function literal;
  - modules then functions.

  Module recursion through `translate` and `children()` stops at
  100,000, as in stage 1.
- WebKit's function depth was 66 and comprehensions 24 (the base core
  today, probed). It is now the limit. Chromium's was 327/137 and
  Firefox's 498/165.
- The worker starts with no probe: its `probe.ms` is 0, against the
  base core's runs.
- All 8 `/try` examples preview and render in all three browsers.
- **Source nesting is unchanged, and the probe never protected it.**
  Nested parentheses `((…1…))` crash the engine at the same depth with
  and without the probe in Chromium (925) and Firefox (1,749): the
  parser overflows first. WebKit evaluates 170 levels on the heap core,
  against 105 on the base core, and both crash past that. Nested
  `translate()` statements evaluate deeper on the heap core (Chromium
  1,312, WebKit 189, Firefox 2,807) and then crash. The base core stops
  them earlier with the frame budget's clean error (474, 89, 623). That
  is stage 1's finding. Its cause is the parser and `Unit::add_scope`
  (a followup), not the evaluator, so the probe would not help it.

**Depth.** `conformance depth`, plain and PGO + thin, main, base and
heap:

| Test | main and base, plain | main and base, PGO | heap, plain and PGO |
|---|---:|---:|---:|
| `recursion-test-module` | 66,021 | 39,919 | 199,977 |
| `module-if` | 22,072 | 12,371 | 99,999 |
| `recursion-test-function3` | 110,338 | 55,159 | 99,977 |
| `function-add` | 110,360 | 55,181 | 99,999 |

- The heap builds give the same numbers plain and PGO. The depth no
  longer depends on the build.
- `function-add` on the heap is 10.9× OpenSCAD's. That is above the PGO
  builds that ship (6.0×), but below the plain build's native 12.0×. A
  plain-build program that recursed between 100,000 and 110,000
  function levels would now stop. Raising `DEFAULT_DEPTH` to stay above
  it is the owner's call (§6, Q1). It would cost about 5 MB per 10,000
  function levels, and 10 MB per 10,000 module levels.

**Speed.** Five interleaved rounds of main, base and heap, each plain
and PGO + thin (each side retrained). Each run started at a 1-minute load
below 3. The load seen at the starts was 2.4–3.0, and up to 8.8 between
runs, with no thermal warnings. The table is `conformance bench --refs
neoscad`, full set: the best of the five rounds' bests, as a ratio.

| Model | heap/base plain | heap/base PGO | base/main plain | base/main PGO |
|---|---:|---:|---:|---:|
| bosl_fractal_tree | 1.009 | 0.991 | 0.984 | 0.995 |
| bosl_gears__003 | 1.000 | 0.995 | 0.987 | 1.005 |
| bosl_isosurface__006 | 1.015 | 1.014 | 0.985 | 0.996 |
| bosl_screws__001 | 1.007 | 0.995 | 0.990 | 1.010 |
| bosl_spring_handle | 0.980 | 1.022 | 1.030 | 0.978 |
| csg_deep_union | 1.027 | 1.005 | 0.976 | 1.015 |
| csg_spheres | 0.984 | 1.010 | 1.014 | 1.003 |
| ex_menger | 0.994 | 0.996 | 1.005 | 1.000 |
| extrude_twist | 0.934 | 1.015 | 1.044 | 1.000 |
| import_stl | 0.985 | 1.035 | 0.988 | 1.008 |
| text_30lines | 0.969 | 1.031 | 1.008 | 0.956 |
| **Geomean (11 models ≥ 30 ms)** | **0.991** | **1.010** | **1.001** | **0.997** |
| Per-round paired geomean | 0.937–1.005 | 0.973–1.021 | 0.985–1.008 | 0.982–1.032 |

- Parity throughout, inside the ±3% layout band of §4.4, and well
  inside the owner's 5%. The models outside ±2% vary on both sides:
  - `import_stl` and `text_30lines` are 1.03 under PGO and 0.97–0.99
    plain, and their time is geometry;
  - `extrude_twist` is 0.93 plain.
- The evaluation-bound models of the kill criterion (isosurface,
  fractal_tree, screws, spring_handle) are 0.991–1.022 under PGO.
- The feature-off build matches main: 1.001 plain and 0.997 PGO.
- `eval_only` (BOSL2's 976 tests, summed), median of five, and the best
  of five in brackets:

  | Build | Main | Base | Heap |
  |---|---:|---:|---:|
  | Plain | 31.02 s | 30.74 s (29.78) | 30.87 s (29.88) |
  | PGO | 28.92 s | 28.86 s (28.16) | 29.54 s (28.50) |

  That is +0.4% plain and +2.4% PGO (+1.2% on the best).
- **The BOSL2 corpus**, echo, process time summed over its 3512 files, in
  one A/B run:
  - base 263.1 s against heap 264.7 s, plain (+0.6%);
  - in the correctness sweep, under a load of 4–10: 286.2 s against
    287.0 s echo, and 293.7 s against 293.4 s csg.

  The PGO and main pairs did not run: the machine's load stayed at
  12–52 from other work.
- **The served edit loop** (`serve`, best and median):

  | Build | BOSL2 render, base | BOSL2 render, heap |
  |---|---|---|
  | Plain | 9.1 / 9.8 ms | 9.3 / 9.9 ms |
  | PGO | 8.7 / 9.2 ms | 8.6 / 9.2 ms |

  Snapshots and the CSG case agree within 0.3 ms. Cold start is 2.9 ms
  in all builds.
- **The app core** (stage 1's `DocumentController` harness, 20 edits a
  round, best of rounds and median of round medians):
  - BOSL2 preview: 8.44/8.89 ms base, 8.38/8.89 ms heap;
  - BOSL2 render: 8.40/8.90 ms base, 8.42/8.96 ms heap;
  - CSG: under 1 ms on both.
- **The web worker in node** (`profile.web`), from request to result,
  best and median:

  | Case | Base | Heap |
  |---|---|---|
  | CSG preview | 3.41 / 3.72 ms | 3.46 / 4.06 ms |
  | Module tree preview | 9.99 / 10.55 ms | 9.93 / 10.67 ms |
  | Module tree render | 142.6 / 148.0 ms | 143.4 / 149.1 ms |
  | Function-heavy preview | 0.77 / 0.85 ms | 0.82 / 1.07 ms |
  | Function-heavy render | 0.87 / 0.92 ms | 0.98 / 1.01 ms |

  - The mesh copies take 0.004–0.19 ms on either side.
  - The function-heavy case is new: a polygon from a 120-deep non-tail
    recursion and a 170-deep sum. It is 7–13% slower on the best. Both
    recursions run mostly past the 8 native levels, where a call costs
    1.3–1.6 times the native one.
- **Deep recursion is where the heap costs.** In instructions retired
  (echo export, plain):
  - a leaf call in a loop: +0.9%;
  - a tail recursion: +0.1%;
  - a loop with no calls: −0.3%;
  - `fib(25)`: +28%;
  - `1 + f(n - 1)` 5,000 deep: +36%.

  No bench model recurses that way.

**Can it become the default?** On correctness, yes: everything above
holds with the feature on. On speed it meets the owner's bar: parity on
the bench models, the corpus, `eval_only`, the edit loops and the app
core. Deep non-tail function recursion is the exception. It runs
1.3–1.6× slower past 8 levels, a cost in exchange for having no depth
ceiling. The step left before switching is the plan's stage 5: run the
gate with the roles swapped, and keep the recursive driver behind
`recursive-eval` for one release.

**What turning it on everywhere retires** (§5), now that both halves are
on the heap:
- **The worker's stack probe and the per-kind weights.** Under the
  feature the probe is already skipped. No recursion reaches the frame
  budget: the native stack holds at most 8 call levels, plus the
  source's own nesting. The budget remains only as a guard for that
  nesting, which the parser bounds first, in every browser alike (see
  "Browsers"). So `FrameWeights`, `set_frame_weights`, the probe and the
  builtin-module ¼ margin can go. A single frame count for source
  nesting is enough until the parser has a depth limit.
- **The PGO depth guard in CI.** `conformance depth` gives the same
  numbers on plain and PGO builds. The guard becomes an identity check,
  and PGO no longer trades depth.
- **The native stack measure for recursion** (`stack_used` in
  `recursion_exhausted`, the 64 MiB `DEFAULT_STACK_LIMIT` and its PGO
  rationale), but not the stack itself:
  - the parser, `Unit::add_scope` and source nesting still recurse;
  - so do the printer, and values nested deep (see below).

  `with_stack`'s per-request thread could shrink to what those need. It
  cannot go.
- **PGO exclusions.** None retire through depth. The DMG's core still
  needs an ffi training run, and Windows arm64 the toolchain fix (§0.2).
  What this removes is the depth cost that PGO would have had there.
- The memo's stack condition (`replay_fits`) already compares module
  depth under the feature.

**Left, and followups** (`docs/followups.md`):
- A few rare shapes stay native even on the heap:
  - ranges;
  - callees that are expressions;
  - methods;
  - C-style `for`;
  - `object()` and `is_undef()` arguments;
  - parameter defaults;
  - `use`d libraries' assignments.

  A recursion through one of them at every level still uses native
  stack per level, and still stops with the frame budget's clean error.
- The heap path's per-call cost, for deep non-tail recursion.
- Values nested as deep as the limit can now be built in a browser.
  Dropping and printing them recurses on the value's depth (§6, "Value
  depth"). That is unmeasured in WebKit.
- The `may_call` share of the corpus is not counted (`resolve::Stats`).
- The memory estimate still does not charge heap frames: 41 MB for a
  function recursion at the default depth, 100 MB through a
  comprehension.

## Feature removed

Done on 2026-10-02 against `bebdbc7`, on the same Apple M4 Pro. The
owner chose (2026-10-01) to turn the heap evaluator on everywhere, and
to delete the recursive driver now rather than keep it behind a
`recursive-eval` feature for a release (§3, stage 5).

**What went.**
- The recursive statement driver: `instantiate`, `user_module` and
  their halves, `instantiate_scope`, `instantiate_children` and
  `builtin_recursion` (`crates/eval/src/inst.rs`), and `with_children`,
  `builtin_module`, `part_module`, `children_module` and the geometry
  modules' driver (`builtins/modules.rs`). With them went the builtin
  modules' quarter-margin frame check and the geometry frame weight.
- Every `cfg(feature = "heap-eval")` in `neoscad-eval`, and the
  feature itself, in `neoscad-eval` and in the five crates that
  forwarded it (cli, ffi, linux-app, wasm-check, web). In all,
  `crates/eval/src` and its `Cargo.toml` lose 874 lines and gain 241,
  mostly documentation rewritten for one evaluator.
- `NEOSCAD_NO_DEFAULT_FEATURES` and `NEOSCAD_FEATURES` in
  `scripts/wasm-check.sh` and `scripts/web/build-core.sh`: both came in
  with the feature (`8f0fe78`), and the two crates they fed have no
  features left. `scripts/pgo.sh` and `scripts/apple/build-core.sh` keep
  `NEOSCAD_FEATURES`; their crates still have features.
- The frame weights. Statements take no native stack, so nothing tunes
  the budget per kind any more: the budget counts expression, call and
  comprehension frames with the constants. `eval::recursion` keeps thin
  shims for the web core, which another change was editing at the time:
  `HEAP_EVAL` (always true), `set_frame_weights` (ignored),
  `frame_weights` (the constants) and `frames_at_last_check` (always
  0). `docs/followups.md` lists what to remove on the web side, and then
  these.

**The native checks, kept and removed.** The stack measure and the
frame budget (`recursion_exhausted`) now guard only what still recurses
natively.
- Kept in `eval_call`'s `call_exhausted`, which every function call
  passes: the first `NATIVE_CALLS` levels, and the shapes that stay
  native and start a nested heap loop per level. Measured natively, a
  recursion through `is_undef()` stops cleanly at 61,667 levels and one
  through a C-style `for`'s initialiser at 49,332, the same as before
  and short of the counted 100,000.
- Kept in printing (`print_stack_exhausted`), whose depth is the value's.
- Kept as they were: the parser's `NESTING_LIMIT` and `Unit::add_scope`
  (source nesting).
- Removed from `heap::begin_user`, the module call. The statement
  driver only starts from a top-level statement (`memo.rs`), never from
  inside an expression, so at a module call the native stack is the
  driver's own at any depth and no expression frame is held. The check
  could only fire for a `frame_limit` of 0.
- Removed from `module_call_text`, which writes `...` for a module's
  parameters near the limit as OpenSCAD's `print_trace` does at its
  stack check: the counted limit is that check now, for the same
  reason.
- `memo_depth` is the module depth alone; the native-stack variant
  went with the recursive driver.
- `DEFAULT_STACK_LIMIT` stays at 64 MiB. Its PGO rationale is gone, but
  it is what the native shapes above reach; shrinking it and
  `with_stack`'s thread is a followup.

**The PGO depth guard is an identity check.** `conformance depth` now
holds a neoscad binary to the exact depths of the counted limit (§4.1's
"identical N across builds"), and only another binary (OpenSCAD, to
check the harness) to 1.25 times the nightly's. A program check needs
two runs (at its depth, and one past it) instead of a bisection: the
whole command takes 1 s. Plain, PGO with either training, and `bebdbc7`'s
PGO build all report the same:

| Check | Every build |
|---|---:|
| `recursion-test-module` | 199,977 |
| `recursion-test-vector` | 199,977 |
| `recursion-test-function3` | 99,977 |
| `module-if` | 99,999 |
| `function-add` | 99,999 |
| `issue4172` (not gated) | 301 |

**The PGO training gained a deep-recursion model** (`DEEP` in
`scripts/pgo-train.py`, to `.echo` and STL): non-tail, branching, `let`,
comprehension, `each` and function-literal recursions 10,000 to 20,000
deep, one through a range's bounds, and module recursion through a
transform, `children()` and a block. Fixed sizes and no `rands()`, about
0.2 s natively. Its effect is below what one training per variant can
show:
- on the deep recursions themselves (best of 7), the PGO builds with and
  without it are within 2%: `fib(25)` 27.9 ms both, the model 188.0
  against 188.9 ms, `n + f(n - 1)` 50,000 deep 63.4 against 64.6 ms;
- on the bench (below), PGO gained 6.7% over plain with it and 4.8%
  without, but the two PGO builds' per-round ratio ranged 0.87–1.07.

**Speed.** Six interleaved rounds of `conformance bench --quick --refs
neoscad` over five binaries: `bebdbc7` plain and PGO + thin ("base"),
and this change plain, PGO, and PGO trained without the new model. Each
round waited for a 1-minute load below 4 (up to 15 minutes). The load
at the runs' starts was 2.7–17, from other work on the machine. The
eleven models of at least 30 ms, best of the six rounds' bests:

| Ratio | Geomean | Per-round paired |
|---|---:|---|
| new / base, plain | 0.991 | 0.967–0.997 |
| new / base, PGO | 0.998 | 0.873–1.070 |
| PGO / plain, base | 0.926 | 0.883–0.993 |
| PGO / plain, new | 0.933 | 0.886–1.028 |
| PGO / plain, new, old training | 0.952 | 0.930–1.116 |

`eval_only` (BOSL2's 976 tests, summed), median of six and the best in
brackets: base 32.74 s (31.93) plain and 31.70 s (30.34) PGO; this
change 33.08 s (32.15) plain and 30.91 s (30.33) PGO. The served BOSL2
edit loop's render is 9.2–10.8 ms best in every build.

**Checks.** `cargo fmt`, `clippy --all-targets -D warnings`, `cargo
test --workspace`; conformance 1773/0 at default threads and with
`RAYON_NUM_THREADS=1`; `scripts/wasm-check.sh --depths` (functions and
modules 99,999; source nesting stops at the parser's limit); `node
crates/web/test/run.mjs` on a core built from this change, its probe
test included.
