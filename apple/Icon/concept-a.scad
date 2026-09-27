// NeoSCAD icon, concept A: "Lattice cube".
// camera: 0,0,0,60,0,35,100
// tile: #26305a #0c1024
//
// (The camera and tile lines are read by scripts/apple/build-icon.sh.)
//
// A rounded cube hollowed into a 2x2x2 smooth lattice: one oversized
// sphere is subtracted per octant, big enough to break through the faces
// and into its neighbours, which leaves a Schwarz-P-like frame of saddle
// surfaces made from nothing but a hull and eight differences. A small
// sphere floats in each cell, so the icon shows a second colour through
// the openings (a nested boolean) without changing the silhouette, which
// stays one rounded block at 16 px.

s = 10;       // half the cube's side
r = 2;        // edge rounding radius
cell = 6.1;   // radius of the sphere carved out of each octant
seed = 3.4;   // radius of the sphere left floating in each cell

module rounded_cube(h, rr) {
    hull()
        for (x = [-1, 1], y = [-1, 1], z = [-1, 1])
            translate([x, y, z] * (h - rr)) sphere(rr, $fn = 64);
}

octants = [for (x = [-1, 1], y = [-1, 1], z = [-1, 1]) [x, y, z] * s / 2];

color("#14a3c7")
difference() {
    rounded_cube(s, r);
    for (c = octants) translate(c) sphere(cell, $fn = 128);
}

color("#ff4f8b")
for (c = octants) translate(c) sphere(seed, $fn = 96);
