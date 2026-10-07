// The exact-geometry audit's case c11 (docs/audits/exact-geometry-rust.md).
// volume: 1047.7106933894745
difference() { cylinder(r=10, h=5, $fn=6); translate([0,0,-1]) cylinder(r=4, h=7); }
