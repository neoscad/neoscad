// The exact-geometry audit's case x01 (docs/audits/exact-geometry-rust.md).
// volume: 1570.7963267948965
union() { cylinder(r=5, h=10); translate([0,0,10]) cylinder(r=5, h=10); }
