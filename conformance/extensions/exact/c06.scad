// The exact-geometry audit's case c06 (docs/audits/exact-geometry-rust.md).
// volume: 3991.0619901184537
difference() {
  cube([30, 30, 5]);
  translate([7.5, 7.5, -1]) cylinder(r=2.25, h=7);
  translate([7.5, 7.5, 0.5]) cylinder(r1=0, r2=4.51, h=4.51);
  translate([7.5, 22.5, -1]) cylinder(r=2.25, h=7);
  translate([7.5, 22.5, 0.5]) cylinder(r1=0, r2=4.51, h=4.51);
  translate([22.5, 7.5, -1]) cylinder(r=2.25, h=7);
  translate([22.5, 7.5, 0.5]) cylinder(r1=0, r2=4.51, h=4.51);
  translate([22.5, 22.5, -1]) cylinder(r=2.25, h=7);
  translate([22.5, 22.5, 0.5]) cylinder(r1=0, r2=4.51, h=4.51);
}
