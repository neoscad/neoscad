// docs/language-extensions.md, section 6.2: a slot with explicit tangent
// arcs, cut from a plate. Run with --enable sketch.
slot_len = 30;
slot_w   = 8;

linear_extrude(3)
difference() {
  square([50, 20], center = true);
  translate([-slot_len / 2, 0])
  sketch(name = "slot") {
    c1   = point([0, 0]);
    c2   = point([slot_len, 0]);
    axis = line(c1, c2, construction = true);
    top  = line([0, slot_w / 2], [slot_len, slot_w / 2]);
    bot  = line([slot_len, -slot_w / 2], [0, -slot_w / 2]);
    e1   = arc(c1, top.start, bot.end);   // left cap, counter-clockwise through 180°
    e2   = arc(c2, bot.start, top.end);   // right cap, counter-clockwise through 0°

    fix(c1);
    horizontal(axis); length(axis, slot_len);
    tangent(e1, top); tangent(e1, bot);
    tangent(e2, top); tangent(e2, bot);
    diameter(e1, slot_w); equal(e1, e2);

    // Handles print with their variable names, and a sub-handle is the
    // entity it names.
    echo(top, top.start, e1.center == c1, e2.start == bot.start);
  }
}
