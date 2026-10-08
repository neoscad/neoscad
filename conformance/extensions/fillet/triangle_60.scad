// Plane-plane at 60 degrees: an equilateral prism with its vertical edges
// rounded; each spandrel is r^2 (cot 30 - (pi - pi/3) / 2).
// volume: 1649.8684168042037
fillet_edges(r = 2, edges = "|z") linear_extrude(10) polygon([[0, 0], [20, 0], [10, 17.320508075688775]]);
