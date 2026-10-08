// The top rim of a blind hole chamfered: its wall ends on the hole's
// floor, which the cone tool does not reach.
// volume: 7819.882021194186
chamfer_edges(d = 1, edges = "%circle and >z")
  difference() {
    cube(20);
    translate([10, 10, 14]) cylinder(r = 3, h = 7);
  }
