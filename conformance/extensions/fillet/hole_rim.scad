// A convex fillet round the top rim of a hole through a plate: the
// spandrel revolved outside the hole, at 5 + 0.2234 r from its axis.
// volume: 8185.224690026346
fillet_edges(r = 2, edges = "%circle and >z")
  difference() {
    cube([30, 30, 10]);
    translate([15, 15, -1]) cylinder(r = 5, h = 12);
  }
