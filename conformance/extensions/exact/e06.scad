// Stage 2 of docs/audits/exact-geometry-rust.md: extrusions.
// A quarter torus: rotate_extrude(angle=90), which wraps the tube but not the axis.
// volume: 444.1321980490211
rotate_extrude(angle=90) translate([10, 0]) circle(3);
