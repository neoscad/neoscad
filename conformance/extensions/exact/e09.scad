// Stage 2 of docs/audits/exact-geometry-rust.md: extrusions.
// A rounded ring sector: offset(r) of a rectangle revolved by 270 degrees (tori beside cylinders and planes).
// volume: 1555.0466521699227
rotate_extrude(angle=270) translate([5, 0]) offset(r=1) square([4, 6]);
