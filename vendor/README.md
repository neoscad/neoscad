# Vendored dependencies

## manifold-rust 0.13.1, patched

A copy of the crates.io release (`.cargo_vcs_info.json` gives the upstream
commit), used through `[patch.crates-io]` in the root `Cargo.toml`. It is
not a workspace member, so the workspace's lints, formatting and tests do
not apply to it. It carries two changes, each marked `NeoSCAD patch`: a
bug fix in `src/edge_op.rs` and a speed fix in `src/polygon_earclip.rs`
(below). Drop the copy once upstream has both.

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

## clipper2-rust 1.2.0, patched

A copy of the crates.io release (`.cargo_vcs_info.json` gives the upstream
commit), used through `[patch.crates-io]` in the root `Cargo.toml`, not a
workspace member, like manifold-rust. Both neoscad (`crates/geom`,
`crates/io`) and manifold-rust depend on it, and the patch applies to
both. It carries two changes: `nearbyint_f64` in `src/core.rs`, marked
`NeoSCAD patch`, and `rust-version` raised from 1.70 to 1.77 in
`Cargo.toml` (the release that stabilised `f64::round_ties_even`; the
workspace needs 1.98 anyway). Drop the copy once upstream has the fix.

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
