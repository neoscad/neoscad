// An error in the queried child stops evaluation at the query, as it
// would at children(0): what the child printed first is printed with it.

module q() {
  a = child_anchors(0);
  echo("not reached", a);
  children(0);
}
module failing() {
  echo("in the child");
  assert(false, "the child fails");
  cube(1);
}
cube(2);
q() failing();
echo("not reached either");
