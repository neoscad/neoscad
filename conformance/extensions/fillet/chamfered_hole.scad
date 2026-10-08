// 12.4: the top rim of a hole chamfered, a cone tool subtracted.
// volume: 3706.784685664953
chamfer_edges(d = 1, edges = "%circle and >z")
  difference() {
    cube([20, 20, 10]);
    translate([10, 10, -1]) cylinder(d = 6, h = 12);
  }
