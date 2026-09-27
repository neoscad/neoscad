// NeoSCAD icon, concept B: "Cutaway core".
// camera: 0,0,0,60,0,35,95
// tile: #2a2f45 #0e1018
//
// (The camera and tile lines are read by scripts/apple/build-icon.sh.)
//
// Three nested solids, a hollow rounded cube around a hollow sphere
// around a solid rounded cube, with the octant facing the viewer
// subtracted from all of them, so the cut reveals the layers like an
// engineering section. It is the plainest of the three concepts: a
// rounded block with a notch is legible at 16 px, and the stepped section
// says "difference" at any size. Alternating cube and sphere keeps it
// clear of OpenSCAD's sphere-based logo. Each layer is its own colour to
// show the nesting.

$fn = 128;

module rounded_cube(h, rr) {
    hull()
        for (x = [-1, 1], y = [-1, 1], z = [-1, 1])
            translate([x, y, z] * (h - rr)) sphere(rr, $fn = 64);
}

// The octant nearest the default (55,0,25-style) camera: +x, -y, +z.
module octant(size) {
    translate([0, -size, 0]) cube(size);
}

wall = 1.3;

color("#4b3fd1")
difference() {
    rounded_cube(10, 2.4);
    rounded_cube(10 - wall, 2.4 - wall);
    octant(11);
}

color("#18b3cc")
difference() {
    sphere(8.2);
    sphere(8.2 - wall);
    octant(11);
}

color("#ff5a6e")
difference() {
    rounded_cube(4.4, 1.2);
    octant(11);
}
