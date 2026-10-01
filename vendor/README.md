# Vendored dependencies

## The convention

Each directory `vendor/<crate>` is a crates.io release with local changes,
used through `[patch.crates-io]` in the root `Cargo.toml`. It must be
exactly:

1. the release's `.crate`, whose SHA-256 is the checksum in the crates.io
   index (what `Cargo.lock` records for a registry dependency);
2. minus the paths listed in `vendor/patches/<crate>/removed`, if that
   file exists (for files a patch cannot delete, such as a binary);
3. plus `vendor/patches/<crate>/0001-*.patch`, `0002-*.patch`, … applied
   in order from the crate root with `patch -p1`, each hunk at its exact
   position (no offset, no fuzz).

One patch per logical change, each starting with a short header: what it
changes, why, its upstream status, and the section of this file that has
the evidence. In the tree, each change to source is also marked with a
`NeoSCAD patch` comment.

`scripts/vendor-check.sh` rebuilds each tree that way (downloading the
`.crate` and checking its checksum) and compares it with `vendor/<crate>`
byte for byte, CRLF line endings included; CI runs it in the lint job.
It fails on any difference, on a patch that does not apply exactly, on
a `removed` entry the release does not have, and on a vendored crate
without a `vendor/patches/<crate>` directory.

**Changing a vendored crate.** Edit `vendor/<crate>`, then put the edit in
a patch:

- a new change: create the next file, say
  `vendor/patches/<crate>/0004-short-name.patch`, holding only its header
  (no line of it may start with `--- `), then run
  `scripts/vendor-check.sh --refresh <crate>`. That rewrites the body of
  the crate's last patch with the difference between the release plus
  every earlier patch and the vendored tree, and checks the crate;
- more work on the last change: edit, and `--refresh` again;
- a fix to an earlier patch: `--refresh` only rewrites the last one, so
  edit that patch by hand (or rebuild the series: apply the patches up to
  it in a scratch copy of the release, make the fix there, and diff with
  `scripts/vendor-check.sh --diff OLD NEW`), then check that the later
  patches still apply exactly.

`--refresh` and `--diff` write plain `diff -u` output, so GNU diff (Linux)
and BSD diff (macOS) can choose different but equivalent hunks; the check
compares trees, not patch bytes, and accepts either.

**When upstream releases a change.** Move the crate to the new release:
download it, apply the series in order, and for each patch that no
longer applies, check whether the release already has the change. If it
does, delete that patch file (and its section here) and renumber the
patches after it so the series stays `0001` onward; if it does not,
regenerate the patch against the new release. Then replace
`vendor/<crate>` with the new release's tree plus the remaining patches,
update any `=` pin on the crate in the workspace's `Cargo.toml` files,
and run the check. When no patch is left, drop
`vendor/<crate>`, `vendor/patches/<crate>` and its `[patch.crates-io]`
line.

## manifold-rust 0.13.1, patched

A copy of the crates.io release (`.cargo_vcs_info.json` gives the upstream
commit), used through `[patch.crates-io]` in the root `Cargo.toml`. It is
not a workspace member, so the workspace's lints, formatting and tests do
not apply to it. It carries six changes, each marked `NeoSCAD patch`: a
bug fix in `src/edge_op.rs`, two speed fixes in `src/polygon_earclip.rs`,
parallel boolean kernels in `src/par.rs` and its callers, a size
threshold for one of them, and cancellation a host can drive (below).
Drop the copy once upstream has them all.

The series, in `vendor/patches/manifold-rust/`:

| Patch | Change |
|---|---|
| `removed` | `.gitmodules` (a submodule the `.crate` does not ship) and `README_HERO.png` (the README's 257 KiB screenshot) are left out; neither is used by the build |
| `0001-edge-collapse-crease.patch` | the edge-collapse fix (`src/edge_op.rs`) |
| `0002-keyhole-loop-visitor.patch` | the keyhole speed patch (`src/polygon_earclip.rs`) |
| `0003-parallel-booleans.patch` | the parallel boolean kernels |
| `0004-keyhole-ring-boxes.patch` | the keyhole ring-box patch (`src/polygon_earclip.rs`) |
| `0005-batch-round-threshold.patch` | `batch_boolean` rounds of under 10,000 vertices run their pairs serially (`src/csg_tree.rs`; see the parallel boolean patch) |
| `0006-cancel-token-over-a-flag.patch` | a `CancelToken` over a caller's flag, polling a caller's check; checks inside `AddNewEdgeVerts` (`src/cancel.rs`, `src/boolean_result*.rs`; see the cancellation patch) |

The first vendoring (`17a31e4`) left the two files out without a
recorded reason; they could as well be restored, which would empty
`removed`. crates.io has had a newer release, 0.15.0, since 2026-09-29;
it has not been compared with this series.

Its source files use CRLF line endings; keep them when editing (a tool
that rewrites them as LF turns every line into a diff). The patches
carry the CRs in their lines, so keep those too.

### The edge-collapse bug

After a boolean, `simplify_topology` collapses "redundant" vertices: a new
vertex whose triangles come from at most two original faces is merged
into a neighbour. `collapse_edge` guards this with checks built from the
stored face references and normals, and those can be wrong about the
triangle in front of them:

- a mesh built without face IDs (OpenSCAD's and neoscad's are) has
  `face_id == -1` everywhere, so two different planar faces of one mesh
  differ only in `coplanar_id`, and the "edge separates faces" test, which
  compares `mesh_id` and `face_id`, lets the vertex leave the crease
  between them;
- `dedupe_edge` gives the triangles it adds a neighbour's reference and
  normal, and the axis-aligned projection the remaining checks use can
  turn a move along a face's normal into a point on a line.

Either way the vertex slides across a crease and the solid changes. In
BOSL2's `cubetruss` (docs/audits/engine-milestone.md, finding 3), a union of
two parts that touch along faces filled a tetrahedral notch of 7.31 mm³:
85024.0 instead of 85016.67. C++ Manifold 3.5.2 (the version OpenSCAD pins,
built from `.reference/openscad/submodules/manifold`) gives the same wrong
result on the same operands, so this is upstream behaviour, not a porting
error. The nightly avoids it on that model only because its lazy CSG
flattens the nested unions into one batch that never runs this boolean.

### The edge-collapse patch

Every triangle around the collapsing vertex (other than the two that
disappear) must stay in its own plane: the volume the move sweeps
(`(p_new - p_old) · ((p_last - p_old) × (p_next - p_old))`) must be at
most `tol` times the longer edge squared. For a proper triangle that
limits the vertex to about `tol` from the plane; sliver triangles, whose
area is at rounding level, sweep next to nothing and pass as before. On
the cubetruss model it rejects 8 collapses, all of them the notch; the
rest of the mesh is unchanged.

`crates/geom/tests/collapse_crease.rs` is the regression test: two
operands of 35 and 28 vertices whose union, unpatched, is 328.29 instead
of 314.49, in manifold-rust and in C++ Manifold 3.5.2 alike.

Manifold's current `master` has rewritten `CollapseEdge` (it no longer
has these checks); whether it still fails on these meshes is untested.

### The keyhole speed patch

The ear clipper joins each hole to an outer ring through a keyhole.
`cut_keyhole` and `find_closer_bridge` look for the bridge by walking every
outer ring, once each per hole, and 0.13.1 did that by cloning `outers`
and collecting each ring into a fresh `Vec` (`loop_verts`). Since each
joined hole becomes part of its outer ring, the rings grow as holes are
cut, and on a square with 5,041 circular holes the collecting was 60% of
the run (docs/audits/performance.md, O9).

The patch adds `for_each_loop_vert`, which visits the same verts in the
same order without collecting them (as C++ Manifold's `Loop` does), and
the two bridge searches use it and borrow `outers`. `loop_verts` is now a
wrapper over it for the three once-per-polygon callers. The walk itself,
and so the quadratic cost, is unchanged.

One detail keeps the output identical. `loop_verts` returned `None` for a
degenerate ring (one whose vert has `right == left`), and the callers then
skipped that ring whole. A visitor has already seen the verts before the
degenerate one, so each bridge search saves its connector before a ring
and restores it if the walk reports a degenerate ring. C++ Manifold does
not do this: its `Loop` applies the function up to the degenerate vert and
the callers ignore the result (`src/polygon.cpp:543-565, 723-725, 772-774`
in `.reference/openscad/submodules/manifold`), so on a degenerate outer
ring manifold-rust and C++ may already choose different bridges. The patch
keeps manifold-rust's behaviour, not C++'s.

`crates/geom/tests/kernel_patches.rs` pins the triangles, in order, of a
24×24 grid of octagonal holes, hashed with the unpatched copy.

### The keyhole ring-box patch

The keyhole speed patch made each walk cheaper but left the quadratic
shape: every hole still walked every outer ring twice, once in each
bridge search. C++ Manifold does the same (`CutKeyhole` and
`FindCloserBridge` loop over all of `outers_`,
`src/polygon.cpp:723-724, 772-773` in
`.reference/openscad/submodules/manifold`), and OpenSCAD triangulates a
2D shape's caps in one call over all its outlines
(`src/geometry/linear_extrude.cc:64`, `Polygon2d::tessellate`). So 200
lines of `linear_extrude`d text, one shape of about 30,000 rings, took
about 34 s in both tools, over 90% of it in the two searches.

The patch keeps a bounding box per outer ring, taken in `find_start` and
grown by the hole's box each time a hole is joined in (the joined verts
are copies of verts already in one box or the other; clipping only
removes verts). Before walking a ring, each search asks whether any vert
in the box could pass its tests:

- `cut_keyhole` takes a connector only where `vert_interp_y2x` is finite,
  which needs an edge with one end at or below start.y + eps and the
  other at or above start.y − eps. A ring wholly above or below that band
  is skipped.
- `find_closer_bridge` takes a vert only if it is right of start.x − eps,
  on the `above` side of start.y, and not clearly outside the line from
  start to the current connector (`ccw`, which counts as collinear
  anything within eps/2 times its longer vector). All three are linear in
  the vert's position, so the box's corners bound them.

Skipping a ring that cannot supply a connector leaves the connector, the
ring order and so the triangles exactly as they were. The margins (twice
epsilon, plus a relative 1e-9 far above the comparisons' rounding) only
make the tests more permissive. The searches also track which ring the
connector came from, so the right box is grown.

`crates/geom/tests/kernel_patches.rs` pins the triangles of a grid of
glyph-like outer rings with holes and islands, hashed before the patch.
Every 3D model in `tests/data/scad` and `examples` (301 STL exports), and
the 50- and 200-line text extrusions at 1 and 14 threads, export
byte-identical STL before and after. The 200-line extrusion went from
33.6 to 2.7 s (`docs/audits/slow-cases.md` §2).

### The parallel boolean patch

0.13.1 runs most of a boolean on one thread, where C++ Manifold runs it
under TBB. On the level-4 Menger sponge the last two differences (the
cube minus the union of the three rotated negatives, about 300,000 faces)
ran alone for over a second while the other cores idled
(`docs/audits/slow-cases.md` §1). The patch makes the large serial
stages parallel, each in a way whose output is the sequential output, so
the result is byte-identical at any thread count and to 0.13.1's. Every
site goes through a helper in `src/par.rs` with a sequential twin for
builds without the `parallel` feature (the WASM build's pool runs on the
calling thread either way). The patch is
`vendor/patches/manifold-rust/0003-parallel-booleans.patch`, on top of
0001 and 0002. By site:

- **`batch_boolean` rounds** (`csg_tree.rs`): a round's up to four pairs
  are picked first, then run side by side (C++ `csg_tree.cpp:451-479` in
  Manifold 3.5.2), and the results go back on the heap in pair order with
  the serials the sequential loop gave them. The one shared state a boolean touches is the
  mesh-ID counter; the kernel compares mesh IDs only for equality, and
  neoscad orders output runs by original ID for the same reason
  (`crates/geom/src/manifold_geom.rs`, `canonical_mesh`). Since 0005 a
  round goes parallel only when its operands have 10,000 vertices in all
  (C++ `autoPolicy`'s `kSeqThreshold`): with five busy loops per core,
  `csg_spheres` took 2.76 s against 3.70 s and the hero 10.5 against
  11.5 s (best of 7); unloaded, and with one busy loop per core, the
  difference was within noise (`docs/audits/slow-cases.md` §1.1).
- **Edge-flag scans** of `collapse_short_edges`, `collapse_colinear_edges`
  and `swap_degenerates` (`edge_op.rs`): the flags are tested in parallel
  above 100,000 halfedges (C++ `FlagStore::run`, `edge_op.cpp:54-97`) and
  collected in index order; the collapses and swaps stay sequential, as
  in C++.
- **Orbit scans** of `split_pinched_verts` and `dedupe_edges`: the
  sequential scan handles each vertex orbit from its smallest eligible
  halfedge, skipping halfedges an earlier orbit visited. Above 10,000
  halfedges each halfedge instead walks its own orbit in parallel and
  owns it if it comes back to itself without meeting a smaller eligible
  one. That gives the same owners only if every orbit is a closed cycle,
  so it first checks that `paired_halfedge` is an involution (then the
  step is injective and a walk either closes or ends at a missing pair;
  an open orbit is seen by its smallest eligible halfedge) and falls back
  to the sequential scan otherwise. The owners' work (the pinched-vertex
  splits, in owner order; the duplicate lists, concatenated in owner
  order) is then the sequential scan's. C++ uses atomics here
  (`edge_op.cpp:722-796, 903-924`) and sorts the duplicates, which would
  change the order the port applies them in.
- **Edge maps of the result assembly** (`boolean_result.rs`): the
  `BTreeMap<K, Vec<EdgePos>>` built one entry at a time became a list of
  `(key, EdgePos)` in push order, stably sorted by key and cut into runs.
  The consumers read keys in ascending order and each run in push order,
  exactly as they read the map. This is a data-structure change more than
  a parallel one: on the big Menger difference the map took 0.23 s and
  the sort 0.04 s.
- **`face2tri` writes** (`face_op_triangulate.rs`): each face writes only
  its own output triangles and its own edges' `contour2tri` entries, so
  fixed runs of 4,096 faces get disjoint slices of the arrays (by
  `split_at_mut`) and write them in parallel. The per-face `HashMap` of
  general triangulations became the `Vec` they were collected into.
- **`sort_geometry`** (`sort.rs`) and the `intersect12` result sort
  (`boolean3_kernels.rs`): Morton codes, face boxes and all gathers are
  per-element maps; the sorts are stable sorts on integer keys, whose
  result does not depend on the algorithm (`par::maybe_par_sort_by_key`).
- **`winding03`**: the test for which edges to unite (forward, not cut)
  runs in parallel; the unions stay sequential in index order, because
  the union-find's roots depend on it and a component's winding number is
  computed at its root.

Not parallel, and serial in C++ too: the collapses themselves (on the
Menger difference, 0.14 s of `collapse_edge` over 5 million flagged edges)
and `recursive_edge_swap`.

**Evidence of identical output.** Release builds before and after,
exporting all 527 `.scad` files in `.reference/openscad/tests/data/scad`,
13 benchmark models (all but `import_stl`), the hero model and Menger
level 4, at the default thread count and at `RAYON_NUM_THREADS` 1 and 3:
384 exported (273 OFF, 111 SVG), and every file is byte-identical to the
unpatched build's. Conformance is 1,773 passing, 0 failing, at the
default and at `RAYON_NUM_THREADS=1`. `crates/geom/tests/parallel_kernels.rs`
renders two models large enough to take every parallel path above (a
checkerboard of 256 edge-touching cubes for the orbit scans and
`batch_boolean`, a sphere with 49 holes for the rest) on 1 and 8
threads and checks the export against hashes taken with the unpatched
copy.

**Timings** (M4 Pro, 14 cores, interleaved, best of 5; the machine was
shared, load average 5–9 unless noted): see `docs/audits/slow-cases.md`
§1 for the tables. Menger level 4 went from 2.62 s to 1.59 s (the
nightly: 2.29 s), `csg_spheres` from 0.57 to 0.47 s.

### The cancellation patch

manifold-rust ports Manifold's cooperative cancellation (`src/cancel.rs`:
a token checked between a boolean's stages, inside its long loops and
between `csg_tree`'s batch rounds), but `CancelToken::new` makes its own
flag, and the token knows only that flag. A request's cancel is the
host's flag, and its time and memory limits are only known to the
request's guard (the clock, and on wasm32 the counting allocator's
peak). So:

- `CancelToken::from_flag` makes a token over an existing
  `Arc<AtomicBool>`, and `with_check` adds a condition polled at every
  check; once it is true the flag is set, so the answer is sticky as
  before. `geom::manifold_geom::kernel_token` builds the request's token:
  its interrupt flag, and the guard's `stopped()` as the check. Without
  a token (`None`, every unlimited render) nothing is polled.
- `AddNewEdgeVerts` (`src/boolean_result.rs`, called from
  `src/boolean_result_assemble.rs`) checks the token every 16,384
  intersections, and between the three sorts after it, and its three
  lists are reserved at their final size (the sum of the inclusions'
  magnitudes) instead of grown by doubling. In the Menger sponge at
  depth 5 (`examples/Old/example024.scad`, `n=5`) the last union grew a
  wasm instance from under 1 GiB past 2 GB within this one step, before
  any check ran; the last doubling of the new-edge list alone copied
  a buffer into one twice its size.

The content and order of the lists are unchanged (a capacity is not
content), and an uncancelled token changes no output:
`crates/geom/tests/kernel_cancel.rs` compares a render with a token that
never fires against one without, on 1 and 8 threads, and checks that a
cancel lands inside one boolean (a boolean of two dense spheres stops
within milliseconds of the cancel, where it ran 140 ms in all) and that
a cancelled, empty result is never cached. In the web core, the Menger
sponge's depth-5 render under the 1 GiB limit now stops with a
`resource-limit` error at a measured peak of about 1.4 to 1.7 GiB, and
the engine lives on (`crates/web/test/run.mjs`).

## clipper2-rust 1.2.0, patched

A copy of the crates.io release (`.cargo_vcs_info.json` gives the upstream
commit), used through `[patch.crates-io]` in the root `Cargo.toml`, not a
workspace member, like manifold-rust. Both neoscad (`crates/geom`,
`crates/io`) and manifold-rust depend on it, and the patch applies to
both. It carries four changes, each marked `NeoSCAD patch` in the
source: `nearbyint_f64` in `src/core.rs`; two sweep shortcuts and a
split-off counter in `src/engine.rs` and `src/engine_public.rs` (below);
and `rust-version` raised from 1.70 to 1.77 in `Cargo.toml` (the release
that stabilised `f64::round_ties_even`; the workspace needs 1.98 anyway).
Drop the copy once upstream has the fixes; the counter exists only for
neoscad's banded union and would stay a local patch.

The series, in `vendor/patches/clipper2-rust/` (nothing is removed):

| Patch | Change |
|---|---|
| `0001-nearbyint-round-ties-even.patch` | the rounding patch (`src/core.rs`) and the `rust-version` it needs (`Cargo.toml`) |
| `0002-sweep-shortcuts.patch` | the two sweep shortcuts (`src/engine.rs`) |
| `0003-late-outrecs-counter.patch` | the split-off counter (`src/engine.rs`, `src/engine_public.rs`) |

The shortcuts and the counter were committed together (`4fe4459`) and
split into 0002 and 0003 afterwards; the tree between them (shortcuts
without the counter) compiles (`cargo check --lib`) but was never tested
or benchmarked on its own.

### The rounding patch

`nearbyint_f64` is Clipper's round-half-to-even, used by `top_x` (the x of
an active edge at a scanline, called for every edge at every scanline)
and `get_closest_point_on_segment`. 1.2.0 implements it in software
(`trunc`, a subtraction, two comparisons and an `f64 %` on ties) because
its MSRV predates `round_ties_even`. In the 200-line 2D text union it was
16% of the serial union (docs/audits/performance.md, O10).

The patch calls `x.round_ties_even()`, one instruction on arm64 (`frintn`)
and wasm32 (`f64.nearest`), for finite `x`. For finite input the two agree
bit for bit, the sign of zero included. They differ for ±∞: the old code
computes `∞ - ∞` and returns NaN, `round_ties_even` returns ∞. `top_x`
could pass ∞ (a horizontal edge's `dx` is ±`f64::MAX`, times a nonzero
height; whether any caller reaches that is untested), and it casts the result to `i64`: NaN gives 0, ∞ gives `i64::MAX`,
which would then overflow the add. So non-finite input still returns NaN.
`crates/geom/tests/kernel_patches.rs` checks the patched function against
a copy of the old one on special values, every half-integer (and its
neighbours) in ±100,000, and 4 million other values.

### Evidence that neither patch changes output

Release builds before and after, each exporting all 527 `.scad` files in
`.reference/openscad/tests/data/scad` plus the 14 benchmark models, the
5,041-hole circle grid, 50- and 200-line text (2D and extruded) and an
`offset()` sweep (r, delta, chamfer, negative r on text): 392 exported
(278 OFF, 114 SVG), and every file is byte-identical. The circle grid, text
and offset models are also identical with `RAYON_NUM_THREADS` 1 and 3.

Interleaved A/B, best of 7 (3 for the 200-line extrude), M4 Pro:

| Model | Before | After | Change |
|---|---|---|---|
| square with 5,041 holes, `linear_extrude(5)` | 1.278 s | 1.061 s | −17% |
| 71×71 circles, `linear_extrude(5)` (no holes) | 0.082 s | 0.082 s | 0% |
| `text_30lines` (benchmark) | 0.454 s | 0.320 s | −30% |
| 50 lines of text, `linear_extrude(2)` | 3.83 s | 2.58 s | −33% |
| 200 lines of text, `linear_extrude(2)` | 50.4 s | 32.8 s | −35% |
| 200 lines of text, 2D, to SVG | 2.93 s | 2.01 s | −31% |
| 50 lines of text, 2D, to SVG | 0.733 s | 0.515 s | −30% |
| `offset()` sweep, 2D | 30.5 ms | 19.7 ms | −35% |

With only the clipper2-rust patch (best of 5), the 2D text is −31% (all
of the gain), the extruded 50 lines −6% and the hole grid 0%: the
keyhole patch is the extrusions' gain, the rounding patch the 2D one.

### The sweep shortcuts

After the rounding patch, a Time Profiler trace of the 200-line 2D text
union (`xcrun xctrace`, 2,925 samples) put 62% of the whole run inside
`build_intersect_list`: 39% in its own merge sort, 9% in `top_x`, 6% in
`adjust_curr_x_and_copy_to_sel`. At every scanbeam it recomputes each
active edge's x at the beam's top and merge-sorts the list by it, to find
the edges that crossed. In a union of glyph outlines that do not overlap,
almost no beam has a crossing, and the sort (`log2 n` passes over a few
hundred edges) runs to find nothing.

- `adjust_curr_x_and_copy_to_sel` now also reports whether any edge's new
  x is less than its left neighbour's. If none is, `build_intersect_list`
  returns `false` before the sort. The sort only records an intersection
  (and only moves an edge) on a strict `<` between runs of a list that is
  already in order, so it would have returned `false` too, with
  `intersect_nodes` empty (it always is on entry: `do_intersections`
  clears it after each use). The SEL and `jump` links the sort would have
  rearranged are dead: `do_top_of_scanbeam` sets `sel` to `None` before
  anything reads them again.
- When that happens, `do_top_of_scanbeam` does not recompute `top_x` for
  edges that do not end at the scanline: `do_intersections` returns
  whether every edge's `curr_x` is still the value just computed for the
  same `y`, and the loop only rewrites the edge it is on (`do_maxima`'s
  `intersect_edges` and `split` leave edge geometry alone), so the store
  would write the same value.

The first is −44% wall and −49% CPU on the 200-line text, the second a
further 1–3% of CPU. Two other ideas were measured and dropped: a
`repr(C, align(64))` `Active` with the fields the sweep reads in one
cache line (under 1%, within noise), and copying the SEL only when the
list is out of order (no gain).

### The split-off counter

`ClipperBase::late_outrecs` is the number of output records the last
`Clipper64::execute_tree` created after its sweep, in `process_horz_joins`
and `fix_self_intersects`. It changes no output; `crates/geom`'s banded
union (`union_by_bands` in `crates/geom/src/clipper.rs`) reads it to
decide whether its result is exactly the full union's (see that function
and `docs/audits/slow-cases.md` §2).

### Evidence that the sweep shortcuts change no output

Release builds of `6d73727` and of this patch set (the shortcuts and the
banded union together), exporting all 527 `.scad` files under
`.reference/openscad/tests/data/scad`, 16 adversarial 2D models (rows of
shapes that touch exactly or are 0.001 apart, nested holes, L shapes whose
boxes overlap but which do not touch, 6,400 tiny circles, 3,600 squares
that touch, diamonds that meet at vertices, self-intersecting polygons,
rotated and touching lines of text, rectangles that join and split, text
under `offset()`, and extruded text) and the 200-line text: 671
exports (2D to SVG and DXF, 3D to OFF), every output file byte-identical,
at the default thread count and with `RAYON_NUM_THREADS` 1 and 3. Two
reference files' console output differs between any two runs of the
unpatched build as well (`docs/followups.md`, Determinism). Conformance is
1,773 passes and 0 failures before and after. The crate's own tests pass
(406, or 426 with `using_z`, run in a copy outside the workspace).

Interleaved A/B, M4 Pro, load average 5–9 (another build on the
machine), best of 5 (1 for the 200-line extrude). "Sweep" is the two
shortcuts alone; "all" adds the banded union:

| Model | Before | Sweep | All | Nightly |
|---|---|---|---|---|
| 200 lines of text, 2D, to SVG | 1.667 s (2.95 s CPU) | 0.938 s (1.48 s CPU) | 0.491 s (1.55 s CPU) | 1.414 s |
| 50 lines of text, 2D, to SVG | 0.125 s | 0.085 s | 0.055 s | 0.152 s |
| 50 lines of text, `linear_extrude(2)` | 0.388 s | | 0.319 s | 0.457 s |
| 200 lines of text, `linear_extrude(2)` | 39.4 s | | 36.3 s | 31.0 s |

## wgpu-core 30.0.1, patched

A copy of the crates.io release (`.cargo_vcs_info.json` gives the upstream
commit, `40f4a34e`), extracted from the `.crate` whose SHA-256 matched
`Cargo.lock` (`14c018fc…`), with no file left out. It is used through
`[patch.crates-io]` in the root `Cargo.toml` and is not a workspace
member, like the others. Every build that compiles wgpu-core uses it: the
native renderer (`crates/render`, `crates/ffi`) and the web viewer's WebGL
build (`crates/web-view`'s default `webgl` feature). The WebGPU-only web
builds do not compile wgpu-core at all; the browser implements WebGPU.

It carries one change, upstream's own fix, marked `NeoSCAD patch` in
`src/device/resource.rs`, and its series is that one patch,
`vendor/patches/wgpu-core/0001-maintain-queue-empty-race.patch`.

Being a path dependency, its compiler warnings are no longer capped the
way a registry crate's are, so every build now shows one from upstream
code: an unfulfilled `expect(unused)` in `src/lock/ranked.rs:83`. It is
left alone to keep the copy equal to the release plus the fix; it is not
an error under `cargo clippy -- -D warnings`, which only lints workspace
members.

### The poll race

`Device::maintain`, which every `Device::poll` runs, waits on the fence,
reads the fence value, and later asks the queue to retire finished
submissions, which reports whether the queue is now empty. Nothing holds
a lock across those steps. When a timed `poll(Wait)` times out and another
thread's poll (or the maintain inside `Queue::submit`) retires every
submission in between, this poll sees an empty queue with a fence value
below its target and trips a defensive assert ("If the queue is empty,
the current submission index (N) should be at least the wait submission
index (N+1)", line 948 of the release's `device/resource.rs`). NeoSCAD's
readbacks wait in 50 ms slices on a device shared between threads, so any
slice that expired under load could panic: CI run 36632690616 failed that
way, and `the_shared_device_survives_concurrent_use` (in `crates/ffi`)
failed 14 times in 300 runs on a Mac with every core busy. The stopgap
(`4d2222a`) waited with no timeout, so a wedged GPU blocked the thread.

### The patch

gfx-rs/wgpu#9958 (https://github.com/gfx-rs/wgpu/pull/9958), merged to
trunk as `385520f72f5bbb614fec957ed31f3f6d19076a3f` on 2026-08-07, applied
unchanged: its `wgpu-core/src/device/resource.rs` diff applies cleanly to
30.0.1 (its `CHANGELOG.md` line is not part of the crate). `maintain` keeps
the wait's outcome and reports `QueueEmpty` only when the wait did not time
out; a timed-out wait then reports `WaitSucceeded` or `Timeout` from the
fence value it read, and the assert only runs where the fence was read
after a successful wait. `Readback::wait_mapped` and `release_staging`
(`crates/render`) wait for at most 10 s again.

Drop the copy, the patch file and the `[patch.crates-io]` line once a
wgpu release carries #9958. As of 2026-09-29 none does: crates.io's
newest is 30.0.1 (2026-08-22), and the `v30` branch does not contain the
commit. Until then, upgrading wgpu means re-vendoring the new wgpu-core
with the patch applied (or dropping it, if the release has the fix).

## Upstream drafts

Both crates are Lars Brubaker's ports. Their `main` branches, fetched
2026-09-27, still have the code these patches replace. The owner can send
these as issues, with the diff of the vendored file as the patch.

### manifold-rust: `EarClip` allocates every outer ring once per hole

> **Title:** Triangulation is dominated by `loop_verts` allocation on
> polygons with many holes
>
> `EarClip::cut_keyhole` and `find_closer_bridge`
> (`src/polygon_earclip.rs`) clone `self.outers` and call `loop_verts`,
> which collects each outer ring into a new `Vec`, for every hole. The
> outer rings grow as holes are joined, so on a square with 5,041 circular
> holes (`$fn = 16`) this collection is 60% of the run, and in extruded
> text (many glyph holes) `Vec` growth alone is 29%.
>
> Suggested fix: a visitor, `fn for_each_loop_vert(&self, first: usize,
> f: impl FnMut(usize)) -> bool`, with the same traversal as `loop_verts`
> (which can then wrap it), used by both bridge searches over
> `&self.outers`. This matches C++'s `Loop(first, func)`. To keep the
> current output exactly, a search saves its `connector` before each ring
> and restores it when the visitor reports a degenerate ring, since
> `loop_verts` skipped such a ring whole. (C++ keeps the partial update;
> matching C++ would be the other option, and would change output only for
> degenerate outer rings.) With the restore, output is byte-identical on
> every OpenSCAD test model we export (392 files), and triangulation-heavy exports are
> 17–35% faster end to end.

### manifold-rust: every hole walks every outer ring

> **Title:** Triangulating many separate polygons with holes is quadratic
> in the number of rings
>
> `EarClip::cut_keyhole` and `find_closer_bridge` walk every ring in
> `outers` for every hole, as C++ Manifold does. For one polygon with
> many holes that is needed, but for many separate polygons (a page of
> text is about 30,000 rings) almost every ring is far from the hole:
> 200 lines of extruded text spent over 90% of 34 s in these walks.
>
> Suggested fix: keep a `Rect` per outer ring (from `find_start`, grown by
> the hole's box on each `join_polygons`), and skip a ring whose box
> cannot contain a qualifying vert: for `cut_keyhole`, a ring wholly above
> or below start.y ± eps; for `find_closer_bridge`, a box left of
> start.x − eps, on the wrong side of start.y, or wholly outside the line
> from start to the connector (checked at the corners, since the test is
> linear). With margins above rounding, the output is unchanged; on the
> text model the run is 12 times faster.

### clipper2-rust: `nearbyint_f64` in software

> **Title:** Use `f64::round_ties_even` in `nearbyint_f64`
>
> `nearbyint_f64` (`src/core.rs`) implements round-half-even in software
> because the MSRV (1.70) predates `f64::round_ties_even` (stable in
> 1.77). It is called from `top_x` for every active edge at every
> scanline, and in a large union (200 lines of glyph outlines) it was 16%
> of the time. Replacing it with
>
> ```rust
> if x.is_finite() { x.round_ties_even() } else { f64::NAN }
> ```
>
> is bit-identical for every input (including `-0.0`, and NaN for ±∞,
> which the old code produced as `∞ - ∞`; `top_x` could pass ∞ for a
> horizontal edge, where `∞ as i64` would overflow the following add), and
> makes the union about 30% faster end to end. It needs `rust-version =
> "1.77"`.

### clipper2-rust: the intersection sort runs when nothing crossed

> **Title:** Skip `build_intersect_list`'s merge sort when the active
> edges are still in order
>
> At every scanbeam, `build_intersect_list` (`src/engine.rs`) recomputes
> each active edge's x at the top of the beam and merge-sorts the edges by
> it to find the ones that crossed. When no edge crossed, the list is
> already sorted and the sort records nothing, but it still makes
> `log2 n` passes. In a union of many shapes that do not overlap (200
> lines of glyph outlines) that was 39% of the run by itself.
>
> Suggested fix: have `adjust_curr_x_and_copy_to_sel` return whether any
> new `curr_x` is less than its left neighbour's, and return `false`
> from `build_intersect_list` when none is. The sort only acts on a
> strict `<`, `intersect_nodes` is empty on entry, and the SEL links it
> would rearrange are reset by `do_top_of_scanbeam` before they are read,
> so the result is the same. In that case `do_top_of_scanbeam` can also
> skip recomputing `top_x` for edges that do not end at the scanline, since
> their `curr_x` already holds it. Output is byte-identical on every
> OpenSCAD test model we export and on 2D stress models (671 exports),
> and the 2D union is about twice as fast. C++ Clipper2 has the same
> sort (`ClipperBase::BuildIntersectList`) and could take the same check.
