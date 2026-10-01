# Changelog

## 0.2.1

### Fixed

- **Safari:** the /try BOSL2 examples (gear, gearbox) render in WebKit.
  The renderer's tree walk no longer uses one native stack frame per
  level.
- **Deep recursion never crashes the web engine.** Each browser's stack
  is measured at start-up, and too-deep recursion stops with OpenSCAD's
  recursion error. Fewer wasm frames per level give more depth in every
  browser.
- **Preview stops on Cancel and at its time and memory limits,** including
  inside a single huge boolean. In the web demo, the Menger sponge at depth
  5 gives a memory-limit error instead of crashing the engine.
- **Measured memory limits:** the command line, `serve`, `mcp`, the macOS
  app and the web measure memory instead of only estimating it, and shrink
  their caches before failing a request.
- **A call memo bug** replayed the wrong `echo` for calls whose children
  only assign values.
- **Check** reports a sealed internal void as a cavity, not a floating
  piece, and pluralises its summary ("1 error").
- **`use <font.ttf>`** no longer parses the font as a library, and a
  missing font prints OpenSCAD's error.
- **Windows:** an installed rc and its release now upgrade in place.

### Faster

- **A preview reuses repeated subtrees, as a render does:** the Menger
  sponge at depth 4 previews in 1.4 s instead of 3.8 s natively, and in
  8.5 s instead of 34 s on the web. A preview of a big repeated tree can
  use about as much memory as its render.

### New

- **Windows and Linux apps:** Customizer, Check and Measure panels, file
  watching, and every export format with progress and Cancel.
- **The render summary** reports the geometry cache size, as OpenSCAD does.
- **The groundwork for update checks:** a signed release feed, and a
  once-a-day notice from `neoscad` in an interactive terminal (turn it off
  with `NEOSCAD_NO_UPDATE_CHECK`; see docs/privacy.md). The feed goes live
  once its signing key is set up.
- **macOS releases no longer wait on Apple's notarization.** A release
  stays a prerelease until its notarized DMG is attached by an hourly job.

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
