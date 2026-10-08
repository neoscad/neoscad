// Every edge of a cube chamfered: three convex chamfers per corner, each
// extended, meet in a point; 12 prisms less what they share (1/3 per pair,
// 1/4 for the three).
// volume: 946.0
chamfer_edges(d = 1) cube(10);
