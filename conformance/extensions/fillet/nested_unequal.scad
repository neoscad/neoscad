// Nested calls of unequal radii (stage F5a): the vertical edges with
// r = 2, then everything with r = 1, whose top and bottom outlines run
// line, arc, line round the first call's blends. Each arc's blend is a
// horn torus (centre 2 − 1 = 1 from the axis, radius 1). The opening of
// an 18 × 18 × 8 rounded-corner core (corner radius 1) by a unit ball:
// 8(A + P + π) + 2(A + Pπ/4 + 2π/3) with A = 320 + π, P = 64 + 2π,
// = 3712 + 202π/3 + π².
// volume: 3933.4035097428023
fillet_edges(r = 1) fillet_edges(r = 2, edges = "|z") cube([20, 20, 10]);
