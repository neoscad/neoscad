// The exact-geometry audit's case x03 (docs/audits/exact-geometry-rust.md).
// volume: 4392.699081698724
union() {
  translate([5,0,0]) cube([20,30,5]);
  translate([0,5,0]) cube([30,20,5]);
  for (p = [[5,5],[25,5],[5,25],[25,25]]) translate([p[0],p[1],0]) cylinder(r=5, h=5);
}
