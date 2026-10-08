// Concave edges round a square boss, mitred at its sharp vertical edges
// (7.3, section 18).
// volume: 3037.4041447323048
fillet_edges(r = 2, edges = "concave") union() { cube([20, 20, 5]); translate([5, 5, 0]) cube([10, 10, 15]); }
