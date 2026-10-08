// 12.2, the box: its vertical edges rounded, then its top outline, a
// tangent chain of four lines and four arcs built as one tool (four
// cylinders, four quarter tori).
// volume: 23460.397414464685
L = 40; W = 30; H = 20; R = 5;
fillet_edges(r = 2, edges = ">z") fillet_edges(r = R, edges = "|z") cube([L, W, H]);
