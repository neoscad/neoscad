# Third-party licences

NeoSCAD is licensed under the GNU General Public License, version 2 or
later (`LICENSE`). `NOTICE` covers the code it ports (libtess2). The
`neoscad` binary also contains:

| File | Covers | Licence |
|---|---|---|
| `Liberation-Fonts-OFL-1.1.txt` | The Liberation Sans, Serif and Mono 2.00.1 fonts, embedded | SIL Open Font License 1.1 |
| `MCAD-LGPL-2.1.txt` | The MCAD OpenSCAD library, embedded | GNU LGPL 2.1 (some files allow more permissive terms, as their comments say) |
| `manifold-rust-Apache-2.0.txt` | manifold-rust, the geometry kernel (a patched copy) | Apache License 2.0 |
| `clipper2-rust-BSL-1.0.txt` | clipper2-rust, the 2D polygon clipper (a patched copy) | Boost Software License 1.0 |
| `scad-deps-MIT.txt` | scad-deps, the dependency manager (NeoSCAD's own crate, from [neoscad/scad-deps](https://github.com/neoscad/scad-deps)) | MIT |

The other Rust crates it is built from are named in the source's
`Cargo.lock`; each is available under permissive terms (MIT, Apache 2.0,
Zlib, ISC or Unicode-3.0), as its own manifest states.

In the source tree the first four are copies of `assets/fonts/Liberation-2.00.1/LICENSE`,
`assets/libraries/MCAD/lgpl-2.1.txt`, `vendor/manifold-rust/LICENSE` and
`vendor/clipper2-rust/LICENSE`; `scripts/release/licenses.sh --check` keeps
them in step. `scad-deps-MIT.txt` is the `LICENSE` of the scad-deps release
in `Cargo.lock` (the crate comes from crates.io, so there is no copy in
this tree to check it against).
