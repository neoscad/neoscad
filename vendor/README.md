# Vendored dependencies

## manifold-rust 0.13.1, patched

A copy of the crates.io release (`.cargo_vcs_info.json` gives the upstream
commit), used through `[patch.crates-io]` in the root `Cargo.toml`. It is
not a workspace member, so the workspace's lints, formatting and tests do
not apply to it. The only change is in `src/edge_op.rs`, marked
`NeoSCAD patch`; drop the copy once upstream has a fix.

### The bug

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

### The patch

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
