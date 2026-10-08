// 12.2's box under a rotation: the top outline's tools are placed under
// the matrix above the call, and the export finds the polygon they
// conform to in the call's own coordinates.
// volume: 23460.397414464685
L = 40; W = 30; H = 20; R = 5;
rotate([10, 20, 30])
  fillet_edges(r = 2, edges = ">z") fillet_edges(r = R, edges = "|z") cube([L, W, H]);
