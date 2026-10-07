// Stage 2 of docs/audits/exact-geometry-rust.md: extrusions.
// A plate with rounded corners (offset(r) after offset(delta)) and a drilled hole.
// volume: 5463.008881569225
difference() {
  linear_extrude(10) offset(r=2) offset(delta=-2) square([30, 20]);
  translate([15, 10, -1]) cylinder(r=4, h=12);
}
