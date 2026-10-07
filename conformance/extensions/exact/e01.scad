// Stage 2 of docs/audits/exact-geometry-rust.md: extrusions.
// A washer: a linear_extrude of a circle with a circular hole (two cylinders).
// volume: 791.6813487046279
linear_extrude(3) difference() { circle(10); circle(4); }
