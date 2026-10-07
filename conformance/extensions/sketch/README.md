# Constrained sketch goldens

Models for NeoSCAD's `sketch()` extension (`--enable sketch`;
`docs/language-extensions.md`), not OpenSCAD's: they are not in
`conformance/manifest.json` and `conformance run` does not read them.

Each `NAME.scad` has what the command line prints for it with
`--enable sketch` (`NAME.echo`) and its `.csg` export (`NAME.csg`).
`crates/session/tests/sketch.rs` checks both; rewrite them with
`NEOSCAD_BLESS=1 cargo test -p neoscad-session --test sketch` after a
change meant to alter them, and read the diff.

- `gusset.scad`, `slot.scad`: the design's worked examples (sections 6.1
  and 6.2).
- `holes.scad`: holes and nested loops (even-odd), and sketch circles
  next to `circle()` at several `$fn`, `$fa`, `$fs`.
- `scoping.scad`: BOSL2's and MCAD's names outside sketch bodies, and
  helper modules whose body is a `sketch()`.
- `handles.scad`: entity handles as values.
- `diagnostics.scad`: every diagnostic, at its span.

The `.csg` files are plain OpenSCAD: the stock nightly renders them
(`--backend=manifold`) to the meshes NeoSCAD renders from the sources, to
the 6 digits a `.csg` prints.
