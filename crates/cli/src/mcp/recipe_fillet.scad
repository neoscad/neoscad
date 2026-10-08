// Fillets (server started with --enable fillet): rounds edges a selector string picks, on any solid.
// Selectors: "|z" vertical, ">z" top outline, "%circle and >z" top hole rims, "child(0, 1)" where children
// meet, convex, concave; and/or/not. chamfer_edges(d = 1, ...) bevels. check lists each call's edges.
module l_bracket(t = 5, w = 20)
  fillet_edges(r = 4, edges = "convex and |y and <x and <z")  // outer heel, on the result
    fillet_edges(r = 3, edges = "child(0, 1)") {              // inner corner, concave
      cube([40, w, t]);
      cube([t, w, 30]);
    }
