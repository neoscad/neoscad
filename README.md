# NeoSCAD

NeoSCAD is a new implementation of the [OpenSCAD](https://openscad.org)
language, written in Rust. It runs your `.scad` files and libraries as
they are, renders with [Manifold](https://github.com/elalish/manifold),
and adds a surface built for people and for AI agents: structured JSON
output, `check`, `measure` and `snapshot`, an MCP server, a language
server and a long-running `serve` mode.

- Website: <https://neoscad.org>
- Try it in the browser: <https://neoscad.org/try>

## Status

NeoSCAD is pre-release. As of the final v0.1 audit
(`docs/audits/final.md`):

- **Conformance:** it passes all 1,773 of OpenSCAD's in-scope regression
  tests (`conformance run`; the ids are in `conformance/baseline.json`).
  Out of scope are the CGAL-only, experimental, known-bug and
  upstream-disabled cases.
- **Speed:** about 2.8× faster than the OpenSCAD nightly (Manifold
  backend) on heavy models. The method and the numbers are in
  `docs/audits/final.md` and on <https://neoscad.org/benchmarks.html>.

## Platforms and install

- **macOS** (Apple silicon): the app and the `neoscad` command line.
- **Linux** and **Windows** (x86_64 and ARM64): the command line.
- **Browser:** a WebAssembly demo at <https://neoscad.org/try>.

Release builds are coming soon; see <https://neoscad.org/download.html>
for their status. Until then, build from source.

## Build from source

You need a Rust toolchain (`rust-toolchain.toml` pins it). The
conformance suite also needs OpenSCAD's test data, from a checkout at
`.reference/openscad`:

    git clone --depth 1 https://github.com/openscad/openscad.git .reference/openscad
    git -C .reference/openscad submodule update --init --depth 1 libraries/MCAD

Then:

    cargo build --release
    cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
    ./target/release/conformance run [--tier N] [--filter S] [-v]
    ./target/release/neoscad --help

The macOS app:

    scripts/apple/build-core.sh                    # Rust core -> XCFramework + Swift bindings
    xcodegen generate --spec apple/project.yml     # apple/NeoSCAD.xcodeproj
    xcodebuild -project apple/NeoSCAD.xcodeproj -scheme NeoSCAD -derivedDataPath apple/build/DerivedData build

The web demo is in `web/` (see `web/README.md`); `scripts/wasm-check.sh`
builds the library crates for wasm32 and runs them in Node.

`CLAUDE.md` lists every build, test and benchmark command, and
`docs/architecture.md` describes the design.

## Licence

NeoSCAD is licensed under the GNU General Public License, version 2 or
later (`GPL-2.0-or-later`; see `LICENSE`). Third-party notices are in
`NOTICE`.

Some dependencies, such as `manifold-rust` (vendored in `vendor/`), are
licensed under Apache-2.0 only. The Free Software Foundation considers
Apache-2.0 compatible with GPLv3 but not with GPLv2, so distributed
NeoSCAD binaries, which combine the two, are effectively under GPLv3 (or
later; see the FSF's
[licence list](https://www.gnu.org/licenses/license-list.html#apache2)).
The source files themselves remain GPL-2.0-or-later.

## Credits

NeoSCAD would not exist without [OpenSCAD](https://openscad.org): its
language, its behaviour and its regression tests are the specification
NeoSCAD is built against. Geometry is computed with
[Manifold](https://github.com/elalish/manifold), through a Rust port.
Bundled fonts and libraries are credited in `assets/README.md`.

Built by The Ned Workshop (Matt Robinson); built with Claude.
