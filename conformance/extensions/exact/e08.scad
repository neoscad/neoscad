// Stage 2 of docs/audits/exact-geometry-rust.md: extrusions.
// A groove: a quarter circle cut from a revolved square (a torus beside a cylinder and planes).
// volume: 2486.065588055809
rotate_extrude() difference() { square([10, 10]); translate([10, 10]) circle(4); }
