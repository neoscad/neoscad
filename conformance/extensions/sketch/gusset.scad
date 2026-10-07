// docs/language-extensions.md, section 6.1: an L-bracket gusset profile
// with tangent fillets, extruded. Run with --enable sketch.

// [Bracket]
leg_a  = 40;   // [20:80]
leg_b  = 30;   // [20:80]
t      = 4;    // [2:8]
gusset = 18;   // [8:30]
r      = 3;    // [1:6]
width  = 12;

linear_extrude(height = width)
sketch(name = "gusset", $fn = 48) {
  o  = point([0, 0]);
  a  = point([leg_a, 0]);
  a2 = point([leg_a, t]);
  g1 = point([t + gusset, t]);      // gusset foot on the horizontal leg
  g2 = point([t, t + gusset]);      // gusset foot on the vertical leg
  b2 = point([t, leg_b]);
  b  = point([0, leg_b]);

  bottom = line(o, a);
  end_a  = line(a, a2);
  top_a  = line(a2, g1);
  hyp    = line(g1, g2);
  in_b   = line(g2, b2);
  end_b  = line(b2, b);
  back   = line(b, o);

  fix(o);
  horizontal(bottom); length(bottom, leg_a);
  vertical(end_a);    length(end_a, t);
  horizontal(top_a);
  vertical(in_b);
  horizontal(end_b);  length(end_b, t);
  vertical(back);     length(back, leg_b);
  distance(o, g1, t + gusset, along = "x");
  distance(o, g2, t + gusset, along = "y");

  fillet(g1, r);      // tangent arcs where the gusset meets each leg
  fillet(g2, r);
}
