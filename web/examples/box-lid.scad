// A box and its lid, side by side on the bed, with rounded corners
// (minkowski) and a lip on the lid that fits inside the box's wall.
// Written for the NeoSCAD web demo, after the box-and-lid task of the
// agent evaluation (docs/agent-eval.md). CC0: do what you like with it.
//
// It uses NeoSCAD's part() extension (`--enable part`; the demo turns it
// on for this example) so that Check and Measure can name the box and the
// lid: in Measure, with Parts on, choose "box" and "lid" under "Between
// parts". Without the extension (OpenSCAD, or Parts off) part() is an
// unknown module: it is ignored with a warning, and nothing is drawn.

/* [Size] */
// Outside width (mm)
width = 60; // [30:5:120]
// Outside depth (mm)
depth = 40; // [30:5:120]
// Outside height of the box (mm)
height = 25; // [10:1:60]
// Corner radius (mm)
radius = 4; // [1:0.5:10]

/* [Print] */
// Wall and floor thickness (mm)
wall = 2; // [0.8:0.2:4]
// Gap between the lid's lip and the box's wall, per side (mm)
clearance = 0.2; // [0:0.05:0.6]
// Thickness of the lid's plate (mm)
lid_thickness = 2; // [1:0.5:4]
// Height of the lid's lip (mm)
lip = 4; // [2:1:10]
// Space between the box and the lid on the bed (mm)
gap = 8; // [2:1:20]

/* [Hidden] */
$fn = 32;

// A slab `h` tall whose outline is a `size` rectangle with corners of
// radius `r`: the rectangle shrunk by r, then grown back by a disc. Half
// the height comes from each, so the sum is `h`.
module rounded(size, r, h) {
    minkowski() {
        translate([r, r, 0]) cube([size[0] - 2 * r, size[1] - 2 * r, h / 2]);
        cylinder(r = r, h = h / 2);
    }
}

// Inner corners keep the wall even: the outer radius less the wall, but
// never so small that minkowski gets a degenerate disc.
function inner(r, by) = max(0.5, r - by);

part("box")
    difference() {
        rounded([width, depth], radius, height);
        translate([wall, wall, wall])
            rounded([width - 2 * wall, depth - 2 * wall], inner(radius, wall), height);
    }

// The lid prints plate down with the lip up. The lip starts half a
// millimetre inside the plate so the two fuse into one solid rather than
// touching along a face.
inset = wall + clearance;
part("lid")
    translate([width + gap, 0, 0]) {
        rounded([width, depth], radius, lid_thickness);
        difference() {
            translate([inset, inset, lid_thickness - 0.5])
                rounded([width - 2 * inset, depth - 2 * inset], inner(radius, inset), lip + 0.5);
            translate([inset + wall, inset + wall, lid_thickness])
                rounded([width - 2 * (inset + wall), depth - 2 * (inset + wall)], inner(radius, inset + wall), lip + 1);
        }
    }
