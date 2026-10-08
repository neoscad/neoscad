// Half a hole at a plate's edge: the rim is an arc that ends on the
// plate's front face, a plane through its axis, where the material ends,
// so the tool runs on past it. Half a turn of the hole rim's spandrel.
// volume: 1871.488934865419
fillet_edges(r = 1, edges = "%circle and >z")
  difference() {
    cube([20, 20, 5]);
    translate([10, 0, -1]) cylinder(r = 4, h = 7);
  }
