# Audit: a bytecode VM for the evaluator (spike at `cd7d5c7`)

The question: is a bytecode compiler plus VM for expressions and function
bodies worth building? A prototype was built behind an off-by-default
switch, checked against the tree-walker for exact output, and measured
against the tree-walker and against a targeted alternative.

## Recommendation

**No-go on a second evaluation engine. Go on a hybrid: move the VM's
analysis into the tree-walker.**

- **The VM is 1.19–1.35× faster on evaluation-bound BOSL2 models**
  (isosurface, the hero, fractal tree, screws, spring handle; wall time
  of the whole process). It runs 1.3–1.5× fewer instructions. On
  micro-benchmarks it is 1.2–2.2× faster. It is identical in output on
  everything tested, and compiling costs 0.4 ms per program.
- **The bytecode is not where the gain comes from.** With every `let` and
  `for` variable kept in a real context, the VM's dispatch loop is no
  faster than the recursive tree-walker (0.95–1.08×; §3.4). The gain comes
  from **escape analysis**:
  - `let` and comprehension variables live in registers rather than a
    heap context each;
  - calls that bind only positional parameters get a pure frame in
    registers, with no frame context;
  - builtin callees are resolved once at compile time.

  None of these needs bytecode. The tree-walker's cost is allocation,
  reference counting and chain walks, not dispatch.
- **A second engine is a standing tax.** Every change to warnings, limits,
  traces, the frame budget or the memo would need doing twice. The
  prototype needed a differential fuzzer to stay exact (§4.4). Even
  switched off, its presence makes the tree-walker 0–3% slower, because
  the functions the two engines share are inlined differently (§3.6).
- **The targeted alternative alone gives 1.02–1.11×** on the same
  models (1.23× on builtin-heavy code; §5). It takes 2–4
  days and stays exact.

The hybrid, in order:

1. Land the targeted call-path fixes, T1–T5 in §5 (1.02–1.11×).
2. Port the prototype's frame plan into the tree-walker. The pass that
   decides which regions can be registers (`compile.rs`'s `scan`) and the
   pure-frame rule (`exec.rs`'s `pure_bindable`) would drive a per-call
   register window in `eval_call`, `eval_cold`'s `let` and `for_each`.
   Estimate: 1.2–1.35× on BOSL2 evaluation in total, which is most of the
   VM's gain, with one engine.
3. Keep the VM in the tree, switched off, until step 2 lands. Its fuzzer
   and `vm_ab` then act as the oracle for the rewritten tree-walker. Then
   delete it.

A full VM (§6) would take 6–9 engineer-weeks. It would add maybe 1.1–1.3×
more than the hybrid, mostly on call-heavy code, from calls that do not
recurse natively and from fused builtins. That is worth revisiting only
if evaluation dominates the edit loop after the hybrid and the memo.

## 1. What was built

All of it is in `crates/eval/src/vm/` (`mod.rs` holds the types,
`compile.rs` the compiler and `exec.rs` the interpreter and call loop).
The switch is `Options::vm`, off by default (`crates/eval/src/lib.rs:224`).
The CLI turns it on with `NEOSCAD_VM=1` (`crates/cli/src/main.rs`). The
`vm-default` cargo feature turns it on for test runs. It is pure Rust
with no new dependencies, and builds and passes for wasm32 (§4.5).

- **Entry.** The tree-walker hands over in two places:
  - `eval_call` defers to `vm_eval_call` (`call.rs:386-388`), which runs
    the call's first step through the tree-walker's own `simplify` and
    then drives the callee's body (`exec.rs` `vm_drive`);
  - `eval_cold` hands a vector containing a comprehension, or a bare
    comprehension, to a chunk of its own (`eval.rs:1322`, `eval.rs:1376`).

  Module instantiation and statements stay in the tree-walker.
- **Chunks.** A function body (a user function or a literal) is compiled
  at its first call, with OpenSCAD's tail positions: a ternary's branches,
  the body of a `let`, `assert` or `echo`, and a call in such a place
  replace the frame instead of nesting, as `simplify` does. It is cached
  per evaluation, keyed by (unit, expression, tail, pure). Instructions
  are a 16-byte `enum Op` (`mod.rs:92`). Values live on an operand stack,
  and lists are built into builders.
- **Registers.** A first pass (`compile.rs` `scan`) marks the regions that
  must stay real contexts (*materialized*). These are regions around:
  - a function literal (it captures its context);
  - a `$` binding (dynamic lookups walk the context stack);
  - a C-style `for`, or a name the resolver left (both looked up by name).

  Every enclosing region is materialized as well, so the chain stays
  whole. Every other `let` or `for` variable is a register. A variable
  read tries its register candidates, innermost first, then walks the
  context chain from the innermost real context. This is the tree-walker's
  resolved lookup, with the register regions taken out of the chain.
- **Pure frames.** A call gets its parameters in registers and makes no
  context when all of these hold (`exec.rs` `pure_bindable`,
  `vm_call_pure`):
  - its arguments are all positional, and no more than the parameters;
  - no parameter is a `$` name;
  - the dying frame has no `$` variables to copy (tail calls);
  - the body compiles with the frame in registers.

  `bind_user` prints nothing for such a call and binds only slots, so
  nothing observable changes. Any other call binds a real context with
  the tree-walker's `bind_user`.
- **Exactness mechanisms.**
  - *Frame budget.* The compiler computes, per instruction that can
    observe it, the frames the tree-walker would hold there, and charges
    them around recursion checks, builtins and printing.
  - *Checks.* `check_hard` runs after every value-producing instruction,
    as it runs after every `eval`. Recursion and interrupt checks run at
    the same points as `eval_call`'s.
  - *Traces.* A call being argued is a *pending* record, and an error in
    its arguments is traced with its name (`vm_run_in`, `exec.rs:340`).
    An activation traces with its current tail call.
  - *Accumulators.* A tail call's accumulator is moved out of a register,
    or out of the frame context when only the call loop holds it.
    Otherwise it is copied.
- **Fallbacks.** A body nested deeper than 400 expressions is not compiled
  (`compile.rs:24`), and the tree-walker steps it. A C-style `for`
  comprehension runs in the tree-walker inside a chunk (`EvalLc`).

## 2. Method

- **Hardware and runs.** M4 Pro. Interleaved A/B, 5 runs each, best of 5.
  The median agrees within 1%.
- **Configurations.** All measured on the `neoscad` CLI, exporting `.echo`
  (parse and evaluate only), with `OPENSCADPATH=.reference` and
  `NEOSCAD_NO_SERVER=1`:
  - *base*: HEAD (`cd7d5c7`) built unchanged;
  - *off*: this tree with the switch off;
  - *tgt*: HEAD plus the targeted changes T1–T5 of §5, built from a
    scratch copy with its own target directory;
  - *vm*: this tree with `NEOSCAD_VM=1`.
- **Instruction counts.** "Instructions retired" from `/usr/bin/time -l`.
- **In-process runs.** `crates/eval/examples/vm_ab.rs` runs both engines
  in one binary, interleaved. It compares every message (with its line)
  and the `.csg` of the tree byte for byte, and prints the VM's counters
  (`Evaluation::vm`, `lib.rs:405`).
- **Models.** `bosl_*` are the bench models (`conformance/bench.json`);
  the hero is `apple/Icon/hero.scad`. The micro cases are the
  `eval_bench` scripts, plus three for calls, builtins and `let`.

## 3. Measurements

### 3.1 Models and micro-benchmarks

Wall time of the process (best of 5), with the speedup over *base*:

| Model or case | base | off | tgt | vm | instructions, base → vm |
|---|---|---|---|---|---|
| isosurface__006 | 807 ms | 814 ms (0.99×) | 729 ms (1.11×) | **598 ms (1.35×)** | 17.67G → 11.77G (1.50×) |
| hero (`apple/Icon/hero.scad`) | 1634 ms | 1634 ms (1.00×) | 1524 ms (1.07×) | **1237 ms (1.32×)** | 34.67G → 23.53G (1.47×) |
| fractal_tree | 3594 ms | 3609 ms (1.00×) | 3325 ms (1.08×) | **2967 ms (1.21×)** | 69.78G → 51.51G (1.35×) |
| screws__001 | 145 ms | 146 ms (0.99×) | 138 ms (1.05×) | **122 ms (1.19×)** | 2.79G → 2.12G (1.32×) |
| spring_handle | 153 ms | 154 ms (1.00×) | 145 ms (1.05×) | **124 ms (1.23×)** | 3.08G → 2.29G (1.35×) |
| gears__003 | 43.6 ms | 43.1 ms (1.01×) | 42.6 ms (1.02×) | **41.7 ms (1.05×)** | 0.65G → 0.59G (1.10×) |
| fib(24) | 21.2 ms | 21.3 ms (1.00×) | 20.7 ms (1.03×) | **17.1 ms (1.24×)** | 0.40G → 0.30G (1.34×) |
| tail loop 9e5 | 103 ms | 104 ms (0.98×) | 93.4 ms (1.10×) | **66.5 ms (1.54×)** | 2.49G → 1.51G (1.64×) |
| list comp 9e5 | 112 ms | 114 ms (0.99×) | 109 ms (1.03×) | **79.8 ms (1.41×)** | 2.17G → 1.35G (1.61×) |
| nested for 1000×1000 | 47.0 ms | 47.1 ms (1.00×) | 43.1 ms (1.09×) | **21.0 ms (2.24×)** | 1.10G → 0.47G (2.33×) |
| string build 20000 | 16.0 ms | 16.3 ms (0.98×) | 15.4 ms (1.04×) | **15.4 ms (1.04×)** | 0.27G → 0.25G (1.10×) |
| `concat` accumulator 1e5 | 27.9 ms | 27.6 ms (1.01×) | 26.3 ms (1.06×) | **20.1 ms (1.39×)** | 0.57G → 0.37G (1.53×) |
| `each` accumulator 1e5 | 23.4 ms | 23.5 ms (0.99×) | 22.5 ms (1.04×) | **14.9 ms (1.57×)** | 0.47G → 0.26G (1.84×) |
| modules 100k | 60.0 ms | 61.2 ms (0.98×) | 59.9 ms (1.00×) | **61.0 ms (0.98×)** | 0.93G → 0.93G (1.00×) |
| `f(i)` with 3 `let` bindings, 3e5 | 68.8 ms | 70.8 ms (0.97×) | 66.0 ms (1.04×) | **45.6 ms (1.51×)** | 1.60G → 0.90G (1.77×) |
| `min`/`max`/`abs`, 3e5 | 55.9 ms | 56.1 ms (1.00×) | 45.4 ms (1.23×) | **46.0 ms (1.21×)** | 1.39G → 1.00G (1.39×) |
| `f([...])` with a comprehension, 1e5 | 40.0 ms | 40.6 ms (0.98×) | 37.7 ms (1.06×) | **27.2 ms (1.47×)** | 0.86G → 0.53G (1.63×) |

- **Where the VM helps most.** Comprehensions and calls: nested `for`
  2.2×, tail loop 1.6×, `let` inside a called function 1.5×.
- **Where it does nothing.** Module instantiation (`modules100k`, 1.00×),
  which stays in the tree-walker. `gears__003` (1.05×), which is mostly
  `include` parsing and module work.
- **Builtins.** `builtins3e5` gains the same under *tgt* as under *vm*:
  the whole difference there is the call path into the builtin.
- **In-process.** `eval_bench` with and without `--features vm-default`,
  system allocator: fib(24) 18.4 → 12.5 ms, tail loop 126 → 60 ms, list
  comp 111 → 75 ms, nested for 45 → 14.6 ms, `concat` accumulation 100k
  28.9 → 17.5 ms, `each` accumulation 100k 23 → 10.5 ms, modules 60 →
  61 ms.

### 3.2 Compile time

Summed over each evaluation, across all 3,590 BOSL2 files that parse
(`vm_ab --only vm --runs 1`):

| | median | p90 | p99 | max |
|---|---|---|---|---|
| Compile time per program | 0.42 ms | 0.73 ms | 1.02 ms | 1.48 ms (`ex__BOSL2logo`, 244 ms of evaluation) |

This is negligible against the 15 ms edit loop. Chunks are compiled per
evaluation. Keeping them across a session's evaluations (keyed as the memo
keys definitions) would remove even that.

### 3.3 Fallback rate on BOSL2

Over the same 3,590 files:

- **Expressions.** 27.98 M expression nodes compiled (counted once per
  evaluation), of which 162,026 (0.58%) are fallbacks inside chunks
  (C-style `for` comprehensions and everything in them).
- **Bodies.** No body was too deep to compile.
- **Regions.** 1,017,190 compiled to registers; 16,928 materialized
  (1.6%), because of function literals and `$` bindings.
- **Dynamically.**
  - 382 M function-body activations ran in the VM.
  - 18.0 M call steps ran in the tree-walker (4.5%). These are the first
    step of calls the tree-walker makes (from statements, module
    arguments and defaults), which `vm_eval_call` runs through
    `simplify` so that the binding is exactly the tree-walker's.
  - 0.71 M fallback comprehensions ran.

### 3.4 Where the gain comes from (isosurface, the hero)

Cumulative, measured with a scratch build that has environment knobs
(not in the tree). Each row is relative to that build's tree-walker:

| Configuration | isosurface | hero | fib(24) | `let` in calls |
|---|---|---|---|---|
| Bytecode, every region a real context, no pure frames, no static builtins | 1.08× | 1.08× | 0.95× | 1.00× |
| + registers for `let` and `for` | 1.23× | 1.22× | 0.95× | 1.24× |
| + builtin callees resolved at compile time | 1.26× | 1.26× | 0.96× | 1.24× |
| + pure frames (the full VM) | 1.39× | 1.34× | 1.18× | 1.47× |

The first row already includes two call-path fast paths that the
tree-walker can have as well (T2 and T3 of §5): positional binding and
reused argument vectors. The prototype's first version had neither, and
with every region real it measured 0.95–1.02× against the tree-walker.
Bytecode dispatch alone is therefore worth nothing here.

Profiles (xctrace, 1 ms samples) of the VM on isosurface give:

| Share | What |
|---|---|
| 20% | The interpreter loop itself |
| about 9% | The allocator (malloc and free) |
| about 6% | Value drops |

The rest is spread over lookups, builtins (`min_max`, `index`) and
binding. Dispatch was about 29% of the tree-walker's profile in
`performance.md` §2.4, and the VM's loop is not much cheaper per node. What
it removes is the context per `let`, per iteration and per call, and the
chain walks through them.

### 3.5 Profile of the tree-walker for comparison

On isosurface at HEAD, self time is spread thin. The top entries are:

| Share | What |
|---|---|
| 7.8% | `eval_expr` |
| 5.2% | `drop_glue<Value>` |
| 5.1% | Result branching |
| 4.7% | `find_binding` |
| 4.1% | `Unit` slice indexing |
| 3.0% | `mi_malloc` |
| 2.6% | `find_function` |
| 1.0% | `min_max` |
| 1.4% | `ops::index` |

The builtins' own work is under 5%.

### 3.6 The cost of keeping the VM in the tree, switched off

The *off* column: 0–3% slower in wall time, 0.2–1.5% more instructions.

- The switch's own checks cost nothing measurable. A build with them
  compiled out kept the whole cost, and a build with a stub VM lost it.
- The cost comes from LLVM inlining shared functions differently once the
  VM also calls them. `simplify`, `call_builtin`, `lookup_function`,
  `new_ctx_in`, `function_region` and `expr_loc` had been inlined into
  `eval_call`. Shared, they were not, which cost 4.5% instructions on
  fib.
- Giving the VM its own monomorphized copies (`simplify::<VM>`, and so
  on) and forcing the small helpers inline brought it down to the figure
  above. The attributes carry comments saying why.

A second engine keeps perturbing the first this way.

## 4. Correctness

All of these were run with the final code.

### 4.1 Conformance

`conformance run` gives 1,719 passes with `NEOSCAD_VM=1`, the same as
with it off. The same 10 image cases fail either way (pre-existing).

### 4.2 BOSL2 corpus diffs

`conformance diff --format echo` and `--format csg` compare the VM against
the tree-walker directly, with wrapper scripts as `--binary-ref` and
`--binary`, `--library-path .reference .reference/BOSL2`:

- **echo:** 3,597/3,597 identical.
- **csg:** 3,596/3,597. The one difference is
  `examples_x/isosurface__022.scad`, which calls unseeded `rands()`, and
  the CLI seeds from entropy per process. With a fixed seed, in process
  (`vm_ab`), the two engines are identical on it.

Process time over the corpus: 405 s for the tree-walker against 354 s for
the VM (parse, startup and export included).

### 4.3 The eval tests and the workspace

- **The whole workspace, both ways.** `cargo test --no-fail-fast` gives
  533 passed, 0 failed. With
  `cargo test --no-fail-fast --features neoscad-eval/vm-default` (the VM
  on for every test in the workspace, `semantics.rs` and `incremental.rs`
  included), the same: 533 passed, 0 failed.
- **An incremental shard.** `random_edits` with `NEOSCAD_REUSE_CORPUS=full
  NEOSCAD_REUSE_SHARD=0/40`: 104 files and 1,248 edits consistent
  between memo reuse and fresh evaluation. The reuse counts are the same
  as the tree-walker's, and so is the peak RSS (3.14 GB against 3.15 GB,
  under the harness's 4 GB cap).

### 4.4 The differential fuzzer

`crates/eval/tests/vm_fuzz.rs` generates random programs and compares
both engines byte for byte: messages with their lines, the `.csg`, and
the aborted and hard-warning flags.

- **What the programs contain:**
  - functions with defaults, and tail and non-tail recursion templates
    with `concat` and `each` accumulators;
  - comprehensions of every kind, including C-style;
  - `let` with duplicate and `$` bindings;
  - function literals, including ones capturing a frame whose
    accumulator a tail call must not move;
  - `$` variables through calls;
  - builtins with wrong types and arities;
  - asserts, `echo`, ranges, member access, and unbounded recursion.
- **Configurations:**
  - a 3,000-frame budget;
  - `--hardwarnings`;
  - frame budgets of 40, 90 and 200, which stop evaluation part way
    through ordinary expressions.
- **Result.** 5,000 programs per configuration (25,000 pairs), all
  identical. About a quarter of them end in an error, so traces are
  compared too.
- **Mutations it catches:**
  - a frame charge off by one;
  - a dropped duplicate-`let` warning;
  - an accumulator moved without the ownership test.

  It does not catch a dropped `check_hard` after a literal, which only
  matters under `--hardwarnings`.

The fuzzer's first version exhausted the machine's memory (§8). It is now
bounded several ways:

- a 64 MiB memory limit, and list and string limits, under which both
  engines must stop identically;
- a 5 s watchdog per evaluation (an interrupted pair is not compared);
- a background thread that exits the process past 2 GB resident;
- a generator that keeps ranges, nesting and recursion small.

Peak RSS is 160 MB for 25,000 pairs.

### 4.5 wasm32

`scripts/wasm-check.sh` passes. With the VM on
(`--features eval/vm-default`), all 18 cases pass as well, and the frame
budget stops recursion at the same depths as the tree-walker (function
498, module 249).

Where V8's stack overflows, with the budget lifted
(`--depths --all-programs --frames=4000000000`):

| Recursion | VM | Tree-walker, this tree | HEAD |
|---|---|---|---|
| function | 1,051 | 1,439 | 1,334 |
| function-lc | 1,049 | 362 | 354 |
| function-nested | 1,051 | 781 | 749 |
| module | 321 | 321 | 321 |

The default budget's 498 is 47% of the VM's limit, inside the 63% margin
`recursion.rs` calibrates for.

At HEAD, the tree-walker's module-children recursion already stops at 206
of a 214 trap. That is outside the documented margin, and not about the
VM.

## 5. The targeted alternative

These changes make the tree-walker's calls and scopes cheaper, with no
function-name special-casing:

- **T1: builtin callees resolved at compile time.** In `find_function`: a
  name no candidate can take ends at the builtin context.
- **T2: reused argument vectors.** `call_builtin` takes its argument
  vector from `arg_pool`, and `apply_builtin` borrows it.
- **T3: positional binding.** A fast path in `bind_user` for positional
  arguments into slots. It prints nothing and binds the same.
- **T4: a direct call for builtins.** A call that is always a builtin
  skips `eval_call`'s frame slot and loop.
- **T5: reused contexts.** Contexts of `let`, `for` iterations and call
  frames are recycled when nothing else holds them.

The prototype (about 200 lines, in a scratch copy of HEAD, not in this
tree) passes `conformance run` with 1,719. Its gains are in the *tgt*
column:

- isosurface 1.11×, hero 1.07×, fractal tree 1.08×, screws 1.05×,
  spring handle 1.05×, gears 1.02×;
- builtin-heavy code 1.23×;
- `let`, call and loop micro-benchmarks 1.03–1.10×.

Estimated effort to land it properly, with the edge-case tests the D1 and
R3 commits have: 2–4 days.

**The hottest builtins.** Making `isosurface()`'s builtins themselves
faster (`min`, `max`, `norm`, indexing, vector arithmetic) cannot give
much: their own work is under 5% of the profile (§3.5). What costs is
calling them:

- the lookup walk, which T1 removes;
- the argument vector, which T2 removes;
- the frame slot and the call loop, which T4 removes.

## 6. What full coverage would take

Estimates for one engineer who knows this codebase; agent time differs.

| Item | Estimate | Expected gain |
|---|---|---|
| Statements and module instantiation in the VM: module bodies, `children()`, `for`/`if`/`let` statements, assignments. This is where fractal tree and gears spend their time. | 2–3 weeks | 1.1–1.2× on module-heavy models |
| Calls without native recursion: a VM frame stack, with the stack limit emulated deterministically | 1 week | removes the per-call re-entry cost (fib is 1.2× now; about 2× expected) |
| Fused builtin and vector instructions (no pending record for known builtins, numeric vectors on the stack) | 1 week | |
| C-style `for`, `$` bindings in registers (a dynamic-scope side stack), closure conversion for function literals | 1–2 weeks | removes the last fallbacks |
| Chunks kept across a session's evaluations | 3 days | |
| Isolating the tree-walker's code generation from the VM's use of it | 3 days | |
| Tests: extending the fuzzer to statements and modules, VM depth calibration on wasm, limits parity | 1 week | |

In total, 6–9 weeks. The expected result, extrapolated from §3.4 and not
measured, is 1.4–1.8× on BOSL2 evaluation.

## 7. Risks

- **Semantic drift.**
  - Every observable was matched by hand: warning order, `--hardwarnings`
    check points, trace attribution, frame charges, interrupt and limit
    checks, the tail-call limit, accumulator moves.
  - The next change to any of them (a new builtin warning, a new limit, a
    memo rule) must be made in both engines. Otherwise the VM has to keep
    delegating to the tree-walker's functions, which is what costs the
    tree-walker its inlining (§3.6).
  - The fuzzer and the corpus diffs catch drift only where they reach.
- **Memory-limit timing.** The live-bytes estimate is sampled at interrupt
  checks. Registers drop their values at scope exit, as contexts do. A
  program near a memory limit could still see a different point of
  failure if a lifetime differs. The fuzzer ran thousands of programs up
  to the 64 MiB limit with no difference, but nothing proves it.
- **Native stack.** Natively the VM recurses deeper before the measured
  stack limit. The error is the same, but the "Excluding N frames" count
  in its trace differs. That count already depends on the build.
- **Maintenance.** The prototype is 2,850 lines plus a 720-line fuzzer,
  against the tree-walker's roughly 3,000 lines of expression and call
  code.

## 8. The fuzzer's memory incident

The fuzzer's first version grew to 41 GB (71 GB at peak) and filled swap
before the coordinator killed it. Its programs had no resource limits,
and three tests ran in parallel. Two kinds of generated program caused
it, and **both engines behave the same way on them**. There is no VM leak.
Running each engine alone, resident memory tracked within 1% over 1,600
programs.

- **A shared list tree, materialized.** A tail call doubled a list at
  every step (`f2([each false, p0, for (...) p0], ...)`). Shared, that
  costs nothing. An element-wise operation such as unary minus then
  materializes all 2^n elements. The evaluator's memory estimate does not
  count lists shorter than `LIST_MIN` (1,024; `limits.rs:560`,
  `value.rs:421-425`), so a tree of 2-element lists never trips
  `--limit memory`.
  - Reproduced with the tree-walker on the CLI: negating
    `f([1], 26)`, where `function f(p, n) = n == 0 ? p : f([p, p],
    n - 1);`, with `--limit memory=64`, passed 1.1 GB before an external
    guard killed it.
  - This is a gap in the resource limits, independent of the VM, and
    worth a followup.
- **A million tail steps, each printing a warning.** Kept by the output
  collectors, and twice over when comparing.

## 9. Not verified

- **Other hardware.** Speedups on anything but the M4 Pro, and the
  tree-walker's inlining sensitivity (§3.6) with other rustc versions.
- **The hybrid's estimate** (1.2–1.35×) is extrapolated from §3.4 and not
  built.
- **The serve path and the app** were not measured with the VM. The memo
  path was only exercised by `incremental.rs`.
- **wasm speed** with the VM on. Only correctness and depths were checked.
- **A dropped `check_hard` after a literal** under `--hardwarnings` would
  show only after a `let` with a duplicate name followed by a tail call
  with literal arguments. It is reasoned about but not tested.

## 10. Reproduction

- **Switch the VM on.** `NEOSCAD_VM=1 target/release/neoscad FILE -o
  x.echo`, or `Options { vm: true, .. }`, or `--features
  neoscad-eval/vm-default` for tests.
- **In process:** `cargo run --release -p neoscad-eval --example vm_ab --
  [--runs N] [--only vm|tw] FILES` gives both engines, output comparison
  and counters.
- **The fuzzer:** `cargo test --release -p neoscad-eval --test vm_fuzz --
  --test-threads=1`, with `NEOSCAD_VM_FUZZ=N`, `NEOSCAD_VM_FUZZ_FROM`,
  `NEOSCAD_VM_FUZZ_ONLY=tw|vm` and `NEOSCAD_VM_FUZZ_MAX_RSS_MB`.
- **Not kept.** The A/B driver (`ab.py`), the ablation knobs, the targeted
  patch and the bisection builds were in the session's scratch directory.
