// 90° countersink (head 9, shank 4.5), plate t thick, top at z = t: difference() it.
module countersink(t, d = 4.5, head = 9) {
  translate([0, 0, -1]) cylinder(d = d, h = t + 2);
  translate([0, 0, t - head / 2]) cylinder(d1 = 0, d2 = head + 0.02, h = head / 2 + 0.01);
}
// Plate with R3 vertical corners.
module rounded_plate(w, d, h, r = 3) linear_extrude(h) offset(r = r) offset(delta = -r) square([w, d]);
// R fillet along x in the corner of planes y = 0 and z = 0: union it, 0.01 overlap.
module fillet(r, l) difference() {
  cube([l, r, r]);
  translate([-1, r, r]) rotate([0, 90, 0]) cylinder(r = r, h = l + 2);
}
// Right-hand ISO metric helix: thread(24, 2, 12) is M24x2, 12 long.
module thread(d, p, l, fn = 72) {
  h = 0.5413 * p;  // 5/8 H
  function r(u) = d / 2 - h * min(1, max(0, (abs(u - 0.5) * p - p / 16) / (5 * p / 16)));
  linear_extrude(l, twist = -360 * l / p, slices = ceil(l / p * fn))
    polygon([for (i = [0:fn - 1]) r(i / fn) * [cos(360 * i / fn), sin(360 * i / fn)]]);
}
// Upright snap hook: catch c deep with 45° faces; cut its slot 0.2 larger.
module snap_hook(w = 6, l = 10, t = 1.6, c = 0.8)
  rotate([90, 0, 90]) linear_extrude(w)
    polygon([[0, 0], [t, 0], [t, l - 2 * c], [t + c, l - c], [t, l], [0, l]]);
