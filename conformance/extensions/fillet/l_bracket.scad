// The L-bracket of docs/fillets.md, 12.1: a concave fillet where the legs meet,
// then the outer heel of the result.
// volume: 6469.9557428756425
t = 5; w = 20;
fillet_edges(r = 4, edges = "convex and |y and <x and <z")   // outer heel
fillet_edges(r = 3, edges = "child(0, 1)")                   // inner corner
{
  cube([40, w, t]);
  cube([t, w, 30]);
}
