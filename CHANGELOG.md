# Changelog

## 0.2.0

### New

- **NeoSCAD for Windows, preview.** A WinUI 3 app with the same editor,
  live preview, 3D view, console, Examples menu and STL/PNG export as the
  macOS app. It installs from an MSI, `NeoSCAD-0.2.0-windows-x64.msi` or
  `-arm64.msi`, which adds a Start-menu entry and "Open with" for `.scad`
  files. It is unsigned, like the command-line tool: Windows warns about
  an unknown publisher (docs/release.md, "Windows is unsigned").
- **NeoSCAD for Linux, preview.** A GTK 4 and libadwaita app with the
  editor, live preview, 3D view, console, examples, export, and the
  language server (markers, completion, go to definition into MCAD and
  BOSL2). It ships as a Flatpak bundle,
  `NeoSCAD-0.2.0-linux-x86_64.flatpak` or `-aarch64.flatpak`:
  `flatpak install --user <file>`.
- **Community benchmarks.** `neoscad bench --submit` times this release
  on your machine, alongside OpenSCAD if it is installed, and offers to
  submit the result to
  [neoscad/benchmarks](https://github.com/neoscad/benchmarks). Results
  appear on [neoscad.org/community.html](https://neoscad.org/community.html).
  Each release also benchmarks itself on GitHub's runners as a baseline.
- **Profile-guided builds.** The `neoscad` binaries for macOS arm64, Linux
  x86_64 and aarch64, and Windows x64 are built with PGO. Before a PGO
  binary ships, the release checks its recursion depth against OpenSCAD's.

### Fixed

- A preview of a very large CSG tree no longer overflows the stack. For
  example, the Menger sponge example at depth 5 crashed the web demo's
  engine. Past 10,000 elements to combine, a preview draws the tree
  thrown together, with a warning to render instead.
- The native stack budget is 64 MiB, up from 48 MiB, so deep recursion
  goes at least 1.4× deeper than OpenSCAD's in every build.
- On Linux's software renderer (lavapipe), axis lines seen end-on no
  longer draw striped marks in PNG export and the viewport.
- Embedded examples and libraries keep their exact bytes when a Windows
  checkout converts line endings.
- The web demo's worker says why its engine stopped: out of memory, or a
  stack overflow.

### Changed

- The macOS app and the new apps share their document logic in the Rust
  core (`crates/client`): runs, the console, the customizer, examples and
  exports.
- The Linux Flatpak is not on Flathub (docs/linux-app.md, "Flathub: not
  submitted"). Install it from the release.

## 0.1.1

Performance: parallel geometry kernels, a call-level memo, faster 2D
unions and PNG encoding. Extruded text and the Menger sponge are much
faster.

## 0.1.0

The first release: the `neoscad` command line on macOS, Linux and
Windows, the macOS app, and the web demo at neoscad.org/try.
