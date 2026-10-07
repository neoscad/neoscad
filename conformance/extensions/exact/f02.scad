// The exact-geometry audit's case f02 (docs/audits/exact-geometry-rust.md).
// volume: 3420.0885142487145
difference() {
  cube([20,20,10]);
  for (c = [[17,17,0],[3,17,90],[3,3,180],[17,3,270]])
    translate([c[0],c[1],-1]) rotate(c[2]) difference() { cube([4,4,12]); cylinder(r=3, h=12); }
  translate([10,10,-1]) cylinder(r=4, h=12);
}
