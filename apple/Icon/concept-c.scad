// NeoSCAD icon, concept C: "Threaded ring".
// camera: 0,0,0,40,0,20,70
// tile: #1f2238 #0a0b14
//
// (The camera and tile lines are read by scripts/apple/build-icon.sh.)
//
// A torus carved by a smooth helical channel that winds round the tube
// while it circles the ring. The channel is a tube swept along that
// path (one polyhedron), subtracted from the ring.
// The ring is built as wedges, each its own colour, which gives the solid
// a cyan-to-violet-to-magenta sweep (color() applies per object, so a
// gradient has to be made of pieces). At 16 px it is a ring, unlike both
// a block and OpenSCAD's sphere.

R = 7;          // ring radius (centre of the tube)
r = 3.6;        // tube radius
turns = 6;      // how many times each channel winds round the tube
groove = 1.4;   // channel radius
// Tessellation. Facets are shaded flat, so each one shows as a band of
// its own shade: at 144/72 round the ring and tube, and a channel of 180
// hulled 28-gon spheres, the 1024 px icon was visibly striped. These
// counts are about as high as a render of under 3 s allows (2.8 s to STL
// on an M4 Pro); the channel's steps * groove_fn costs the most.
steps = 540;    // channel cross-sections, round the whole ring
groove_fn = 72; // segments round the channel's cross-section
ring_fn = 432;  // segments round the ring (rotate_extrude)
tube_fn = 216;  // segments round the tube's cross-section
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

// The channel is a circle of radius `groove` swept along the path, in
// the plane normal to it. It used to be a hull between each pair of
// consecutive spheres, which is nearly the same solid, but 360 hulls of
// 48-gon spheres already took 10 s to render. The circle's plane is
// spanned by the tube's outward normal n (the path lies on the tube, so
// n is perpendicular to its tangent t) and t x n; both follow the path
// smoothly and close up after a full turn, so the tube does not twist. Faces wind so that they face outward: reversed, the polyhedron
// is inside out and the difference below silently removes nothing.
module channel() {
    for (dir = dirs) {
        pts = [for (k = [0 : steps - 1]) let(
                    a = k * 360 / steps,
                    p = path(a, dir),
                    w = dir * a * turns,
                    t = path(a + 0.5, dir) - path(a - 0.5, dir),
                    n = [cos(w) * cos(a), cos(w) * sin(a), sin(w)],
                    b = cross(t, n) / norm(cross(t, n)))
                for (j = [0 : groove_fn - 1]) let(c = j * 360 / groove_fn)
                    p + groove * (cos(c) * n + sin(c) * b)];
        faces = [for (k = [0 : steps - 1], j = [0 : groove_fn - 1]) let(
                    k1 = (k + 1) % steps, j1 = (j + 1) % groove_fn)
                    [k * groove_fn + j, k1 * groove_fn + j, k1 * groove_fn + j1, k * groove_fn + j1]];
        polyhedron(pts, faces);
    }
}

// The ring is cut into coloured wedges and each wedge is carved by the
// whole channel. Carving each wedge with only the nearby part of the
// channel is tempting but wrong: the channel is wide, so on the inside of
// the ring a stretch well outside a wedge's angles still cuts into it,
// and the result has ragged, half-cut grooves. Each wedge is its own
// partial rotate_extrude rather than the whole ring intersected with a
// prism, because that intersection handled the full, finely tessellated
// ring 36 times and cost more than the channel did.
for (i = [0 : wedges - 1])
    color(sweep(i / wedges))
    difference() {
        rotate(i * 360 / wedges)
            rotate_extrude(angle = 360 / wedges, $fn = ring_fn)
                translate([R, 0]) circle(r, $fn = tube_fn);
        channel();
    }
