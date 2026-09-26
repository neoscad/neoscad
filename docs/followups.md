# Follow-ups

Deferred items found along the way, with where they came from. Remove an
entry when it is done.

## Performance
- Deep union trees and the level-4 Menger sponge render slower than the
  nightly (3.0 s vs 1.8 s and 1.3 s vs 0.7 s). Total CPU is the same, but
  the nightly spreads the work across cores better. (5a)

## Parity
- `manifold-rust` 0.13.1 ports Manifold v3.5.0; OpenSCAD pins v3.5.2.
  `minkowski_sum` of two unit cubes gave volume 8.875 rather than 8; check
  it against the C++ library before relying on it in 5c. (5a)
- Faces with more than three vertices are split by ear clipping, where
  OpenSCAD uses libtess2. The surface is the same, but STL/OBJ bytes differ
  for quads. (5a)
- A render warning for a duplicated sibling subtree is printed once;
  OpenSCAD prints it again, because of how it caches. (5a)
- `--hardwarnings` is parsed but has no effect. Only tier 5 uses it.
  (2, 3, 5a)
- `r(3000)`-style recursion succeeds where the nightly stops with a
  recursion error. The deeper limit is deliberate, but decide whether a
  compatibility mode should match the nightly. (3, 5a)
- A `\r` inside `include<>`/`use<>` brackets doesn't count as a new line,
  as it does in OpenSCAD. (2)
- Malformed parameter-set JSON gives different error text from Boost. (2)

## Structure
- The DXF reader lives in `crates/eval/src/dxf.rs` for `dxf_dim` and
  `dxf_cross`; move it to `io` when that crate exists. (3)
- The tier 3 baseline needs the pinned nightly installed as its renderer.
  CI would need it too. (5a)
