# Exact STEP export cases

The 28 models of `docs/audits/exact-geometry-rust.md` (its 15 boolean
cases, the idioms x01–x11 and the CSG fillets f01–f02), and stage 2's
extrusions e01–e10 (`linear_extrude`, `rotate_extrude`, `offset(r)`), as
OpenSCAD sources, for NeoSCAD's `--enable exact` STEP export. They are not in
`conformance/manifest.json`, and `conformance run` does not read them.

Each file's `// volume:` comment is its closed-form volume (none for
x07, whose faceted sphere has no simple one). `conformance exact`
exports every case at four `$fa`/`$fs` settings and holds the exact
volume to it within 1e-6 (the audit's gate 3);
`crates/geom/tests/exact.rs` does the same at the defaults, checks the
STEP bytes of four of them against pinned hashes, and checks that the
bytes are the same at 1, 2 and 8 threads and with a warm cache.

x07 and x08 use `hull()` to stand in for the audit's faceted solids (a
mesh-only construct, written as planar facets beside exact faces).
