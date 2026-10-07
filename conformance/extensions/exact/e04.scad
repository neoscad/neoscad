// Stage 2 of docs/audits/exact-geometry-rust.md: extrusions.
// A rounded rectangle: offset(r) arcs become cylinders tangent to the flat sides.
// volume: 2041.3716694115408
linear_extrude(5) offset(r=3) square([20, 10]);
