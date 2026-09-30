# Audit: performance opportunities not yet taken (at `e15eef7`)

Scope: what is left after `docs/audits/performance.md` (O1–O12 done except
O11 and the `dlopen`ed renderer, per its status block), `slow-cases.md`
(Menger, text, banded 2D unions), `unwind.md`, `bytecode-vm.md`, and
`docs/followups.md` ("Serve and session", "Performance"). Those are cited,
not re-proposed. Heavily extruded text is being worked on and is skipped.
No code was changed and nothing was built. Numbers come from
`progress/bench/20260930T054922Z-e15eef7.json` (M4 Pro, best of 3, ASCII
STL) unless marked *measured here* (one run of `target/release/neoscad`
while another agent was timing on the machine, so ±10%).

## Firm ground

- `bosl_fractal_tree` is the slowest benchmark model: 3.455 s wall, 5.9 s
  CPU (nightly Manifold 11.7 s). *Measured here:* evaluation alone
  (`-o ft.echo`) is 3.00 s of a 3.55 s render. It is one recursive
  statement, so the statement memo (O3) cannot help, and evaluation is
  single-threaded by design (`docs/architecture.md`, "Determinism").
- Served BOSL2 edit: 12.2 ms render, 21.2 ms snapshot; the CSG example
  1.0 and 10.2 ms. A snapshot costs about 9 ms over a render on both: a
  fixed floor, not model cost.
- Cold one-shot BOSL2 edit 38.9 ms against 12.2 ms served; the earlier
  audit put parsing and lowering BOSL2 at 15.3 ms of a 33.4 ms edit
  (`performance.md:439`), which the session's `FragmentStore` avoids and
  the one-shot command line cannot.
- `Value` is 16 bytes (`crates/eval/src/value.rs:25-37`: a tag and an
  `f64` or an `Rc`); the remaining evaluator layout items are in
  `followups.md` ("Performance") and are not repeated here.

## Opportunities, ranked

### P1. Memoise repeated module instantiations within one evaluation

- **Evidence:** `tree()` in `.reference/BOSL2/examples/fractal_tree.scad`
  runs 2,047 times at 11 distinct `(l, sc, depth)` argument sets (2^k
  identical calls per depth), and evaluation is 3.0 of 3.55 s. Arrays of
  identical parts (concept C's 36 `channel()`s, `performance.md` O11)
  repeat the same way. The statement memo (`crates/eval/src/memo.rs:1-50`)
  already has every piece: fingerprints by syntax plus transitively
  reachable names, value digests (`memo.rs:896-1000`), node renumbering
  and message relocation on replay (`memo.rs:1491`), and the "what always
  runs" list.
- **Change:** on a user module call, key on the definition digest, the
  argument values, the values of every `$` name the reachable definitions
  mention (`zrot_copies` sets `$idx`, so depth-k siblings split into two
  keys: 22 evaluations instead of 2,047), the reachable top-level names,
  and `children()` (start with calls without children). Replay the
  recorded subtree renumbered; refuse what the statement memo refuses
  (`rands`, imports, deprecation-once messages, errors, limits,
  `parent_module`). Record only above a cost threshold, under a budget.
- **Gain:** evaluation-bound recursive and patterned BOSL2 models in
  every host, the one-shot CLI and wasm included: fractal_tree from about
  3.0 s of evaluation to well under 0.5 s (estimate; the replay's cloning
  cost is the unknown). Nothing for the hero or isosurface.
- **Risk:** medium–high: dynamic `$` scope, `$parent_modules`, message
  order, memory. Output must equal a fresh evaluation; the randomised
  harness in `crates/eval/tests/incremental.rs` extends to it. Parity:
  none, the tree is identical by construction. **Effort:** L.

### P2. Profile-guided optimisation of the release and dist builds

- **Evidence:** the evaluator is a tree-walker whose remaining cost is
  branchy dispatch and context walks (`architecture.md`, "Evaluator
  performance"), where PGO's layout and inlining decisions help most.
  `Cargo.toml:62-67` uses `lto = "thin"`, no PGO. `unwind.md` measured
  5–9% for unwinding that a product decision blocks; PGO can win a
  similar amount without that decision.
- **Change:** `cargo pgo` with a training run over `conformance/bench.json`
  plus `eval_only`, in the release workflow, `aarch64-apple-darwin` first.
  Try `lto = "fat"` for `dist` at the same time (`web` has it).
- **Gain:** unverified here; typical for interpreters 5–15% on evaluation,
  less on kernel-bound models. Every workload except wasm.
- **Risk:** none to output (FP unchanged). CI complexity: six dist
  targets need per-target profiles or PGO on macOS only. **Effort:** M.

### P3. A persistent parse-and-lower cache for the one-shot command line

- **Evidence:** every `neoscad FILE` re-parses and re-lowers BOSL2
  (`crates/cli/src/run.rs:204-256`); the session's spliced fragments show
  what that saves (`followups.md`: 34 to 18 ms). `eval_only` runs 976
  processes in 30.3 s, 31 ms each, much of it BOSL2's parse.
- **Change:** in `crates/cli` only (the library rule keeps `std::fs` out
  of `lang`): an on-disk store of lowered fragments keyed by path, size,
  mtime, content hash and build id, in the user cache directory, spliced
  through `lang::fragment`. Off until measured; `--no-cache`. Cache the
  `resolve::named_arguments` scan (`followups.md`, O4) in the same file.
- **Gain:** about 15 ms of the 39 ms cold BOSL2 run (40%): agents that
  shell out per render, and the BOSL2 suite. Nothing for the session.
- **Risk:** low for output (content hash and build id in the key), but a
  stale-cache bug would be confusing: diff cached against fresh `.ast`
  and `.echo` over `examples/` and the BOSL2 tests. Serialising `Program`
  (`ExprId` arenas, spans) is the work. **Effort:** M–L.

### P4. Geometry keys: hash in parallel, or carry them with the memo

- **Evidence:** `Keys::new` (`crates/eval/src/dump.rs:710-729`) hashes the
  whole node tree serially on every render, CLI (`run.rs:643`) and every
  served request (`session/src/lib.rs:1570`, `:1862`), including subtrees
  the memo replayed unchanged. O6 replaced the text formatting; its
  "hash sibling subtrees on rayon" was not done (no rayon in `dump.rs`).
- **Change:** (a) `rayon::join` over sibling subtrees above a node count,
  folded in order (a Merkle hash gives the same bytes at any thread
  count); (b) later, keep each memo entry's per-node hashes and shift
  them by the replay's index offset.
- **Gain:** unverified; before O6 the hero's keys were 200 ms, and what
  remains is a share of its 294 ms warm re-render and of edits on
  VNF-heavy BOSL2. **Risk:** low (a 1-thread equality test).
  **Effort:** S for (a), M for (b).

### P5. Overlap evaluation with geometry, statement by statement

- **Evidence:** the session evaluates the whole program, then builds
  geometry (`session/src/lib.rs:1405-1470`, then `:1554`); the CLI too
  (`run.rs:633-707`). The renderer already takes several tops
  (`render_many`, `geom/src/evaluate.rs:824`) with ID blocks in tree
  order, and the memo delimits statements.
- **Change:** hand each completed top-level subtree to the renderer on
  the pool; buffer geometry messages until evaluation ends (OpenSCAD
  prints evaluation messages first). The root union waits for all.
- **Gain:** up to min(evaluate, geometry) on multi-statement files with
  both phases substantial (BOSL2 assemblies); nothing for one-statement
  models or the 12 ms edit loop. **Risk:** medium (message order,
  cancellation across stages, the shared time limit); parity unaffected
  with blocks in tree order. **Effort:** M–L.

### P6. The snapshot's fixed 9 ms: PNG compression level

- **Evidence:** `encode_png` (`crates/render/src/lib.rs:129-147`) uses
  `png` 0.18.1 at its default compression and filter for a 1024×1024 RGB
  image (`session/src/snapshot.rs:65`) after an RGBA-to-RGB copy. How
  much of the 9 ms is encoding is unverified; the rest is the GPU round
  trip (`render/src/offscreen.rs:246-266`, `:497`).
- **Change:** time the encode alone; if it dominates, `Compression::Fast`
  (fdeflate) and rows through a `StreamWriter` without the copy.
- **Gain:** a few ms per snapshot (the agent loop's most common request
  after render), thumbnails, QuickLook. **Risk:** none to parity: PNGs
  are not byte-compared with OpenSCAD's (they differ between GPUs,
  `followups.md` "Rendering") and stay deterministic. **Effort:** S.

### P7. wasm: enable `simd128` for the web profile

- **Evidence:** `scripts/web/build-core.sh` sets no `target-feature` and
  there is no `.cargo/config.toml`; the core runs Manifold, Clipper and
  mesh packing on one thread (`crates/web/src/lib.rs:21`), where
  autovectorised loops are the only parallelism.
- **Change:** `-C target-feature=+simd128` for the web profile and
  `wasm-opt --enable-simd`; diff the demo's outputs against native over
  `node crates/web/test/run.mjs`.
- **Gain:** unverified; kernel-bound models in `/try`. The demo's bigger
  known gap (3.8 s to show for 1.8 s of engine time: packing and
  transfer) is already in `followups.md` ("Web demo").
- **Risk:** low for results (simd128 has no FMA, so arithmetic stays
  IEEE-exact; keep `relaxed-simd` off). Browser support was not looked
  up. **Effort:** S.

### P8. Cache hits that deep-copy a mesh to relabel IDs

- **Evidence:** `rebase` (`geom/src/evaluate.rs:1350-1381`) clones the
  `ManifoldGeometry` and relabels whenever a hit's ID block moved, then
  writes the copy back; inserting a statement above shifts every later
  block, so each hit below copies once per shift.
- **Change:** store original IDs relative to their block and apply the
  base where IDs are consumed (colour runs, export). **Gain:** small: one
  copy per large hit per shifting edit. **Risk:** medium
  (`crates/session/tests/warm_export.rs` guards it). **Effort:** M.

### P9. Parallel evaluation of independent top-level statements

`performance.md` O13 listed this as L and high-risk. Two things changed:
the memo's fingerprint computes each statement's inputs, and its replay
renumbers nodes and relocates messages, which is what per-thread results
need to be merged in order. The evaluator is `Rc`-based and not `Send`
(`crates/eval/src/context.rs:36`), so each thread runs its own evaluator
with top-level assignments re-run (cheap, per the memo header). Gain only
on files with several heavy independent statements; not the hero,
fractal_tree or the edit loop. Do P1 first, which shares the machinery.
**Effort:** L. **Risk:** high.

## Considered and rejected

- **Speculative evaluation while typing.** The app waits 150 ms
  (`apple/App/Document/SCADDocument.swift:187`); `serve` supersedes
  (`docs/serve-protocol.md`, "Ordering and concurrency"). Starting earlier
  warms nothing: memo entries are keyed by text, so only the statements
  not being edited would be recorded, and those are already warm.
- **A cheaper preview path.** The preview is the real boolean by product
  decision (`performance.md` O13), where the image-space option is kept.
- **Streamed export.** `io::stl::write` builds one exact-size buffer
  (`crates/io/src/stl.rs:131-200`); streaming saves peak memory, not time.

## Checked and found fine

- Geometry children run on rayon per node (`evaluate.rs:1384-1390`) with
  ID blocks in tree order; `CHAIN_NODES` (`:542`) keeps small renders off
  the pool; `Geometry`'s `Arc` (`geom/src/lib.rs:53`) makes cache inserts
  free. Cold start is 2.4 ms; the rest is in `followups.md` (O12).
- Big booleans and 2D: the parallel kernels, `batch_boolean`, Clipper
  sweep and banded unions are done (`slow-cases.md` §1.1, §2); the
  x-separated 2D case awaits the owner decision in `followups.md`.

## Not verified

- The shares of key hashing (P4) and PNG encoding (P6) in warm times: no
  profiling was run. The gains of P2 and P7 on this codebase.
- Whether `$` reads in BOSL2's attach code keep P1's hit rate near 2 keys
  per depth (`distributors.scad` mentions `$idx` 64 times).

## Recommended next steps

1. P6: time `encode_png` on a 1024² snapshot; `Compression::Fast` if it
   is most of the 9 ms. (S)
2. P4(a): `rayon::join` over sibling subtrees in `Keys::new`, with a
   1-thread equality test. (S)
3. P7: build the web core with `+simd128`; diff outputs against native. (S)
4. P2: a `cargo pgo` build for macOS arm64, measured interleaved against
   HEAD on the bench models. (M)
5. P1: a gated prototype for modules without children, measured on
   fractal_tree and checked by `incremental.rs`'s harness. (L)
6. P3: serialise `lang::fragment` keyed on content hash and build id,
   behind a flag, measured on the cold BOSL2 edit. (M–L)
