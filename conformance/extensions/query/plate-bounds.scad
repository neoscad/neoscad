// The design's section 6.3 as written: a base plate under its child, with
// a fixing hole 6 mm beyond the child's +x edge, centred in y, all read
// from the child's bounding box (`child_bounds()`, which renders it).
// `plate.scad` is the same model with the extent read from anchors.

// Any module producing a gear: a toothed disc, with no anchors.
module gear(teeth = 17, m = 1, thick = 5) {
  r = m * teeth / 2 + m;
  linear_extrude(thick)
    for (i = [0:teeth - 1]) rotate(i * 360 / teeth) {
      circle(r = r - 1.25 * m, $fn = 4 * teeth);
      translate([r - m, 0]) square([2 * m, m], center = true);
    }
}

module plate_for(margin = 4, thick = 3, hole_d = 5) {
  b  = child_bounds(0);                 // forces the child's geometry
  lo = b[0];
  hi = b[1];
  cy = (lo[1] + hi[1]) / 2;
  echo(lo = lo, hi = hi, hole = [hi[0] + 6, cy]);
  difference() {
    translate([lo[0] - margin, lo[1] - margin, -thick])
      cube([hi[0] - lo[0] + 2 * margin + 12, hi[1] - lo[1] + 2 * margin, thick]);
    translate([hi[0] + 6, cy, -thick - 1])
      cylinder(d = hole_d, h = thick + 2, $fn = 32);
  }
  children(0);                          // reuses the queried instance
}

plate_for() translate([10, 5, 0]) gear(teeth = 17);
