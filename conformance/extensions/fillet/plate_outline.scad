// A mixed chain from 2D: a rounded rectangle extruded (its arcs are
// offset()'s exact arcs), its top outline filleted as one tool.
// volume: 3235.571283563743
fillet_edges(r = 1.5, edges = ">z") linear_extrude(6) offset(r = 4) square([20, 12]);
