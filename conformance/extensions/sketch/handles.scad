// Entity handles as values: how they print, compare and are named in
// messages, and their sub-handles. Run with --enable sketch.
sketch(name = "handles") {
  a = point([0, 0]);
  base = line(a, [10, 0], construction = true);
  cap = arc([5, 5], base.end, [0, 10], construction = true);
  side = line(base.end, [10, 10], construction = true);
  back = line([10, 10], a, construction = true);
  fix(a); fix(base.end); horizontal(back.start, cap.end);

  echo(a, base, base.start, base.end, cap.center, cap.start);
  echo(base.start == a, base == base, base == side, cap.end == cap.start);
  echo(str("as text: ", base), [a, base], is_undef(base.x), base.middle);
  echo(is_num(a), is_list(a), is_function(a), a ? "true" : "false");
  // A handle has no arithmetic: OpenSCAD's undefined-operation warning,
  // naming its type.
  x = a + 1;
  echo(x);
}
