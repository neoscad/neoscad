// NeoSCAD icon, concept C: "Threaded ring".
// camera: 0,0,0,40,0,20,70
// tile: #1f2238 #0a0b14
//
// (The camera and tile lines are read by scripts/apple/build-icon.sh.)
//
// A torus carved by a smooth helical channel that winds round the tube
// while it circles the ring. The channel is a chain of hulls between
// consecutive spheres along its path, subtracted from the ring.
// The ring is built as wedges, each its own colour, which gives the solid
// a cyan-to-violet-to-magenta sweep (color() applies per object, so a
// gradient has to be made of pieces). At 16 px it is a ring, unlike both
// a block and OpenSCAD's sphere.

R = 7;          // ring radius (centre of the tube)
r = 3.6;        // tube radius
turns = 6;      // how many times each channel winds round the tube
groove = 1.4;   // channel radius
steps = 180;    // hull segments per channel, round the whole ring
wedges = 36;    // colour pieces round the ring
// Winding directions: [1] is one helix; [-1, 1] crosses two into a braid,
// which reads as noise rather than knurling at icon sizes.
dirs = [1];

palette = ["#19c3d6", "#3f8ff0", "#6a5cf2", "#9b4de6", "#d04fc4", "#ff5a8a"];

function mix(a, b, t) = a + (b - a) * t;
function hex(c, i) = search(c[i], "0123456789abcdef")[0];
function rgb(c) = [for (i = [1, 3, 5]) (hex(c, i) * 16 + hex(c, i + 1)) / 255];
// Blend through the palette and back again as t runs 0..1, so the sweep
// closes round the ring without a hard seam where the wedges meet.
function sweep(t) =
    let(u = t < 0.5 ? 2 * t : 2 - 2 * t,
        x = u * (len(palette) - 1),
        i = min(floor(x), len(palette) - 2))
    mix(rgb(palette[i]), rgb(palette[i + 1]), x - i);

// A point on the tube's surface: angle a round the ring, and the channel
// has wound dir * turns * a round the tube by then.
function path(a, dir) = let(w = dir * a * turns)
    [(R + r * cos(w)) * cos(a), (R + r * cos(w)) * sin(a), r * sin(w)];

module channel() {
    for (dir = dirs, k = [0 : steps - 1])
        hull() {
            translate(path(k * 360 / steps, dir)) sphere(groove, $fn = 28);
            translate(path((k + 1) * 360 / steps, dir)) sphere(groove, $fn = 28);
        }
}

module carved_ring() {
    difference() {
        rotate_extrude($fn = 144) translate([R, 0]) circle(r, $fn = 72);
        channel();
    }
}

// A prism covering ring angles [i, i + 1] * 360 / wedges. Neighbouring
// prisms compute their shared edge from the same expression, so they meet
// exactly and the coloured pieces tile the ring with no slivers.
module wedge(i) {
    far = 2 * (R + r);
    a0 = i * 360 / wedges;
    a1 = (i + 1) * 360 / wedges;
    translate([0, 0, -far / 2])
        linear_extrude(far)
            polygon([[0, 0], far * [cos(a0), sin(a0)], far * [cos(a1), sin(a1)]]);
}

// The ring is carved once and then cut into coloured wedges. Carving each
// wedge separately with only the nearby part of the channel is tempting
// but wrong: the channel is wide, so on the inside of the ring a stretch
// well outside a wedge's angles still cuts into it, and the result has
// ragged, half-cut grooves.
for (i = [0 : wedges - 1])
    color(sweep(i / wedges))
    intersection() {
        carved_ring();
        wedge(i);
    }
