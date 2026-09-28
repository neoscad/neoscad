// T3 reference for the grader's self-check (grade_selftest.py): an M24x2
// hose-barb adapter. The thread is a true helical surface: a polyhedron
// whose radius at angle a and height z is a profile of z - P*a/360.
// `rings = true` makes the stacked-ring fake the spec bans (the profile of
// z alone); `hand = -1` a left-hand thread; `pitch`, `af` for more variants.
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

difference() {
    union() {
        thread();
        translate([0, 0, thread_len - 0.01]) cylinder(d = af / cos(30), h = flange + 0.02, $fn = 6);
        for (k = [0:2])
            translate([0, 0, thread_len + flange + k * barb_len / 3 - 0.01])
                cylinder(d1 = 14, d2 = 12, h = barb_len / 3 + 0.01);
    }
    translate([0, 0, -1]) cylinder(d = channel, h = 100);
}
