// A convex edge that runs into a wall: the tool is cut by the wall (7.1).
// volume: 4987.123889803847
fillet_edges(r = 2, edges = "convex and box(4, -1, 9, 21, 1, 11)") union() { cube([20, 20, 10]); cube([5, 20, 20]); }
