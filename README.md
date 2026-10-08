# NeoSCAD

NeoSCAD is a new implementation of the [OpenSCAD](https://openscad.org)
language, written in Rust. It runs your `.scad` files and libraries as
they are, renders with [Manifold](https://github.com/elalish/manifold),
and adds a surface built for people and for AI agents: structured JSON
output, `check`, `measure` and `snapshot`, an MCP server, a language
server and a long-running `serve` mode.

NeoSCAD extends OpenSCAD with three language extensions, all off by
default so that a file means exactly what it means in OpenSCAD: named
parts (`part("lid") { ... }`, `--enable part`), which `check` and
`measure` report on one by one; constrained 2D sketches
(`sketch() { ... }`, `--enable sketch`): points, lines, arcs and circles
tied by constraints and solved into a 2D shape, as in FreeCAD's
Sketcher (`docs/sketch.md`); and geometry queries (`--enable query`):
a module can read its children's bounding box, volume and area
(`child_bounds()`, `child_measure()`) or the points they name
(`anchor()`, `child_anchors()`) as ordinary values
(`docs/geometry-queries.md`). A fourth switch, `--enable exact`, exports
STEP whose faces are the model's true planes, cylinders, cones, spheres
and tori, for CAD programs such as FreeCAD, from the command line, the
apps, the MCP server and the browser (`docs/step-export.md`). A fifth,
`--enable fillet`, rounds or bevels chosen edges of any solid, boolean
results included, with CadQuery-style selectors
(`fillet_edges(r = 2, edges = "|z") cube(...)`), as FreeCAD's and
CadQuery's fillets do, and exports them as exact cylinders, tori and
cones with `--enable exact` (`docs/fillet-edges.md`).

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
  To time a release on your own machine, against your own OpenSCAD, and
  share the result, run `neoscad bench` (`docs/community-bench.md`).

## Platforms and install

- **macOS** (Apple silicon and Intel): the app and the `neoscad` command line.
- **Linux** and **Windows** (x86_64 and ARM64): the command line. Windows
  builds are unsigned; verify them with `gh attestation verify`.
- **Browser:** a WebAssembly demo at <https://neoscad.org/try>.

Releases: <https://github.com/neoscad/neoscad/releases>, with every way to
install on <https://neoscad.org/download.html>. For example
`brew install neoscad/tap/neoscad` (the command line) or
`brew install --cask neoscad/tap/neoscad-app` (the macOS app).

Each desktop app carries its own `neoscad` for AI agents to run
(`docs/mcp.md`, "Setup from the apps"): inside `NeoSCAD.app` on macOS,
as `bin\neoscad.exe` in the Windows install folder, and as
`flatpak run --command=neoscad org.neoscad.NeoSCAD` from the Flatpak.
None of them puts it on `PATH`; for the terminal, install the command
line on its own.

`neoscad lsp --stdio` is the language server for other editors; setting
it up in VS Code, Neovim, Helix and Emacs is in `docs/lsp.md`.

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

## Support

NeoSCAD is free and open source. If it's useful to you, you can support
its development at [givebutter.com/neoscad](https://givebutter.com/neoscad).
Donations go to [The Ned Workshop](https://nedworkshop.org/), a 501(c)(3)
nonprofit, and are tax-deductible in the US to the extent the law allows.
Donations pay for what shipping it costs: the Apple Developer Program for
the signed macOS app, the domain, and time on the engine and the apps.

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
