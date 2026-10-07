// The exact-geometry audit's case x05 (docs/audits/exact-geometry-rust.md).
// volume: 3761.2389583271756
difference() { cube([20,20,10]); translate([10,10,-1]) cylinder(r=2, h=12); translate([10,10,7]) cylinder(r=4, h=3); }
