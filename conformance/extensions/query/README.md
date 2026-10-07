# Query goldens

Models for NeoSCAD's queries (`--enable query`: `anchor()`,
`child_anchors()`, `child_bounds()` and `child_measure()`;
`docs/language-extensions.md`, sections 5.2 to 5.4), not OpenSCAD's:
they are not in `conformance/manifest.json` and `conformance run` does
not read them.

Each `NAME.scad` has what the command line prints for it with
`--enable query --enable sketch` (`NAME.echo`) and its `.csg` export
(`NAME.csg`). `crates/session/tests/query.rs` checks them; rewrite them
with `NEOSCAD_BLESS=1 cargo test -p neoscad-session --test query` after a
change meant to alter them, and read the diff.

- `anchors.scad`: anchors through every kind of node: transforms, the
  operations that keep coordinates, linear extrusion and projection, and
  the ones that hide them (`resize`, `rotate_extrude`, a NaN transform);
  anchors among the children; `child_anchors()` with indices.
- `reuse.scad`: the sandbox: nested queries, `$` variables the child
  reads, `rands()`, a child never instantiated, a child instantiated
  twice, repeated calls for the call memo, held-back messages. Every
  echo prints where it would with no query.
- `diagnostics.scad`: every warning, at its span.
- `error.scad`, `cycle.scad`: an error in the queried child, and a child
  that asks about itself; each stops evaluation.
- `sketch.scad`: sketch anchors (named entities and `anchor(name,
  entity)`), a helper sketch, and a sketch whose solve fails.
- `plate.scad`: the design's section 6.3, with the child's extent read
  from its anchors.
- `plate-bounds.scad`: the design's section 6.3 as written, with
  `child_bounds()`.
- `bounds.scad`: `child_bounds()` and `child_measure()` on 2D and 3D
  children, booleans, `%` and `#` children, mixed dimensions, empty
  children, indices, nested queries, a recursion, and their warnings.

The `.csg` files are plain OpenSCAD: an anchor makes no node and a query
is a value, so the stock nightly renders them (`--backend=manifold`) to
the meshes NeoSCAD renders from them (to the six significant digits a
`.csg` prints its numbers with).
