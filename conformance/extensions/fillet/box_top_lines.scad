// The straight parts of the box top outline of 12.2: lines that run into
// the unselected arcs of the rounded vertical edges are cut across there (7.2).
// volume: 23484.955592153878
fillet_edges(r = 2, edges = ">z and %line") fillet_edges(r = 5, edges = "|z") cube([40, 30, 20]);
