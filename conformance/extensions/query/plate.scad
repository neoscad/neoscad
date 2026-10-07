// The design's section 6.3 (a base plate under its child, with a fixing
// hole 6 mm beyond the child's +x edge, centred in y), with the child's
// extent read from anchors it declares rather than from its bounding box,
// which needs a render (stage 6).

// Any module producing a gear: a toothed disc that declares its centre
// and its outer edges as anchors.
module gear(teeth = 17, m = 1, thick = 5) {
  r = m * teeth / 2 + m;
  linear_extrude(thick)
    for (i = [0:teeth - 1]) rotate(i * 360 / teeth) {
      circle(r = r - 1.25 * m, $fn = 4 * teeth);
      translate([r - m, 0]) square([2 * m, m], center = true);
    }
  anchor("hub", [0, 0, 0], [0, 0, 1]);
  anchor("+x", [r, 0, 0], [1, 0, 0]);
  anchor("-x", [-r, 0, 0], [-1, 0, 0]);
  anchor("+y", [0, r, 0], [0, 1, 0]);
  anchor("-y", [0, -r, 0], [0, -1, 0]);
}

module plate_for(margin = 4, thick = 3, hole_d = 5) {
  a = child_anchors(0);
  lo = [a["-x"][0][0], a["-y"][0][1]];
  hi = [a["+x"][0][0], a["+y"][0][1]];
  cy = a.hub[0][1];
  echo(lo = lo, hi = hi, hole = [hi[0] + 6, cy]);
  difference() {
    translate([lo[0] - margin, lo[1] - margin, -thick])
      cube([hi[0] - lo[0] + 2 * margin + 12, hi[1] - lo[1] + 2 * margin, thick]);
    translate([hi[0] + 6, cy, -thick - 1])
      cylinder(d = hole_d, h = thick + 2, $fn = 32);
  }
  children(0);  // reuses the queried instance
}

plate_for() translate([10, 5, 0]) gear(teeth = 17);
