// 90° countersunk hole (head 9, shank 4.5) in a plate t thick, top at z = t: difference() it.
module countersink(t, d = 4.5, head = 9) {
  translate([0, 0, -1]) cylinder(d = d, h = t + 2);
  translate([0, 0, t - head / 2]) cylinder(d1 = 0, d2 = head + 0.02, h = head / 2 + 0.01);
}
// Plate with R3 vertical corners.
module rounded_plate(w, d, h, r = 3) linear_extrude(h) offset(r = r) offset(delta = -r) square([w, d]);
// R fillet along x in the inner corner of the planes y = 0 and z = 0: union it, overlapping by 0.01.
module fillet(r, l) difference() {
  cube([l, r, r]);
  translate([-1, r, r]) rotate([0, 90, 0]) cylinder(r = r, h = l + 2);
}
// Right-hand ISO metric thread, 60° flanks, a true helix: thread(24, 2, 12) is M24x2, 12 long.
// One slice per section step keeps the flanks smooth.
module thread(d, p, l, fn = 72) {
  h = 0.5413 * p;  // flank depth, 5/8 H
  function r(u) = d / 2 - h * min(1, max(0, (abs(u - 0.5) * p - p / 16) / (5 * p / 16)));
  linear_extrude(l, twist = -360 * l / p, slices = ceil(l / p * fn))
    polygon([for (i = [0:fn - 1]) r(i / fn) * [cos(360 * i / fn), sin(360 * i / fn)]]);
}
// Snap hook printed upright: arm t thick, l long, w wide; catch c deep with 45° faces (printable,
// and it releases); cut the mating slot 0.2 larger.
module snap_hook(w = 6, l = 10, t = 1.6, c = 0.8)
  rotate([90, 0, 90]) linear_extrude(w)
    polygon([[0, 0], [t, 0], [t, l - 2 * c], [t + c, l - c], [t, l], [0, l]]);
