// A concave fillet between a plane and a parallel cylinder (a boss half
// outside a block).
// volume: 4395.964377249046
fillet_edges(r = 1, edges = "|z and concave") union() { cube([20, 20, 10]); translate([20, 10, 0]) cylinder(r = 5, h = 10); }
