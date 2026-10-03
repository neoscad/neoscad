// T6 reference for the grader's self-check (test_grade.py): two module
// 1.5 involute spur gears (30 and 15 teeth) and a plate with two axles
// 34 apart. -D part="plate", "large_gear" or "small_gear". Overridden for
// the wrong variants: `dist`, `axle_d`, `bore`, `thick`, `plate_t`,
// `small_z` and `fat` (each tooth's half-angle grows by this many
// degrees at the pitch circle, so the teeth bind).
part = "plate";
m = 1.5;
pa = 20;
large_z = 30;
small_z = 15;
thick = 6;
bore = 5.4;
plate = [80, 55];
plate_t = 4;
axle_d = 5;
axle_h = 10;
dist = 34;
fat = 0;

function inv(a) = tan(a) * 180 / PI - a;  // involute function, degrees

// One gear's outline: for each tooth a radial flank from the root to the
// base circle, the involute to the tip, and back down the other side.
function gear_points(z, steps = 8) = let(
    rp = m * z / 2, rb = rp * cos(pa), ra = rp + m, rf = rp - 1.25 * m,
    psi = function(r) 90 / z + fat + inv(pa) - inv(acos(rb / r)),
    r0 = max(rb, rf),
    flank = [for (i = [0:steps]) r0 + (ra - r0) * i / steps]
) [for (k = [0:z - 1]) let(c = 360 * k / z) each concat(
    [rf * [cos(c - psi(r0)), sin(c - psi(r0))]],
    [for (r = flank) r * [cos(c - psi(r)), sin(c - psi(r))]],
    [for (i = [steps:-1:0]) let(r = flank[i]) r * [cos(c + psi(r)), sin(c + psi(r))]],
    [rf * [cos(c + psi(r0)), sin(c + psi(r0))]]
)];

module gear(z) linear_extrude(thick) difference() {
    polygon(gear_points(z));
    circle(d = bore, $fn = 64);
}

module plate() {
    cube([plate.x, plate.y, plate_t]);
    for (x = [plate.x / 2 - dist / 2 - 2, plate.x / 2 + dist / 2 - 2])
        translate([x, plate.y / 2, plate_t - 0.01]) cylinder(d = axle_d, h = axle_h + 0.01, $fn = 64);
}

if (part == "plate") plate();
else if (part == "large_gear") gear(large_z);
else gear(small_z);
