# Changelog

## 0.5.0

NeoSCAD now extends OpenSCAD's language. The extensions are off by
default, so every OpenSCAD file means exactly what it means in OpenSCAD;
turn each one on with `--enable NAME` on the command line, or in the
apps' settings (macOS: Settings > Language; Linux: Preferences >
Language; Windows: Design > NeoSCAD Extensions).

### New

- **Constrained 2D sketches** (`--enable sketch`, `docs/sketch.md`).
  `sketch() { ... }` declares points, lines, arcs and circles and ties
  them with constraints (coincident, tangent, horizontal, distance,
  angle, equal, symmetric and more), as FreeCAD's Sketcher does; the
  solved profile is an ordinary 2D shape for `linear_extrude`,
  `rotate_extrude` and the rest. Fillets and chamfers at corners.
  - Under-constrained, redundant and conflicting constraints are
    reported at their source line, with fixes you can apply as edits
    ("add all" leaves a sketch fully constrained; "pin the drawing"
    writes the solved positions back).
  - The language server completes the sketch vocabulary inside sketch
    bodies, shows solved values on hover and offers the fixes; `check`
    lists each sketch, `measure --sketch NAME` reports solved lengths,
    angles and radii, and `snapshot --sketch NAME` draws the sketch
    with its constraints.
  - The solver is a separate crate, `sketch-solver`
    (github.com/neoscad/sketch-solver, MIT OR Apache-2.0), validated
    against FreeCAD's and SolveSpace's own solver tests and giving
    bit-identical results on every platform, the browser included.
- **Geometry queries** (`--enable query`, `docs/geometry-queries.md`).
  Inside a module, `child_bounds()`, `child_measure()` and
  `child_distance()` measure its children as rendered, and
  `anchor()`/`child_anchors()` pass named points up from them; a solved
  sketch's named points are anchors too. Queries never change what a
  model exports.
- **Exact STEP export** (`--enable exact`, `docs/step-export.md`).
  `-o part.step`, and STEP in the apps' and /try's export menus, writes
  true planes, cylinders, cones, spheres and tori wherever `$fn` is not
  set (an explicit `$fn` keeps OpenSCAD's polygon as flat faces), so a
  CAD program sees real holes and diameters. Whatever has no exact form
  (`hull`, `minkowski`, `polyhedron`, text, twisted extrusions) is
  written as flat facets and reported at its source line, and a model
  that can't be written correctly is refused rather than written wrong.
  Pure Rust, in the browser too.

### Changed

- `serve` advertises `sketch`, `query` and `exact`; MCP's `check` and
  `render` export STEP when the server runs with `--enable exact`, and
  its `measure` takes `sketch`.
- New resource limits `sketch_unknowns` and `queries` (both on for
  agents).
- `neoscad mcp --enable part` now turns parts on; it was ignored before.

## 0.4.3

### Fixed

- **`minkowski()` of a shape with a hole is fast.** A cube with a round
  hole plus a sphere took 127 s and 18.8 GB; it now takes 0.06 s and
  52 MB (OpenSCAD's nightly: 0.2 s). Other holed and slotted shapes are
  from 2.5 to several hundred times faster, and their volume and area
  match the nightly's; the thin slit `issue2841` exported is gone.
- **A long `minkowski()` can be stopped** by Cancel, the time limit and
  the memory limit.
- **A memory limit no longer slows renders on many cores.** With
  `--limit memory` (which the apps and every agent tool set), heavy
  models ran up to 2.8 times slower; BOSL2's `fractal_tree` now takes
  0.66 s instead of 1.86 s.
- **Windows: Disconnect ends an agent's connection at once,** instead of
  waiting for the agent to close it.
- **Deep recursion through a C-style `for`, `object()`'s arguments or a
  parameter default** reaches the full 99,999 levels in every browser
  (it stopped at 34-37 in /try), and values nested hundreds of thousands
  deep no longer overflow the stack when printed, combined with `+ - * /`
  or freed. A deeply nested list prints to the same depth in every build
  (`issue4172` prints 302 levels).

### Changed

- **The macOS app is about 7% faster:** its engine and bundled
  command-line tool are now profile-guided builds, like the
  command-line downloads.
- **Linux and Windows: a chip over the view clears an agent's marks,**
  as on macOS.
- **/try downloads half as much:** the engine is 2.3 MB instead of 4.6 MB.
  The fonts now download the first time a model draws text. /try is
  tested in Firefox and Safari's engine as well as Chrome's.
- **winget installs the desktop app** (`winget install NeoSCAD.NeoSCAD`),
  with the command-line tool in its `bin` folder.
- **Setting up the language server in other editors** (VS Code, Neovim,
  Helix, Emacs) is in `docs/lsp.md`.

## 0.4.2

### Changed

- **"Connect your AI agent" shows one client at a time.** A selector
  across the top (Claude Code, Claude Desktop, Cursor, VS Code, Other;
  Claude Desktop isn't on Linux) starts on Claude Code and remembers
  your choice. Below it is only that client's one-click setup.
- **New "Using NeoSCAD with your agent" section** in all three apps,
  worded for the chosen client:
  - keep NeoSCAD open;
  - what to ask;
  - how edits and saving work;
  - how to watch the agent work;
  - how to stay in control;
  - how to render and export.
- **Claude Desktop's agent can now export files.** The one-click setup
  gives NeoSCAD a `Documents/NeoSCAD` folder to write in, and offers
  to update an entry added by an earlier version (with a backup).
  Relative export paths land in that folder.

## 0.4.1

### Changed

- **The geometry kernel is manifold-rust 0.16.0,** which includes all of
  NeoSCAD's changes upstream (larsbrubaker/manifold-rust #5-#10). NeoSCAD
  now carries one small patch to it instead of seven.
- **`hull()` and `minkowski()` agree more closely with OpenSCAD's
  nightly,** because QuickHull now decides exactly whether a point is
  above a face. Volumes change by at most a few parts in 10^8, and the
  benchmark's convex Minkowski model renders about three times faster.
  Every other model exports the same bytes as 0.4.0.

## 0.4.0

### New

- **Connect your AI agent to the desktop apps.** Claude Code, Cursor,
  VS Code, Claude Desktop or any MCP client can now work on the document
  open in NeoSCAD:
  - it reads the code and the selection;
  - it edits, each edit one highlighted step that Undo takes back;
  - it looks at the 3D view as shown, moves the camera and marks things
    in it.

  Each app has a "Connect your AI agent" control and a setup sheet with
  one-click setup for each client. On macOS it's in the toolbar, the Help
  menu and Settings > Agents; on Linux in the header bar and Preferences;
  on Windows in the menu row and the Help menu.

  Agent access is off until you allow it. An indicator shows while an
  agent is connected, each agent can be disconnected, and "Ask before
  applying edits" makes every edit wait for your OK.
- **`neoscad mcp` finds a running NeoSCAD app** and works on its open
  document. Without an app (or with `--no-app`) it works on files as
  before. Its read roots widen to the open document's folder.
- **The apps bundle the `neoscad` command-line tool**, and the setup
  sheet writes its absolute path into each client's configuration, so no
  separate install is needed. The Flatpak shows `flatpak run` commands to
  copy instead.
- **The apps notice when the open file changes on disk.** A document
  with no unsaved edits reloads; one with unsaved edits warns, and is
  never saved over the newer file.

### Changed

- The geometry kernel's patches carry the fixes from upstream review
  (larsbrubaker/manifold-rust #6-#10): a guard for the ear clipper's
  bounding-box cull at extreme scales, linear orbit scans on high-valence
  vertices, and mesh IDs in batch booleans that don't depend on thread
  scheduling. Output is unchanged.

### Fixed

- **Windows:**
  - `neoscad mcp` no longer deadlocks on an app's pipe, because the
    client now uses overlapped I/O.
  - Agent directories with the same folder name no longer find each
    other's apps.
  - `neoscad mcp` accepts a working directory under `%TEMP%`.

## 0.3.1

### Changed

- **The geometry kernel is manifold-rust 0.15.0** (from 0.13.1), with
  NeoSCAD's changes to it in the form offered upstream
  (larsbrubaker/manifold-rust #5-#10). Results are the same to within
  rounding; a few models triangulate slightly differently. One of them
  is BOSL2's `cubetruss`, whose union no longer leaves a two-sided sheet,
  so its area now matches OpenSCAD's.
- A long boolean can be cancelled sooner, because the kernel checks for
  cancellation while it intersects edges, as Manifold's C++ library
  does.

## 0.3.0

### New

- **The apps update themselves.** The macOS app uses Sparkle (NeoSCAD >
  Check for Updates…, and Settings for automatic checks and release
  candidates). The Windows app downloads the new installer, checks it and
  installs it after one prompt. The Linux app says when a release is out
  and links to it, and its Flatpak now has network access for that
  check. The checks read a signed feed on neoscad.org, and can be turned
  off in each app's settings or with `NEOSCAD_NO_UPDATE_CHECK`.
- **`check` finds geometry that a `difference()` removes completely**
  (`cut-away`), such as standoff posts subtracted along with the cavity
  they were placed in, and subtracted shapes that cut nothing
  (`cuts-nothing`).
- **`neoscad --seed N`** makes `rands()` without a seed repeatable.
- **For AI agents (`neoscad mcp`):**
  - `check` includes the model's warnings and echo in its structured
    result, and can also export and measure sections in the same call;
  - an export is read back and confirmed;
  - printing recipes (countersink, fillet, thread, snap hook) come with
    the server's instructions, and `docs` answers them by name;
  - `test` and `format` are opt-in (`--tool test`).

### Changed

- **Recursion runs on a heap stack instead of the native one,** so it
  ends at OpenSCAD's limit of 100,000 levels in every build and
  browser, and Safari reaches the same depth as Chrome and Firefox.
  `--limit depth=N` sets the limit.
- **Deeply nested source ends in a parse error** ("memory exhausted",
  as OpenSCAD says) instead of a crash. The limit counts each kind of
  nesting by its cost, so libraries such as MCAD and BOSL2 still load
  in every browser.
- **Quieter `check`:** faces exactly at the overhang limit, short
  bridges, thread flanks and tiny slivers are reported as information,
  not warnings.

### Faster

- **Previews:** a cutter repeated across coloured parts is converted
  once, and a re-preview reuses parts that didn't change. In /try,
  threaded-ring previews in 1.9 s instead of 3.3 s in Safari, and in
  0.3 s when previewed again. "Previewed in" now counts this time, so it
  matches the wait.

### Fixed

- A deeply nested file no longer crashes the macOS app's customizer.
- Nested list literals use memory in proportion to their depth, not its
  square.
- Recursion counts toward `--limit memory`.
- A syntax error at the end of a file says "unexpected end of input".
- /try: the 3D view stays drawn while the editor pane is resized, and
  the AI agent connection explains how to allow it in Chrome and Edge.

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
