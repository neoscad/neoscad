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
`NeoSCAD patch` comment, except in a patch that is an upstream proposal
taken verbatim (its header names the branch), where the marker would be
the only difference from what upstream is asked to merge.

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

## manifold-rust 0.16.0, patched

A copy of the crates.io release (`.cargo_vcs_info.json` gives the upstream
commit), used through `[patch.crates-io]` in the root `Cargo.toml`. It is
not a workspace member, so the workspace's lints, formatting and tests do
not apply to it. It carries one change, NeoSCAD's own and marked `NeoSCAD
patch` in the source: a cancel token a host can drive (below). Drop the
copy once the token API has a home upstream.

0.16.0 took the five changes NeoSCAD had proposed upstream (two speed
fixes in the ear clipper, parallel boolean stages, parallel
`batch_boolean` rounds, and cancellation checks inside the result
assembly), so their patches are gone; the sections on them below now
describe upstream's code, and what NeoSCAD relies on in it.

The series, in `vendor/patches/manifold-rust/`:

| Patch | Change |
|---|---|
| `removed` | `.gitmodules` (a submodule the `.crate` does not ship) and `README_HERO.png` (the README's 257 KiB screenshot) are left out; neither is used by the build |
| `0001-cancel-token-over-a-flag.patch` | NeoSCAD's: a `CancelToken` over a caller's flag, polling a caller's check (`src/cancel.rs`; see the cancel token patch) |

The first vendoring (`17a31e4`) left the two files out without a
recorded reason; they could as well be restored, which would empty
`removed`.

Every file in the 0.16.0 `.crate` uses LF line endings. Upstream's git
blobs were LF all along (`src/vec.rs` at 0.15.0's `a866917` has no CR),
but the 0.15.0 `.crate` was packaged with CRLF in most source files, so the move from 0.15.0 rewrote
every line of those once. The patch is plain LF too. Keep whatever the
release has: `scripts/vendor-check.sh` compares bytes.

### Moving from 0.15.0 to 0.16.0

0.16.0 (upstream `f43ec62`, 28 commits after 0.15.0's `a866917`) has
all five of NeoSCAD's upstream proposals, so `0001` to `0005` were
dropped; `0006` was dropped as well, and `0007` is now `0001`. Its
release commit (`d1cdf28`) lists the changes; upstream keeps no
changelog file, and its `docs/CPP_DIVERGENCES.md` has the departures
from C++. How 0.16.0's versions differ from NeoSCAD's former patches:

- **Keyhole walks** (`0001`; upstream `c15d4ad`, PR #6, on top of the
  pure move `8a963bd`): the same walk and the same save-and-restore
  around a degenerate ring. `loop_verts` and `for_each_loop_vert` stay in
  `src/polygon_earclip.rs` (the patch moved them into the keyhole file),
  and the patch's test of an outer ring collapsed by an earlier hole was
  not taken: upstream's divergence entry 12 argues, and fuzzes, that a
  degenerate ring is reported before any vert is visited, so the restore
  never acts.
- **Ring boxes** (`0002`; `bc0a64d`, PR #9): the same culls, with a
  different window for the `ccw` cull: the patch culled when the corner
  distances, the connector's distance and epsilon were at most 1e75 and
  the margin at least 1e-150; upstream culls when the connector's
  distance is at least 1e-60 and the box distance and epsilon at most
  1e60. The window decides only whether a ring is skipped or walked, and a
  skipped ring is one the walk would not take a connector from, so the
  triangles are the same either way; upstream's window walks more rings,
  and only at scales beyond 1e60 or below 1e-60.
- **Parallel stages** (`0003`; `6e127b5`, PR #8, unchanged, with
  `e91eeee`, `cf5463e` and `7b89fae`): `boolean3_kernels.rs`,
  `boolean_result_assemble.rs`, `edge_op.rs`, `face_op_triangulate.rs`
  and `sort.rs` are the patched files byte for byte, line endings
  aside. `edge_op_orbits.rs` is the same capped walk (`ORBIT_WALK_CAP`,
  64 steps, with `u8` roles instead of an enum, and a longer proof).
  `par.rs` adds a list of every site and `maybe_par_any_ct`, for the Auto
  engine's self-intersection pre-check, which neoscad does not use (it
  asks for the exact or the robust engine).
- **Batch rounds** (`0004`; `fb1a52e`, PR #7, on top of `a882339`): no
  renumbering after each round. Instead `compose_meshes` gives each
  input local mesh IDs in input order, ascending within an input, before
  `increment_mesh_ids`, which ranks them as C++ `Compose`'s offsets do,
  and a boolean already puts its right operand's IDs after its left's,
  so no output depends on which pair reserved IDs first. The same change
  fixed `compose_meshes` merging two instanced copies of one mesh into
  one run (upstream's "fixed mismatches"); neoscad's `ManifoldGeometry::batch`
  renumbered colliding operands to avoid that, and no longer does.
  The tests stay in `src/csg_tree.rs`.
- **Cancel checks in `AddNewEdgeVerts`** (`0005`; `e81b00f`, PR #10,
  unchanged, with its test `fc64db8`): the test-only poll countdown is
  `cancel_after_polls` (the patch's was `cancelling_after`).
- **Round threshold** (`0006`, NeoSCAD's): dropped; upstream runs every
  round of two or more pairs in parallel. Re-applied to 0.16.0 it
  measured no better, interleaved, best and median of 7 at a load average
  of 2 to 8: Menger level 4 2.140/2.186 s without it against 2.184/2.203
  s with it, `csg_spheres` 1.029/1.038 against 1.021/1.041 s,
  `csg_deep_union` 0.065 against 0.066 s, and `bosl_fractal_tree`,
  `ex_menger` and `bosl_screws__001` within 2%. With five busy loops per
  core (load average up to 210), best and median of 5: `csg_spheres`
  3.70/4.71 s without against 4.59/4.99 s with, Menger level 4
  13.55/14.20 against 13.72/14.38 s, `bosl_fractal_tree` 6.83/7.49
  against 7.26/8.18 s. Outputs were byte-identical every run.
- **Cancel token** (`0007`, now `0001`): the same API, re-applied to
  0.16.0's `src/cancel.rs`; its test-only countdown is upstream's.

The other changes that reach neoscad:

- QuickHull decides whether a face is above a point exactly
  (`orient3d`), where C++ uses the float plane distance (`a69e579`,
  divergence entry 11). A point collinear with a hull edge no longer
  builds a zero-area face and a folded hull. This is the one change in
  neoscad's output (below). `geom::hull::hull_3d` still checks every
  hull and rebuilds a folded one; on `mink_convex`
  (`minkowski() { cube([30,20,5], center=true); sphere(3, $fn=48); }`)
  it no longer has to, so the sum takes 0.007 s instead of 0.022 s and its
  volume is now the nightly's to 1e-10.
- Not reaching neoscad: the robust engine's faster coplanar cross-copy
  and self-intersection pre-check (same output, upstream says; neoscad
  uses the robust engine only for meshes its repair leaves as soups),
  cancellable Minkowski sums and `repair_orientation_with_token`, new
  progress phases, and CI with the `parallel` feature.

**Output.** Every model in `tests/data/scad` (527), `web/examples` (8)
and `examples` (6), plus the 14 benchmark models, Menger level 4 and
three text models (50 lines extruded; 50 and 200 lines in 2D), exported
by the old build (0.15.0 with seven patches) and the new one at 1, 4 and
the default number of threads (`--limit time=60 memory=900M`; the four
largest again at 1800M): 395 export a mesh or SVG, 377 byte-identical in
all six runs and 18 different. Each build gives the same bytes at every
thread count. All 18 are hulls or Minkowski sums, and all 18 are
QuickHull's: with 0.15.0's `quickhull.rs` and `quickhull_algo.rs` put
back into 0.16.0, they are byte-identical to the old build's. Their
volumes agree to 2.6e-8 relative (`issue1089b`, 1848.38907 against
1848.38911) and their areas to 4e-9, apart from
`3D/issues/issue2841.scad`, a Minkowski sum of a cube and two unioned
7-sided cylinders, whose area grew 8.36 mm² (0.8%): both builds leave a
thin internal slit (opposite faces about 1e-6 apart) between two of its
convex pieces, 1.9 mm² a side before and 6.1 mm² after, which the
nightly's CGAL sum does not have (`docs/followups.md`). The other 17
changed triangulation and vertex count (by up to 9%; the nightly's
CGAL-based Minkowski sums differ from both by more). Removing the mesh-ID
renumbering from `ManifoldGeometry::batch` changed none of the 395.
Conformance is 1,773 passing, 0 failing, at the default and at
`RAYON_NUM_THREADS=1`.

**Speed.** `conformance bench --quick`, interleaved, three rounds each:
the geometric mean of the nightly's (Manifold) time over neoscad's is
4.29, 4.52 and 4.42 for the old build and 4.92, 4.92 and 4.84 for the
new one. The gain is `mink_convex` (above); every other model is within
6%, the old build's best against the new one's (`csg_deep_union` 0.038
against 0.036 s, `bosl_fractal_tree` 0.664 against 0.632 s). Interleaved, best of 5:
Menger level 4 2.110 s old against 2.106 s new, 200 lines of extruded
text 1.472 against 1.489 s, 50 lines 0.356 against 0.350 s, 200 lines in
2D 0.532 against 0.536 s, `text_30lines` 0.125 against 0.129 s.

### Moving from 0.13.1 to 0.15.0

0.15.0 (upstream `a866917`, 35 commits after 0.13.1's `57be4a6`) is
mostly a `cargo fmt` sweep over the whole crate (`cfa31bf`), which is
why every 0.13.1 patch stopped applying. The changes that reach neoscad:

- `dedupe_edges` skips a duplicate entry that an earlier repair in the
  same pass already resolved (`4a99dc4`). Repairing the stale entry gave
  the orbit a copy of the wrong vertex. This is what fixed the
  edge-collapse bug below.
- A mirrored mesh keeps each property with its corner (`7fe2593`,
  `8e77334`, upstream Manifold's `422ab6fc`): the flip used to break
  `prop_vert == start_vert` even with no properties.
- `Impl::slice` interpolates as C++ does (`la::lerp`, `a52bb8e`) and
  traces contours from the lowest-indexed triangle (a `BTreeSet`, not a
  randomly seeded `HashSet`; `d3a5967`), and `Manifold::slice` now
  returns the `CrossSection` union of the loops (`9ae04a5`). neoscad's
  `ManifoldGeometry::slice` takes the raw loops from the implementation
  instead and unions them once, as `project` already did.
- The rest does not reach neoscad: `CrossSection` reworked to follow C++
  (neoscad uses Clipper directly), `MeshGL::merge` (neoscad merges in
  `manifold_geom.rs`), the centred cylinder's and `subdivide_impl`'s
  stale caches (`fa18cc5`; neoscad builds its own meshes), the
  `RebuildSolid` API, `manifold.rs` and `impl_mesh.rs` split into
  smaller files, and .NET and wasm packaging. `edge_op`'s collapse code,
  `polygon_earclip`, `csg_tree`, `par.rs`, the boolean kernels and
  `cancel.rs` changed only in formatting.

The `slice` change is the only one neoscad's code needed. Upstream
`main` has one code change since 0.15.0 (`a69e579`, exact QuickHull
visibility); it is not in a release.

The 0.13.1 series became this one as follows:

- the edge-collapse patch (`0001`) was dropped (below);
- the keyhole patches (`0002`, `0004`) are now `0001` and `0002`, the
  same code with comments reworded for upstream;
- the parallel boolean patch (`0003`) is now `0003` and `0004`: the
  kernels, and the batch rounds on their own. The orbit-scan helpers
  moved to a new file, `src/edge_op_orbits.rs`, and the orbit scans and
  `sort_geometry` now go parallel from 100,000 halfedges or elements
  instead of 10,000 (the edge-flag scans already did);
- the cancellation patch (`0006`) is now `0005`, the checks inside
  `AddNewEdgeVerts` (now at every intersection, not every 16,384), and
  `0007`, the token API;
- the round threshold (`0005`) is now `0006`.

The thresholds are upstream's choice: at 10,000 a fold of many small
unions got slower on a busy 14-core machine. On neoscad's models the
two settings measured the same. Interleaved, best of 9 under a load
average of about 9: Menger level 4 1.342 s at 100,000 against 1.343 s
at 10,000, and `csg_spheres` 0.361 against 0.363 s. On all 585 models
below, the output is byte-identical to the 0.13.1-style series rebased
by `rustfmt` onto 0.15.0, at 1, 4 and the default number of threads.

What changed in output, against 0.3.0 (0.13.1 and six patches), on 585
models that export a mesh or SVG (all of `tests/data/scad`, the bench
models, `web/examples`, `BOSL2/examples` and every twelfth file of
`BOSL2/examples_x`): 578 byte-identical and 7 different. In all 7 the
volume is the same to 1e-10 or closer:

- `web/examples/threaded-ring.scad` and BOSL2 `joiners__007` changed with
  the `dedupe_edges` fix: the same output as 0.3.0's once it is disabled.
  In the ring, one 3-triangle fan on each side has a different apex
  (area +0.13 of 2,130 mm²); in the joiner only the triangle order changed.
- `tests/data/scad/3D/issues/issue1138.scad` and BOSL2 `miscellaneous__004`,
  `rounding__036`, `shapes3d__220` and `transforms__043` changed because
  the edge-collapse patch was dropped: with it applied to 0.15.0 they are
  byte-identical to 0.3.0's. The changes are in zero-area triangles or
  are a different triangulation of the same flat region; triangle order
  is all that changed in `issue1138` and `miscellaneous__004`.

Conformance is 1,773 passing, 0 failing, at the default thread count and
with `RAYON_NUM_THREADS=1`. Every one of those models exports
byte-identical output at 1, 4 and the default number of threads.

### The edge-collapse bug (fixed in 0.15.0)

After a boolean, `simplify_topology` collapses "redundant" vertices: a new
vertex whose triangles come from at most two original faces is merged
into a neighbour. In 0.13.1 the vertex could slide across a crease and
change the solid. In BOSL2's `cubetruss` (docs/audits/engine-milestone.md,
finding 3), a union of two parts that touch along faces filled a
tetrahedral notch of 7.31 mm³. C++ Manifold 3.5.2 gave the same wrong
result on the same operands.

0.13.1 carried a patch (`0001-edge-collapse-crease.patch`, in git
history) that refused any collapse that moved a surrounding triangle out
of its own plane. 0.15.0 no longer needs it: the bad collapse followed a
`dedupe_edge` repair of a stale duplicate, and 0.15.0 skips those.
`crates/geom/tests/collapse_crease.rs` is the regression test: two
operands whose union is 328.29 instead of 314.49 when the bug is present.

- On 0.13.1 without the patch the test fails (328.29).
- On 0.15.0 without the patch it passes.
- On 0.15.0 with the stale-entry skip disabled (`dedupe_edges`) it fails
  again (328.29).

On the full cubetruss union, 0.15.0 with the patch and 0.15.0 without it
export byte-identical STL. The volume is 0.3.0's (85016.58; 85023.90
with neither fix). The area is 61960.35 mm², as the nightly's is, where
0.3.0 gives 61977.45: 0.3.0's output has a two-sided sheet of two
triangles, 8.55 mm² each. On 0.15.0 the patch still rejected a few
collapses: these are the five models listed above, and none of them
changes volume or area.

### The keyhole walks (upstream since 0.16.0)

NeoSCAD's patch until 0.16.0, which has it as upstream `c15d4ad` (PR #6)
on top of `8a963bd`, a pure move of the keyhole code into
`src/polygon_earclip_keyhole.rs`. What follows describes the patch as
NeoSCAD wrote it; where 0.16.0 differs is listed under "Moving from 0.15.0
to 0.16.0".

The ear clipper joins each hole to an outer ring through a keyhole.
`cut_keyhole` and `find_closer_bridge` look for the bridge by walking every
outer ring, once each per hole, and manifold-rust (0.13.1 and 0.15.0)
does that by cloning `outers` and collecting each ring into a fresh `Vec` (`loop_verts`). Since each
joined hole becomes part of its outer ring, the rings grow as holes are
cut, and on a square with 5,041 circular holes the collecting was 60% of
the run (docs/audits/performance.md, O9).

The patch adds `for_each_loop_vert`, which visits the same verts in the
same order without collecting them (as C++ Manifold's `Loop` does), and
the two bridge searches use it and borrow `outers`. `loop_verts` is now a
wrapper over it for the three once-per-polygon callers. The walk itself,
and so the quadratic cost, is unchanged. The keyhole code (the two
searches, `join_polygons` and the ring walk) moves, unchanged otherwise,
into a child module, `src/polygon_earclip_keyhole.rs`, which keeps
`polygon_earclip.rs` under upstream's 800-line limit.

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
24×24 grid of octagonal holes, hashed with the unpatched copy. The
crate's own tests (`src/polygon_earclip_tests.rs`) pin the same grid and
a hole that collapses an outer ring to two verts, so that both searches
for the next hole walk a degenerate ring; in practice a ring degenerates
whole, so the walk reports it before visiting any vert and the restore
has nothing to undo.

### The keyhole ring boxes (upstream since 0.16.0)

NeoSCAD's patch until 0.16.0, which has it as upstream `bc0a64d` (PR #9),
with a different magnitude window for the `ccw` cull (below, and under
"Moving from 0.15.0 to 0.16.0").

The keyhole walk change made each walk cheaper but left the quadratic
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

The `ccw` bound holds only where `ccw`'s own arithmetic does. At extreme
scales `ccw` calls a vert that clearly turns the wrong way collinear,
when `area * area` underflows to 0 or `base2 * tol * tol` overflows to
infinity, and the tie-break can then take it; the first version of the
patch skipped such a ring and so changed the bridge (found in upstream
review). NeoSCAD's revised patch applied the `ccw` cull only when the
corner distances, the connector's distance and epsilon were at most 1e75
and the margin at least 1e-150; upstream's `bc0a64d` applies it only when
the connector's distance is at least 1e-60 and the box distance and
epsilon at most 1e60 (`in_window` in `find_closer_bridge`). NaN fails
either test, and outside the window the ring is walked. The coordinate
tests hold at any scale. The crate's tests pin the review's
two cases (lengths of 1e-84 with epsilon 0, and 1e80 with epsilon 1e74),
with triangles taken on the unpatched code.

`crates/geom/tests/kernel_patches.rs` pins the triangles of a grid of
glyph-like outer rings with holes and islands, hashed before the patch.
Every 3D model in `tests/data/scad` and `examples` (301 STL exports), and
the 50- and 200-line text extrusions at 1 and 14 threads, export
byte-identical STL before and after. The 200-line extrusion went from
33.6 to 2.7 s (`docs/audits/slow-cases.md` §2).

### The parallel boolean stages (upstream since 0.16.0)

NeoSCAD's patches until 0.16.0, which has the kernels as upstream
`6e127b5` (PR #8, unchanged) plus the orbit-walk cap (`e91eeee`), and the
batch rounds as `fb1a52e` (PR #7) on top of `a882339`, a different fix
for the mesh-ID question below. 0.16.0's `src/par.rs` lists every site and
its threshold.

manifold-rust (0.13.1 and 0.15.0) runs most of a boolean on one thread,
where C++ Manifold runs it under TBB. On the level-4 Menger sponge the
last two differences (the cube minus the union of the three rotated
negatives, about 300,000 faces) ran alone for over a second while the
other cores idled (`docs/audits/slow-cases.md` §1). The stages below make
the large serial stages parallel, each in a way whose output is the
sequential output, so the result is byte-identical at any thread count
and to the serial crate's (measured on 0.13.1, below). Every site
goes through a helper in `src/par.rs` with a sequential twin for builds
without the `parallel` feature (the WASM build's pool runs on the
calling thread either way). By site:

- **`batch_boolean` rounds** (`csg_tree.rs`): a round's up to four pairs
  are picked first, then run side by side (C++ `csg_tree.cpp:451-479` in
  Manifold 3.5.2), and the results go back on the heap in pair order with
  the serials the sequential loop gave them. The one shared state a
  boolean touches is the mesh-ID counter, so the IDs a round's booleans
  reserve follow the scheduler. NeoSCAD's 0.15.0 patch renumbered them in
  pair order after each round, because `compose_meshes` (a disjoint
  union) then ranked its operands' IDs by value. In 0.16.0 `compose_meshes`
  ranks them node by node, as C++ `Compose` does (`a882339`), and a
  boolean puts its right operand's IDs after its left's, so no output
  depends on the values and upstream does not renumber. neoscad never
  reached that race anyway: its unions go through `batch_union`, which
  composes the disjoint operands into groups first, and the groups, and
  any union of them, overlap pairwise, and an intersection of disjoint
  operands is empty (and neoscad orders output runs sharing an original
  ID by their first triangle: `crates/geom/src/manifold_geom.rs`,
  `canonical_mesh`). Every round of two or more pairs runs in parallel.
  Until 0.16.0 NeoSCAD's own patch (`0006-batch-round-threshold.patch`)
  ran a round serially unless its operands had 10,000 vertices in all
  (C++ `autoPolicy`'s `kSeqThreshold`), measured as faster with five busy
  loops per core on 0.15.0 (`csg_spheres` 2.76 s against 3.70 s,
  `docs/audits/slow-cases.md` §1.1). Upstream did not take it, and on
  0.16.0 it measured no better, so it was dropped ("Moving from 0.15.0 to
  0.16.0").
- **Edge-flag scans** of `collapse_short_edges`, `collapse_colinear_edges`
  and `swap_degenerates` (`edge_op.rs`): the flags are tested in parallel
  above 100,000 halfedges (C++ `FlagStore::run`, `edge_op.cpp:54-97`) and
  collected in index order; the collapses and swaps stay sequential, as
  in C++.
- **Orbit scans** of `split_pinched_verts` and `dedupe_edges`: the
  sequential scan handles each vertex orbit from its smallest eligible
  halfedge, skipping halfedges an earlier orbit visited. Above 100,000
  halfedges (10,000 on 0.13.1) each halfedge instead walks its own orbit in parallel and
  owns it if it comes back to itself without meeting a smaller eligible
  one. That gives the same owners only if every orbit is a closed cycle,
  so it first checks that `paired_halfedge` is an involution (then the
  step is injective and a walk either closes or ends at a missing pair;
  an open orbit is seen by its smallest eligible halfedge) and falls back
  to the sequential scan otherwise. A walk stops after 64 steps; an orbit
  longer than that has no walk that returned, so its owner is its
  smallest capped halfedge, and each such orbit is then walked once,
  sequentially. Without the cap (the first version) a vertex of valence
  100,000 whose orbit meets its halfedges in ascending order cost 1e10
  steps. The owners' work (the pinched-vertex
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
  runs in parallel, in chunks of 1,024 halfedges that each check the
  cancel token first (C++'s `kSeqCancelChunk`), so a cancel does not wait
  for the whole search; the unions stay sequential in index order, because
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
copy. It also combines instances of one sphere (one original ID) in
batch rounds that run in parallel, two unions and an intersection, and
compares the kernel's whole `MeshGL64`, before `canonical_mesh`, and the
export on 1, 2, 3 and 8 threads, twice each. It passed with 0.15.0's
renumbering, without it, and on 0.16.0, for the reason given under the
batch rounds above.

**Timings** (M4 Pro, 14 cores, interleaved, best of 5; the machine was
shared, load average 5–9 unless noted): see `docs/audits/slow-cases.md`
§1 for the tables. Menger level 4 went from 2.62 s to 1.59 s (the
nightly: 2.29 s), `csg_spheres` from 0.57 to 0.47 s.

### The cancel token patch

manifold-rust ports Manifold's cooperative cancellation (`src/cancel.rs`:
a token checked between a boolean's stages, inside its long loops and
between `csg_tree`'s batch rounds; since 0.16.0 also in Minkowski sums
and `repair_orientation_with_token`), but `CancelToken::new` makes its
own flag, and the token knows only that flag. A request's cancel is the
host's flag, and its time and memory limits are only known to the
request's guard (the clock, and on wasm32 the counting allocator's
peak). So `0001-cancel-token-over-a-flag.patch`, NeoSCAD's own:

- `CancelToken::from_flag` makes a token over an existing
  `Arc<AtomicBool>`, and `with_check` adds a condition polled at every
  check; once it is true the flag is set, so the answer is sticky as
  before. `geom::manifold_geom::kernel_token` builds the request's token:
  its interrupt flag, and the guard's `stopped()` as the check. Without
  a token (`None`, every unlimited render) nothing is polled. `Debug` is
  written by hand, since the check is a closure.
- Upstream's test-only poll countdown (`cancel_after_polls`, under
  `#[cfg(test)]`, which lands a cancel at an exact check) is kept beside
  the check, and read first.

neoscad reaches the kernel's cancel points through `boolean_until` and
`batch_until` (`crates/geom/src/manifold_geom.rs`). It does not call
manifold-rust's Minkowski or `repair_orientation`: `geom::minkowski`
ports OpenSCAD's hull-and-union sum (OpenSCAD builds without
`USE_MANIFOLD_MINKOWSKI`), and the repair of a broken mesh is neoscad's
own (`orient_soup`), so the new cancel points there are not used.

### The cancellation checks inside the result assembly (upstream since 0.16.0)

NeoSCAD's patch until 0.16.0, which has it as upstream `e81b00f` (PR #10,
unchanged), its test as `fc64db8` and the poll countdown under the name
`cancel_after_polls` (NeoSCAD's was `cancelling_after`).

`AddNewEdgeVerts` (`src/boolean_result.rs`, called from
`src/boolean_result_assemble.rs`) checks the token at every
intersection, and between the three sorts after it, and its three lists
are allocated at their final size (the sum of the inclusions'
magnitudes) instead of grown by doubling. Upstream's check is one atomic
load; neoscad's also polls the guard, so under a limit a check costs a
clock read. NeoSCAD's earlier version checked every 16,384 intersections
instead: under `--limit time=1000`, Menger level 4 took 1.41 s with this
one against 1.36 s with that one (best of 5, load average about 9), and
without a limit the two take the same time. In the Menger sponge at
depth 5 (`examples/Old/example024.scad`, `n=5`) the last union grew a
wasm instance from under 1 GiB past 2 GB within this one step, before
any check ran; the last doubling of the new-edge list alone copied a
buffer into one twice its size.

The content and order of the lists are unchanged (a capacity is not
content), and an uncancelled token changes no output:
`crates/geom/tests/kernel_cancel.rs` compares a render with a token that
never fires against one without, on 1 and 8 threads, and checks that a
cancel lands inside one boolean (a boolean of two dense spheres stops
within milliseconds of the cancel, where it ran 140 ms in all) and that
a cancelled, empty result is never cached. In the web core, the Menger
sponge's depth-5 render under the 1 GiB limit stopped with a
`resource-limit` error at a measured peak of about 1.4 to 1.7 GiB when
the patch was written, and the engine lived on; `crates/web/test/run.mjs`
now checks a measured memory limit on `web/examples/gearbox.scad`
instead.

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

Both crates are Lars Brubaker's ports. clipper2-rust's `main`, fetched
2026-09-27, still had the code the drafts below replace. (The two
manifold-rust drafts that stood here became upstream PRs #6 and #9 and
are in manifold-rust 0.16.0.) The owner can send these as issues, with
the diff of the vendored file as the patch.

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
