// Reference solution for `gears`: checks the grader, never shown to agents.
m = 2; pa = 20; thick = 6; bore = 5;

function inv(rb, t) = rb * [cos(t * 180 / PI) + t * sin(t * 180 / PI),
                            sin(t * 180 / PI) - t * cos(t * 180 / PI)];
function rot(p, a) = [p[0] * cos(a) - p[1] * sin(a), p[0] * sin(a) + p[1] * cos(a)];

module gear2d(z) {
    rp = m * z / 2; rb = rp * cos(pa); ra = rp + m; rf = rp - 1.25 * m;
    tmax = sqrt(ra * ra / (rb * rb) - 1);
    // Half-tooth angle at the base circle.
    inv_pa = tan(pa) - pa * PI / 180;
    // Less 0.4 degrees of backlash on each flank.
    half = (90 / z) + inv_pa * 180 / PI - 0.4;
    side = [for (i = [0:15]) inv(rb, tmax * i / 15)];
    difference() {
        union() {
            circle(r = rf, $fn = 4 * z);
            for (k = [0:z - 1]) rotate(k * 360 / z)
                polygon(concat(
                    [[0, 0]],
                    [for (p = side) rot([p[0], -p[1]], half)],
                    [for (i = [15:-1:0]) rot(side[i], -half)]));
        }
        circle(d = bore, $fn = 48);
    }
}
module gear_a() linear_extrude(thick) gear2d(24);
// Half a tooth pitch turned, so its teeth fall in gear_a's gaps.
module gear_b() translate([36, 0, 0]) rotate(180 + 180 / 12) linear_extrude(thick) gear2d(12);
gear_a();
gear_b();
