// The mitred boss base with the child rotated inside the call: its planes
// off the axes, so the mitred tools must share their rings bit for bit.
// volume: 3037.4041447323048
fillet_edges(r = 2, edges = "concave") rotate([10, 20, 30]) union() { cube([20, 20, 5]); translate([5, 5, 0]) cube([10, 10, 15]); }
