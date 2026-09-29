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

The other Rust crates it is built from are named in the source's
`Cargo.lock`; each is available under permissive terms (MIT, Apache 2.0,
Zlib, ISC or Unicode-3.0), as its own manifest states.

In the source tree these are copies of `assets/fonts/Liberation-2.00.1/LICENSE`,
`assets/libraries/MCAD/lgpl-2.1.txt`, `vendor/manifold-rust/LICENSE` and
`vendor/clipper2-rust/LICENSE`; `scripts/release/licenses.sh --check` keeps
them in step.
