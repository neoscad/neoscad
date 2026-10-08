// Three concave edges at an inside corner: an added sphere patch.
// volume: 4662.289096305211
fillet_edges(r = 2, edges = "concave") difference() { cube(20); translate([5, 5, 5]) cube(20); }
