// The exact-geometry audit's case x07 (docs/audits/exact-geometry-rust.md).
difference() { hull() sphere(10, $fn=12); rotate([30,20,0]) cylinder(r=3, h=30, center=true); }
