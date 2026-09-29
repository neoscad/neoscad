// T3 reference for the grader's self-check (grade_selftest.py): an M24x2
// hose-barb adapter. The thread is a true helical surface: a polyhedron
// whose radius at angle a and height z is a profile of z - P*a/360.
// `rings = true` makes the stacked-ring fake the spec bans (the profile of
// z alone); `hand = -1` a left-hand thread; `pitch`, `af` for more variants.
// `order = "hex-thread-barb"` stacks the segments flange-down, as the
// NeoSCAD agent did in cad-20260928T231444Z (the spec states no order);
// `hex_cham` chamfers the flange's lower corners with a cone over that
// height (CadQuery's part there); `barb_stem` ends the barb in a plain
// stem of that length with a 0.6 tip chamfer (OpenSCAD's part there);
// `barbs` sets their number; `flip = true` turns the part over.
pitch = 2;
major = 23.8;
depth = 1.2;
thread_len = 12;
af = 30;
flange = 8;
barb_len = 25;
channel = 8;
rings = false;
hand = 1;
skirt = "none";
skirt_dz = 0;
order = "thread-hex-barb";
hex_cham = 0;
barb_stem = 0;
barbs = 3;
flip = false;
$fn = 64;

function prof(s) = let(f = s / pitch - floor(s / pitch))
    // Trapezoid: flat crest and root, 60-degree-ish flanks.
    f < 0.125 ? 0 : f < 0.5 ? (f - 0.125) / 0.375 : f < 0.625 ? 1 : 1 - (f - 0.625) / 0.375;

function rad(a, z) = major / 2 - depth + depth * prof(rings ? z : z - hand * pitch * a / 360);

module thread() {
    na = 96;
    nz = ceil(thread_len / pitch * 16);
    pts = [for (j = [0:nz]) for (i = [0:na - 1])
        let(a = 360 * i / na, z = thread_len * j / nz, r = rad(a, z)) [r * cos(a), r * sin(a), z]];
    side = [for (j = [0:nz - 1]) for (i = [0:na - 1])
        let(p = j * na + i, q = j * na + (i + 1) % na)
            each [[p, q + na, q], [p, p + na, q + na]]];
    // OpenSCAD wants faces clockwise seen from outside.
    bottom = [[for (i = [0:na - 1]) i]];
    top = [[for (i = [na - 1:-1:0]) nz * na + i]];
    polyhedron(pts, concat(side, bottom, top));
}

root_r = major / 2 - depth;
hex_r = af / 2 / cos(30);
skirt_z = thread_len + skirt_dz;
flange_z = skirt == "none" ? thread_len : skirt_z + hex_r - root_r;

module skirt() {
    if (skirt == "cone")
        translate([0, 0, skirt_z]) cylinder(r1 = root_r, r2 = hex_r, h = flange_z - skirt_z + 0.01);
    else if (skirt == "hull")
        hull() {
            translate([0, 0, skirt_z]) cylinder(r = root_r, h = 0.01);
            translate([0, 0, flange_z]) cylinder(r = hex_r, h = 0.01, $fn = 6);
        }
}

module hex(h) {
    intersection() {
        cylinder(d = af / cos(30), h = h, $fn = 6);
        // A cone from the inscribed circle to the corners over hex_cham.
        if (hex_cham > 0)
            union() {
                cylinder(r1 = af / 2, r2 = hex_r, h = hex_cham);
                translate([0, 0, hex_cham - 0.01]) cylinder(r = hex_r + 1, h = h);
            }
    }
}

module barbs() {
    teeth = barb_len - barb_stem;
    for (k = [0:barbs - 1])
        translate([0, 0, k * teeth / barbs - 0.01]) cylinder(d1 = 14, d2 = 12, h = teeth / barbs + 0.01);
    if (barb_stem > 0) {
        translate([0, 0, teeth - 0.01]) cylinder(d = 12, h = barb_stem - 0.6 + 0.01);
        translate([0, 0, barb_len - 0.6 - 0.01]) cylinder(d1 = 12, d2 = 10.4, h = 0.61);
    }
}

height = (order == "hex-thread-barb" ? flange + thread_len : flange_z + flange) + barb_len;

// Turned over by a rotation (not a mirror), so the thread stays right-hand.
if (flip) translate([0, 0, height]) rotate([180, 0, 0]) adapter(); else adapter();

module adapter() difference() {
    union() {
        if (order == "hex-thread-barb") {
            hex(flange + 0.01);
            translate([0, 0, flange]) thread();
            translate([0, 0, flange + thread_len]) barbs();
        } else {
            thread();
            skirt();
            translate([0, 0, flange_z - 0.01]) hex(flange + 0.02);
            translate([0, 0, flange_z + flange]) barbs();
        }
    }
    translate([0, 0, -1]) cylinder(d = channel, h = 100);
}
