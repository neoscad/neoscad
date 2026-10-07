// The exact-geometry audit's case c12 (docs/audits/exact-geometry-rust.md).
// volume: 2286.313079308426
difference() { cylinder(r=5, h=30); translate([0,0,20]) rotate([90,0,0]) cylinder(r=1.5, h=12, center=true); }
