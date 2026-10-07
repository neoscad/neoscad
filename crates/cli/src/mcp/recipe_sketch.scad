// Constrained sketch (server started with --enable sketch): plate w x h, R r corners, centred hole.
// Fully constrained: check lists it so (dof 0); measure with sketch "plate" gives its solved points.
module sketch_plate(w = 40, h = 20, r = 3, hole = 6, t = 4) linear_extrude(t)
  sketch(name = "plate") {
    o = point([0, 0]); a = point([w, 0]); b = point([w, h]); c = point([0, h]);
    bottom = line(o, a); right = line(a, b); top = line(b, c); left = line(c, o);
    fix(o); horizontal(bottom); vertical(right); horizontal(top); vertical(left);
    length(bottom, w); length(left, h);
    hc = circle([w / 2, h / 2], d = hole);  // a circle inside the outline is a hole
    distance(o, hc.center, w / 2, along = "x"); distance(o, hc.center, h / 2, along = "y");
    fillet(o, r); fillet(a, r); fillet(b, r); fillet(c, r);  // cut after the solve
  }
