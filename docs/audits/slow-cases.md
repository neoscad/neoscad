# The three slow cases versus the nightly

neoscad.org says neoscad "is slower than the nightly on some models: deep
unions, the Menger sponge at level 4, and models with many text() nodes".
This audit re-measures each, pins the cause to code, and ranks fixes.
Nightly: OpenSCAD 2026.09.23, `--backend=manifold`. neoscad: `target/release`
at `49eb8b5`. Timings today were taken at load average 73–99 (another agent
and a GPU test loop), interleaved, two runs each; use the ratios and the
recorded unloaded numbers, not today's absolutes. Models are the ones in
`docs/audits/performance.md` §3 and `conformance/bench.json`.

## Summary

| Case | Recorded, unloaded (`performance.md:379-381`, `vendor/README.md:136-141`) | Today, loaded | Verdict |
|---|---|---|---|
| Menger level 4 (`examples/Old/example024.scad`, `n=4`) | 2.87 s (8.2 s CPU) vs 2.01 s (14.4 s CPU) | 3.81–3.95 s (7.5 CPU) vs 3.80–5.11 s (12.5 CPU) | Real, ~1.4× slower. Cause: manifold-rust runs its boolean kernels serially |
| 200 lines × 125 chars of `text()`, 2D to SVG | 2.85 vs 1.63 s, then −31% from the rounding patch ≈ 1.97 vs 1.63 s | 2.24–2.26 s (3.2 CPU) vs 2.07–2.31 s (1.8 CPU) | Real, ~1.2× slower. Cause: one single-threaded Clipper union of 30,140 contours in the pure-Rust port |
| Deep unions | none recorded | flat 800 cubes: 0.036 vs 0.134 s (bench, 2026-09-28); nested 256 spheres: 0.24 vs 0.57 s | **Not reproduced.** neoscad is 2–4× faster on both union shapes tried |

Two corrections to the written record, both verified against the source:

- **The nightly is not lazy across nodes.** `docs/followups.md:141-150` says
  nested unions "flatten into one BatchUnion over every leaf" because
  OpenSCAD's Manifold operators are lazy. They are lazy within one operator,
  but `GeometryEvaluator.cc:168,178` and `manifold-applyops.cc:69` call
  `isEmpty()` on every child before folding, and `ManifoldGeometry::isEmpty`
  (`ManifoldGeometry.cc:66-68`) calls `Manifold::IsEmpty` (`manifold.cpp:293`),
  which goes through `GetCsgLeafNode` → `ToLeafNode` (`manifold.cpp:200-202`)
  and evaluates the tree. Each cached node is a materialised mesh in both
  tools; the flattening only covers an operator's direct children, which
  neoscad's `ManifoldGeometry::batch` already does with the ported
  `CsgNode::op_n` (`crates/geom/src/manifold_geom.rs:270-317`).
  "Lazy solids across cache boundaries" (`followups.md:150`) would not
  close the gap.
- **The `nearbyint_f64` cost in the text case is fixed** (vendored
  clipper2-rust, `vendor/README.md:86-112`); `performance.md:380` predates it.

## 1. Menger sponge, level 4

**Model.** `difference() { cube; for (3 rotations) menger_negative(level=4) }`
then a second difference (`example024.scad:10-32`). Each `menger_negative`
is a group of one bar plus 8 recursive copies: 585 bars per rotation, 1,755
in all. Result: genus 13,394, 207,148 facets, 44 cached geometries
(`--summary cache`). Both tools evaluate the same tree shape: groups union
bottom-up, then one subtraction of the cube by the union of three rotated
negatives.

**Where the time goes** (`performance.md:379`, profile at `26888d7`):
5.6–7.9 cores for the first 1.3 s (the sibling groups, on rayon), then
**1.2 s on one core** in the last big booleans: `simplify_topology`
(`collapse_colinear_edges` 15%, `collapse_edge`, `split_pinched_verts`) and
boolean assembly. The nightly spends 1.75× more CPU to finish 30% sooner,
which is the signature of parallel kernels, not of a different algorithm.

**Root cause: manifold-rust's kernels are serial.** Counting parallel
dispatch sites (`for_each_n`/`ExecutionPolicy`/`reduce` in C++,
`maybe_par`/`rayon` in Rust): C++ `boolean3.cpp` 4, `boolean_result.cpp` 16,
`impl.cpp` 24, `sort.cpp` 16, `face_op.cpp` 7, `edge_op.cpp` 3; Rust
`boolean3.rs` 0, `boolean_result_assemble.rs` 0, `collider.rs` 0,
`face_op.rs` 1, `edge_op.rs` 1 (8 `maybe_par` sites in the whole crate,
`vendor/manifold-rust/src/par.rs`). Secondary: `batch_boolean` runs each
round's four pairs one after another (`csg_tree.rs:408-465`), where the C++
puts them on a `tbb::task_group` (`csg_tree.cpp:452-470`); rayon there
measured only 5–8% (`followups.md:147-149`). Note `simplify_topology`'s
edge collapse is serial in C++ too, so about 1 s of the tail is not
recoverable by parallelism in either tool.

**Fix options, ranked.**

1. **Parallelise the broadphase and the intersection/assembly kernels in
   manifold-rust** (`collider.rs`, `boolean3.rs`, `boolean_result_assemble.rs`,
   `sort.rs`), mirroring the C++ sites, with deterministic reductions
   (fixed chunking, ordered merges). Gain: most of the 0.9 s gap on this
   model; also helps every big single boolean (hero, 125 spheres). Risk:
   medium, determinism must be re-proven (byte-identical at any thread
   count). Effort: L (a week-scale vendor change; upstreamable).
2. **`rayon::join` over the four pairs of each `batch_boolean` round.**
   Gain 5–8% (measured, 5b). Risk low (same output was verified). Effort S.
3. **Do not pursue lazy cross-node solids.** The nightly does not have them
   either (correction above).

## 2. Many `text()` nodes

**Model.** 200 `translate() text(125 chars, size=5)` lines, 2D, `-o .svg`.
SVG byte-identical to the nightly (`performance.md:380`; same 15,851,604
bytes today). 401 cached geometries, 30,140 contours.

**Where the time goes.** Shaping and outlines are cheap and parallel
(0.25 s; per-face glyph and flattening caches, `crates/text/src/outline.rs:77-81`).
The rest is one union of the 200 sanitized polygons: `crates/geom/src/clipper.rs:139-190`
(`apply`, one `add_subject` per child then one `execute_tree`), the port of
`ClipperUtils::apply` that `GeometryEvaluator.cc:694` calls for the same
job. Profile before the rounding patch (`performance.md:380`): 2.0 s serial
in clipper2-rust's `execute_internal`: `build_intersect_list` 27%, `top_x`
23%, `nearbyint_f64` 16%. The patch removed the last item (−31% on this
model, `vendor/README.md:139`). Both tools do exactly one union; Clipper2 C++
is single-threaded as well. What remains is the port's constant factor
(generic `T::from_f64` conversions in `top_x`, per-scanline `Vec` growth in
`build_intersect_list`). A fresh profile could not be taken today (`sample`
returned no call graph under the sandbox); `cargo flamegraph` on
`text200.scad` would confirm the split.

**Fix options, ranked.**

1. **Skip the union for children whose bounding boxes are disjoint** and
   only union overlapping clusters, composing the rest (the 2D analogue of
   Manifold's `BatchUnion` compose step). Text lines rarely overlap, so
   nearly all of the ~1.7 s goes away. Risk: **output order.** Clipper's
   output contour order comes from one scan over all inputs; a composed
   result needs the same order to keep the SVG byte-identical to the
   nightly, which is a parity test today. Product decision: keep byte
   parity, or accept a canonical-but-different order. Effort M.
2. **Optimise clipper2-rust's hot loops** (`top_x`, `build_intersect_list`,
   the active-edge list) with the profile above. Bit-identical by
   construction, upstreamable like the rounding patch. Gain 10–30% of the
   union. Risk low. Effort S–M.
3. Extruded text (64.4 vs 31.5 s at `26888d7`, `performance.md:381`) was
   the keyhole triangulation, cut 35% by the vendored patch
   (`followups.md:151-158`); re-measure before spending more there.

## 3. Deep unions: not reproduced

No model is recorded for this claim: `followups.md:141` names none, and the
`deep_union` row in `performance.md:410` is the bench's `csg_deep_union`
(800 rotated cubes, one implicit union), where neoscad is **3.7× faster**
(0.036 vs 0.134 s, `progress/bench/20260928T130500Z-dca0882.json`). A nested
binary `union()` tree of depth 8 (256 overlapping spheres, `$fn=24`) ran
0.23–0.25 s against the nightly's 0.56–0.57 s today. Since the nightly forces
every child (correction above), there is no structural reason for nested
unions to be slower than flat ones here. The only losing case with unions in
it is the Menger sponge, whose cost is the final differences.

**Suggested change:** drop "deep unions" from the website claim, or restate
it as "large nested differences (the Menger sponge at level 4)". If the
owner remembers the original model, one interleaved run settles it.

## Checked and found fine

- `ManifoldGeometry::batch` folds an operator's children through the ported
  `csg_tree` (smallest-first pairs, disjoint compose), the same order as the
  nightly, with a mesh-ID renumbering the C++ `Compose` also does
  (`manifold_geom.rs:288-312`).
- The preview path's `union_tree` (`crates/geom/src/csg.rs:970-997`) splits
  large negative sets in halves on rayon with a count-only shape, so it is
  thread-count deterministic; it is not on the render path.
- mimalloc is linked (`followups.md:117`); O1's −6.6% on Menger 4 is
  already banked.
- 2D and 3D text output is byte-identical to the nightly on both models.

## Recommended next steps

1. Fix the website claim about deep unions (no evidence; §3). Trivial.
2. Profile `text200.scad` with `cargo flamegraph` and micro-optimise
   clipper2-rust's `top_x`/`build_intersect_list` (§2.2). S–M, bit-identical.
3. Decide on 2D disjoint compose (§2.1): the biggest text win, but it
   touches SVG byte parity. Owner's call.
4. `rayon::join` in `batch_boolean` rounds (§1.2): small, measured, safe.
5. Parallel kernels in manifold-rust (§1.1): the real Menger fix, L effort,
   needs the determinism proof re-run.
