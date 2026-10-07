// The exact-geometry audit's case f01 (docs/audits/exact-geometry-rust.md).
// volume: 7961.371669411541
difference() { cube(20); translate([17,17,-1]) difference() { cube([4,4,22]); cylinder(r=3, h=22); } }
