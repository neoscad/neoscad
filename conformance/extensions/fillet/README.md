# Fillet and chamfer cases

Stages F2, F3 and F5a of `docs/fillets.md`: `fillet_edges()` and
`chamfer_edges()` (`--enable fillet`) on straight edges, on circles and
arcs about an axis, and on convex and concave edges that meet (two
passes in one call), each with its volume in a `// volume:` comment:
a closed form, or for two F5a cases with none written out
(`block_plate_all`, `chamfer_bracket_all`) OCCT 8.0.1's volume of the
exported file, as their comments say. They are not in `conformance/manifest.json`, and
`conformance run` does not read them.

`crates/geom/tests/fillet_build.rs` renders every case, exports it as STEP
with `--enable exact`'s pipeline, and holds the exact volume to the closed
form (1e-9 relative, or 1e-7 where a sphere patch's area integration
dominates), the mesh volume to it within the arcs' sagitta, and the bytes
to the same values at 1, 2 and 8 threads and with a warm cache. With
`MESHBREP_OCCT_CHECK` set to the oracle (`crates/meshbrep/oracle`), every
STEP file is read back by OCCT: one valid closed solid, no free edges, and
OCCT's volume within 1e-6 of the closed form.

The closed forms use the right-angle spandrel `r²(1 − π/4)`, its first
moment about a face `r³(5/6 − π/4)` (what a mitre or a bisecting cut adds
or takes at a corner), and a sphere corner's `r³(1 − π/6)`; the others are
written out in their files. A rotational case's tool is a region revolved
about the edge's axis, so its volume is Pappus's, 2πρ̄A a turn for a
region of area `A` whose centroid is `ρ̄` from the axis: the right-angle
spandrel's centroid is `r(5/6 − π/4)/(1 − π/4)` (0.2234 r) from each of
its faces. The cone and sphere rims' regions are bounded by lines and
arcs at other angles; their `A ρ̄` was integrated on the boundary
(Green's theorem, each arc by 40-point Gauss–Legendre quadrature, which
for these smooth integrands converges to rounding), and the result agrees
with OCCT's volume of the exported file to 3e-14 or better.
