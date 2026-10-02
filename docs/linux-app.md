# The Linux desktop app

`crates/linux-app` (package `neoscad-linux-app`, binary `neoscad-gtk`) is
the Linux counterpart of the macOS app (`apple/`): GTK 4 and libadwaita
around the same Rust core, following the GNOME HIG. It is Rust through
and through, so it calls `crates/client`, `session` and `render` directly;
there is no UniFFI layer as there is for Swift (`crates/ffi`).

This page covers milestone 1 and milestone 2 so far (the language
server, the Flatpak, the side panels, file watching and the rest of File
> Export): what the app does, how it is put together, how to build,
package and run it, and what comes next.

## What milestone 1 does

- **Window**: an `AdwApplicationWindow` with an `AdwToolbarView`; the
  header bar has Open and New Window at the start, an `AdwWindowTitle`
  (file name, `• ` when edited, and the folder or "Unsaved"), Preview,
  Render and the main menu at the end. The editor is on the left, the 3D
  view over the console on the right, in `GtkPaned`s. Messages are
  `AdwToast`s. One window per document: New Window opens an empty one;
  Open and Examples reuse the active window while it holds an untouched
  untitled document (as GNOME Text Editor reuses an empty tab).
- **Editor**: the macOS app's CodeMirror bundle (`apple/Editor/web`, built
  by `scripts/apple/build-editor.sh`), unchanged, in a WebKitGTK 6 web view.
- **Live preview**: each pause in typing previews the document once
  (`client::DocumentLoop`, 150 ms, `client::DEFAULT_PREVIEW_DELAY_MS`);
  F5 previews and F6 renders now.
- **3D view**: the wgpu viewport (`render::viewport`), with OpenSCAD's
  mouse: left drag orbits, right drag (or Shift+left) pans, middle drag
  and the wheel zoom, and a touchpad pinch zooms. View All and Reset View
  are in the menu. A file's `$vpt`, `$vpr`, `$vpd` and `$vpf` move the
  camera as in OpenSCAD's GUI, only when they change.
- **Console**: the run's summary line (`client::describe_render`) over its
  lines, coloured by filter group (`client::console_group`); a line that
  points into the document selects that span in the editor.
- **Files**: New Window, Open, Save, Save As (`GtkFileDialog`, so the
  portal's chooser inside a sandbox), File > Examples from
  `client::examples()` (untitled documents named after the example; a
  heavy one waits for Preview and says why), recent files through
  `GtkRecentManager`, and GNOME's "Save changes?" `AdwAlertDialog` on
  closing an edited window. Files must be UTF-8, as OpenSCAD reads them;
  saves go through a temporary file renamed over the target.
- **Export**: binary STL (detached from the document's runs, so typing
  does not cancel it, with the core's atomic write) and a PNG of the view
  (without the grid, as the macOS app's). Titles and extensions come from
  `client::export_formats()`. Milestone 2 adds the other formats.
- **Style**: follows the system's light or dark preference through
  `AdwStyleManager`, or forced from the menu's Style submenu. The editor
  follows through `prefers-color-scheme`, and the view switches between
  the macOS app's scheme pair (Cornfield, Tomorrow Night) and builds the
  model again in the new colours.
- **Shortcuts**: a `GtkShortcutController` on the window, in the capture
  phase so the web view does not take them first: Ctrl+N, Ctrl+O, Ctrl+S,
  Ctrl+Shift+S, Ctrl+Shift+E (Export STL; Export Again since milestone
  2), Ctrl+W, Ctrl+Q, F5, F6, Ctrl+Shift+V (View All), and since
  milestone 2 F9 and Alt+1 to Alt+3 (the side panels; CodeMirror binds no
  Alt+digit). Editing keys are left to CodeMirror.

## What milestone 2 adds

- **Language features**: the editor's `@codemirror/lsp-client` talks to
  the core's language server (`crates/lsp`) in process, one server per
  window: completion, hover, signature help, rename, references,
  formatting, code actions, and markers for the document's errors and
  warnings, which come from the window's own runs.
- **Go to definition** (F12; the page's other binding is Command+click,
  made for macOS, and a Ctrl+click binding is a follow-up): a
  definition in the same file moves the cursor; one in a file of the
  user's own opens that file in a window (or brings its window forward)
  at the definition; one in a library (MCAD, BOSL2, anything under a
  library directory or not writable) opens read-only in a library viewer,
  which has its own language server, so hover and go to definition work
  inside library code too.
- **Flatpak**: a manifest on the GNOME 51 runtime, with the desktop file,
  AppStream metadata, icons and the `.scad` file type ("Flatpak" below).
- **Side panels**: an `AdwOverlaySplitView` at the window's end, shown
  and hidden by the header bar's sidebar button or F9, over a stack of
  three panels; Alt+1, Alt+2 and Alt+3 open one and put the keyboard in
  it. The panels ask the window for what they need and are shown its
  state; the logic is in the library (`customizer.rs`, `inspect.rs`).
  - **Customizer**: one `AdwPreferencesGroup` per customizer group, a
    row per parameter with OpenSCAD's control for it (a switch row, a
    slider with a number field, a spin row, an entry row, a field per
    vector element, a combo row). As in the macOS app, an edit never
    changes the text: the value goes through `client::edit_parameter`
    (snapped to a slider's step, clamped, cut to length, dropped when it
    equals the text's) into the `DocumentLoop`, and the document runs
    again with the edited values as `-D`-style assignments. Each edited
    row has a button back to the text's value; Reset drops them all.
    Parameter sets are OpenSCAD's JSON file beside a saved model: the
    dropdown applies one as `-p file -P name` does, Save writes the
    current values as a set (an `AdwAlertDialog` asks its name).
  - **Check**: `client::check` on the text as it is, with the customizer's
    values, for a printer preset or check's defaults. The summary line
    (`client::check_summary`) over the findings, each with its severity
    icon, message, code and fix. Activating a finding marks it in the
    view (its numbered ring and box, `client::view_overlay`) and turns
    the view to it; activating it again clears the mark.
  - **Measure**: `client::measure` (volume, area, size), then Pick Points:
    clicks in the view (a click, not a drag: GTK denies the click gesture
    once the pointer moves) pick points on the solid
    (`Measurement::pick` along `Viewport::ray_at`), and two give their
    distance, drawn in the view.
  Check and measure run detached from the document loop: typing does not
  cancel them, and a newer one of the same kind cancels the older.
- **File watching**: the files the last run read that another program
  could change (`Client::run_files`: includes, used files, imports and
  fonts on disk; not the document, and not the bundled MCAD) are watched
  through a `GFileMonitor` on each of their directories, as the macOS
  app watches with FSEvents: editors save by renaming a new file over
  the old one, which a monitor of the file itself can lose. A burst of
  events (100 ms) becomes one `DocumentLoop::files_changed`: a preview
  after the pause, or a render at once if the last run was a render.
- **Export**: File > Export lists every entry of `client::export_formats()`
  (binary and ASCII STL, 3MF, OBJ, OFF, SVG, DXF, PDF, a PNG of the view,
  the snapshot sheet); Export Again (Ctrl+Shift+E) uses the last format,
  or SVG for a 2D model and STL for a 3D one
  (`client::suggest_export_format`). The save dialog starts beside the
  model. An export runs detached, with the customizer's values, under a
  toast that names its stage (`session::Progress`) and has a Cancel
  button; success is a toast, a failure an alert with the core's reason
  (`client::export_failure_reason`), never silence.

## How it is put together

The crate is a library plus a binary. The library (`src/*.rs`) is the host
logic that is not GTK glue, and builds and is tested on every platform:

| Module | What |
|---|---|
| `bridge.rs` | The editor protocol's state machine: message parsing, versions, applying changes to the host's copy (`client::EditorText`), resyncs, the scripts sent to the page |
| `resources.rs` | The `neoscad-editor:` scheme: which files are served, the per-load CSP nonce, where the bundle is looked for |
| `host.rs` | The session configuration (disk plus MCAD in memory, library path, fonts, clock, seed, limits) and the GPU |
| `language.rs` | The editor's language server on a worker thread, where a definition opens (document or library), messages described for the log |
| `document.rs` | A window's document: path, text, edited state (undo-aware), titles |
| `run.rs` | A document run (with the files it read) and the exports (every format, the snapshot sheet, progress and cancel), off the main thread |
| `view.rs` | The viewport drawing into a texture, device pixel sizes, drag mapping |
| `customizer.rs` | The customizer's values, edits, field ranges and parameter sets on disk |
| `inspect.rs` | Check and measure requests, the overlay as the viewport's annotations, picked points, the panels' text |
| `watch.rs` | Which files a window watches, by directory |
| `update.rs` | The update check's settings, when a check is due, the feed's address, and the notice for each install type ("Updates") |

The window (`src/app/`) is compiled only with the `gtk` feature;
`app/library.rs` is the read-only library viewer, `app/customizer.rs` and
`app/inspect.rs` the side panels' widgets, `app/update.rs` the update
check's fetch, banner and preferences.

### The editor bridge

The protocol is the macOS app's, documented at the top of
`apple/App/Editor/EditorController.swift`; the page's side is
`apple/Editor/web/src/bridge.js`. The page posts to
`window.webkit.messageHandlers.editor`, which WebKitGTK provides as
WKWebView does; the handler is registered "with reply" because the page
treats `postMessage` as returning a promise. The host calls
`window.NeoSCADEditor.*` with `call_async_javascript_function`, its
arguments spliced as JSON literals (`bridge::call_script`). The page is
served under `neoscad-editor://app/` with a Content-Security-Policy header
and a fresh style nonce per load, as `EditorSchemeHandler.swift` serves
it. Navigation away from that scheme is refused, and if the web process
ends the page is reloaded and shows the document's text again.

### The language server

The contract is the macOS app's (`apple/App/Editor/LanguageClient.swift`)
and the Windows app's (`windows/NeoSCAD.Host/LanguageBridge.cs`); the
Linux app calls `crates/lsp` directly rather than through `crates/ffi`.

- **One server per editor page.** Each window, and each library viewer,
  has a `linux_app::language::Language`: an `lsp::Server` over the
  process's one session, sharing one `lsp::Cache` of analysed library
  files, so BOSL2 is indexed once per process. When the web process ends
  and the page reloads, the window starts a new server, since the new
  page's client sends `initialize` again.
- **In order, off the main thread.** The page's `lsp` messages go to the
  server's worker thread (with the evaluator's stack, since parsing
  recurses) through a channel, and are handled one at a time. Answers go
  to a sink that feeds a `futures-channel` queue; one main-loop future
  reads it and calls `NeoSCADEditor.lspReceive`, so answers reach the
  page in the order the server gave them.
- **Before `ready`.** Answers are delivered whether or not the page has
  said `ready`: the page's client sends `initialize` first, and dropping
  that answer makes its first request time out (the Windows app's bug
  until `EditorHost` stopped gating it).
- **Markers from the document's runs.** The server is made with
  `host_diagnostics`, so it never evaluates. `run::run_document` hands
  it each run's diagnostics with the exact text the run read
  (`lsp::Server::supply`): the evaluation's before the geometry stage,
  and the finished run's again only if the geometry stage added to them
  (as `crates/ffi/src/document.rs` does). Both go through the same sink,
  so they stay in order. The markers are published for the client's
  version with that text; if the client has not sent it yet, `handle`
  publishes them when it does. Before each run the window calls
  `NeoSCADEditor.lspSync()` so the client sends that version now rather
  than after its own half-second pause. A `host_diagnostics` server never
  has diagnostics pending, so unlike the other apps there is no debounce
  timer.
- **URIs.** A document's URI is its core path as a `file://` URI
  (`lsp::uri::from_path`), passed with `load`; Save As under a new name
  calls `setURI`, so the page's client closes the old URI and opens the
  new one.
- **Where a definition opens** (`language::target`, the macOS app's rule
  from `LibraryViewer.swift`): a writable file on disk outside the
  library directories opens as a document; anything else opens read-only
  in a library viewer, its text read through the core (the bundled MCAD
  is only in memory). There is one viewer per file. A position waits
  until the page has loaded the text.

With `G_MESSAGES_DEBUG=neoscad`, each message is logged in a line
(`lsp: page -> server textDocument/hover #3`, `lsp: server -> page reply
#1: capabilities …`). After each diagnostics publication the window also
logs the markers the page itself counts (`editor: 2 markers, language
server connected`, from `NeoSCADEditor.state()`), which `linux/smoke.sh`
checks.

### The viewport

The wgpu viewport draws into a texture of the widget's size in device
pixels (`Viewport::attach_texture`); each changed frame is read back and
handed to GTK as a `GdkMemoryTexture`, shown by a `GtkPicture` laid over a
`GtkDrawingArea` that sets the size and takes the input. A tick callback
on the frame clock (GTK's display link) draws only when the viewport
changed, so an idle view costs a flag check a frame.

Why not draw into a window surface, as the macOS app draws into its
`CAMetalLayer`:

- GTK 4 has no native child windows. A wgpu surface needs a native window
  (`wl_surface`, X11 window); the only ones a GTK 4 widget can reach are
  the toplevel's, which GTK draws over, so the view would have to be a
  separate subsurface (Wayland) or child window (X11) that GTK neither
  stacks nor clips, positioned by hand and different on each display
  server.
- `GtkGLArea` with wgpu's GL backend would share GTK's GL context, which
  ties the view to GL (no Vulkan) and to GTK's GL renderer (GTK may
  render with Vulkan or, in a VM, Cairo), and needs `unsafe` to wrap a
  foreign framebuffer.

A texture works the same under Wayland, X11, Xvfb and any GSK renderer,
lets wgpu choose Vulkan first and GL second (`host::gpu`, the command
line's order), and needs no `unsafe`. The cost is one GPU-to-CPU copy per
changed frame. Milestone 2 removes it where the driver allows:
`GdkDmabufTexture` (GTK 4.14) takes a GPU buffer without a copy.

Without any GPU the view says so and the rest of the window works.

### The document loop

Each window keeps one `client::DocumentLoop` and one GLib timer. Typing
applies the change to the document's copy and the core's buffer
(`Client::edit`, in UTF-8 offsets from `lang::source`'s conversion), then
schedules a preview; the timer fires at the loop's due time; the run goes
to a worker thread (`gio::spawn_blocking`) and its result is applied only
if the loop says it is still current. The GPU upload of the scene happens
on the worker; the main thread only swaps the model in.

## Building

The app needs GTK 4.14, libadwaita 1.5 and WebKitGTK 6.0 development
files (Ubuntu 24.04, Fedora 40 or later):

    sudo apt install libgtk-4-dev libadwaita-1-dev libwebkitgtk-6.0-dev
    scripts/apple/build-editor.sh           # the editor bundle (node 18+)
    cargo build -p neoscad-linux-app --features gtk
    target/debug/neoscad-gtk [FILE.scad]

The `gtk` feature is off by default, so `cargo build`, `cargo clippy
--all-targets` and `cargo test` of the workspace build only the library
and need none of those packages; that keeps the macOS and Windows builds
and the existing Linux CI jobs unchanged.

The app finds the editor bundle in `$NEOSCAD_EDITOR_DIR`, else
`../share/neoscad/editor` or `editor` beside the binary (the Flatpak's
`/app/share/neoscad/editor`), else the checkout's `apple/Editor/web/dist`
when run from `target/`. The fonts, MCAD and colour schemes are compiled
into the binary (`crates/assets`), so nothing else needs installing.
`NEOSCAD_EDITOR_INSPECT=1` turns on the web inspector and copies the
page's console to stdout. `G_MESSAGES_DEBUG=neoscad` logs each run and
the bridge's messages.

### From macOS, in Docker

`linux/Dockerfile` is an Ubuntu 24.04 image with the packages above,
Mesa's software Vulkan (lavapipe) and GL (llvmpipe), Xvfb and the pinned
toolchain. From the repository root:

    docker build -t neoscad-linux-dev linux
    docker run --rm --memory=8g -v "$PWD":/src \
        -v neoscad-linux-target:/target -e CARGO_TARGET_DIR=/target \
        neoscad-linux-dev cargo build -p neoscad-linux-app --features gtk
    docker run --rm --memory=6g -v "$PWD":/src:ro -v neoscad-linux-target:/target \
        -e TYPE=1 -e SHOTS=/src/target/linux-shots \
        neoscad-linux-dev linux/smoke.sh /target/debug/neoscad-gtk

(For `SHOTS`, mount the checkout read-write or point it at another
mount.) Remove the image and the volume afterwards with `docker rmi
neoscad-linux-dev` and `docker volume rm neoscad-linux-target`.

### Tests

- `cargo test -p neoscad-linux-app` (any platform): the bridge, the
  resources, titles and edited state, runs and exports without a GPU
  (every geometry format, a 2D model to a 3D format saying why, a
  cancelled export leaving the old file, the stages reported), the
  language server (real capabilities, answers in order, nothing after
  stop, a run's diagnostics published as markers for the client's
  version), where a definition opens, the customizer (each control,
  edits snapped, clamped, cut and dropped, sets saved and applied), check
  (a thin plate's error, its marker and box) and measure (two picks ten
  millimetres apart), the files a run reads and watches, and the update
  check against the signed fixtures in `crates/client/testdata/update`
  (the defaults, a day between checks, a Flatpak offered its
  architecture's bundle and a source build the release page, a bad or
  edited signature and a replayed serial refused, rc only when opted
  in, Later).
- `linux/smoke.sh BIN [MODEL]` (Linux, Xvfb): with the editor bundle, it
  first opens a model with a warning and waits for `initialize`'s real
  capabilities and for the page to count its markers, and with `TYPE=1`
  presses F12 on an MCAD module and waits for the library viewer. Then
  it opens MODEL and waits for its preview; with `TYPE=1` it types into
  the editor and waits for the second preview; with `SHOTS=DIR` it saves
  light and dark screenshots. With `TYPE=1` it also opens a model with
  customizer parameters, a known check error and a used file, and from
  the keyboard: Alt+1 and Space turn a switch on (a new run, the file
  unchanged), Alt+2 and Enter check (an error found) and Enter marks the
  first finding (the overlay's box), Alt+3 and Enter measure,
  Ctrl+Shift+E and Enter export an STL beside the model; then it renames
  a new version of the used file over it and waits for the run that
  follows. With `SHOTS` that runs in both styles and saves
  `customizer-`, `check-` and `measure-light.png` and `-dark.png`. It
  kills the app above 2 GB of memory.
- CI's `linux-app` job (ubuntu-24.04) runs clippy and the tests with the
  `gtk` feature, builds the editor bundle and runs the smoke test with
  typing.

## Updates

The app tells the user about a newer release (owner decisions of
2026-09-30, `docs/audits/auto-update.md`): the shared signed feed,
checked by `client::update::check`, the code the CLI uses.

- **When.** Automatically, ten seconds after start-up and then on an
  hourly tick, each time only if a day has passed since the last check
  (`linux_app::update::Settings::due`). Main menu > Check for Updates
  checks at once and says what it found, a failure included; the
  automatic check is silent and only logs (`G_MESSAGES_DEBUG=neoscad`,
  lines starting `update:`). A build with no trusted key (`RELEASE_KEYS` in
  `crates/client/src/update.rs` empty, as before 0.2.2) makes no request
  at all.
- **What it fetches.** `https://neoscad.org/updates/v1/stable.json`, or
  `rc.json` with "Receive release candidates" on, and its `.minisig`:
  plain GETs through libsoup (already linked by WebKitGTK), no cookies,
  User-Agent `neoscad` (`docs/privacy.md`). Each is read up to 64 KiB.
  The feed's signature, channel, serial (kept per channel, so an old
  feed can't be replayed) and version are checked before anything is
  shown.
- **What it shows.** An `AdwBanner` under the header bar of every
  window, "NeoSCAD x.y.z is available", whose Details button opens a
  dialog that depends on how the app was installed
  (`linux_app::update::Install`, from `/.flatpak-info`):
  - *Flatpak* (the only way the app is released): the release bundles
    carry no repository, so `flatpak update` never sees the next
    release. Download Bundle opens the new release's bundle for this
    architecture (through the OpenURI portal, so the browser downloads
    it and Software opens it), and the text gives
    `flatpak install --user --reinstall NeoSCAD-x.y.z-linux-<arch>.flatpak`.
    A release whose bundle for this architecture is missing (aarch64's
    build may fail) is not offered until it is attached.
  - *Anything else* (a source build): Open Release Page, and the advice
    to update it the way it was installed. The `.deb`, `.rpm` and
    tarballs carry only the command line, which has its own notice.

  Later hides the banner and keeps automatic checks from showing that
  version again; a newer one, or the menu's check, shows it.
- **Preferences** (main menu, Ctrl+,): "Check for updates
  automatically" (on by default) and "Receive release candidates" (off).
  Switching the channel checks the other feed at once. They are stored
  with the serials in `updates.json` under the user's configuration
  directory (`~/.config/neoscad/`, or
  `~/.var/app/org.neoscad.NeoSCAD/config/neoscad/` in the Flatpak).
- **Off switches.** `NEOSCAD_NO_UPDATE_CHECK` set (as for the CLI), or
  `CI` set, stops the automatic check for that run; CI's smoke test
  never checks.
- **Testing against a local feed.** `NEOSCAD_UPDATE_FEED_URL` names
  another feed directory: https, or plain http to the loopback address.
  The signature is still checked against the keys compiled in, so a test
  builds the app with `NEOSCAD_UPDATE_TEST_PUBLIC_KEY` set to a
  throwaway key's public line (read at compile time, never at run time),
  as `scripts/release/test-update-feed.sh` does for the CLI.

Once the Flatpak repository the audit recommends exists (owner decision:
its own GitHub Pages site), installs from it are updated by `flatpak
update` and GNOME Software, and the app's notice for them should say so
instead of offering a bundle (`docs/followups.md`, "Linux").

## Flatpak

| File | What |
|---|---|
| `linux/flatpak/org.neoscad.NeoSCAD.yml` | The manifest: GNOME 51 runtime, the rust-stable and node24 SDK extensions, one module |
| `linux/flatpak/generate-sources.sh` | Writes `cargo-sources.json` and `node-sources.json` from the lock files (not committed) |
| `linux/data/org.neoscad.NeoSCAD.desktop` | The desktop file (`MimeType=application/x-openscad;`) |
| `linux/data/org.neoscad.NeoSCAD.metainfo.xml` | AppStream metadata |
| `linux/data/org.neoscad.NeoSCAD.mime.xml` | The `.scad` type for shared-mime-info |
| `linux/data/icons/hicolor/*/apps/org.neoscad.NeoSCAD.png` | 32 to 512 px, from `apple/App/AppIcon.icon/Assets/art.png` by `linux/data/icons/generate.sh` |
| `linux/data/screenshots/window.png` | The metainfo's screenshot (a `linux/smoke.sh` `SHOTS` capture, trimmed) |
| `.github/workflows/flatpak.yml` | CI: generate the sources, build a bundle per architecture, upload it; in a release, attest the bundles and attach them |

Choices:

- **Runtime.** `org.gnome.Platform` 51, the current stable one (what
  Flathub's GNOME apps, such as org.gnome.TextEditor, build on). It
  carries GTK 4, libadwaita and WebKitGTK 6, so the manifest has no
  library modules.
- **Offline cargo.** `flatpak-cargo-generator.py` from
  flatpak-builder-tools (pinned to a commit in `generate-sources.sh`)
  turns `Cargo.lock` into one archive source per crate plus a
  `cargo/config` that replaces crates.io with them; the build sets
  `CARGO_HOME` to that directory and runs `cargo --offline`. The patched
  crates under `vendor/` are path dependencies of the checkout.
- **Offline npm: generated sources, not a prebuilt bundle.** The editor
  bundle is built in the Flatpak from `apple/Editor/web`, with
  `flatpak-node-generator npm` turning `package-lock.json` (the 11
  dependencies, `@lezer/generator` and esbuild: 47 packages with their
  own dependencies and esbuild's per-platform binaries) into npm cache
  entries, and `npm ci --offline`. Flathub
  asks for apps to be built from source, and a committed or downloaded
  minified bundle would be a second copy of the editor to keep in step
  with its source. esbuild is a native binary per platform; the
  generator's sources place this architecture's, and
  `ESBUILD_BINARY_PATH` points at it.
- **Toolchain.** The rust-stable extension of the 26.08 SDK is Rust
  1.98.0; it has no rustup, so `rust-toolchain.toml` (1.98.1) does not
  apply. The workspace's `rust-version` is 1.98, so it builds; the
  byte-for-byte conformance output is checked with the pinned toolchain
  on the other platforms, not in the Flatpak (see `docs/followups.md`).
- **Permissions.** Wayland with X11 fallback, IPC, `--device=dri` for
  the GPU view, `--share=network` for the update check alone ("Updates"
  above; OpenSCAD files can't reach the network), and `--filesystem=home`: a model reads the files beside
  it and in the user's library folder (`include`, `use`, `import`,
  fonts), and exports are written beside it, which the file chooser
  portal's one-file grant does not cover. OpenSCAD's own Flathub
  manifest (flathub/org.openscad.OpenSCAD) has `--filesystem=home` too.
  flatpak-builder-lint reports this as `finish-args-home-filesystem-access`,
  which would need an exception on Flathub (not submitted: see "Flathub: not submitted").
- **The `.scad` type** is `application/x-openscad`, OpenSCAD's own name
  (`resources/icons/openscad.xml` in its repository), so a system with
  both apps has one type; it is a subclass of `text/plain`.
- **Icons.** The icon art is the shape on transparency, which is how a
  GNOME app icon looks; the PNGs are committed so the build needs no
  image tools.

### Building it

The generated sources first (python3 with venv, and network), then
flatpak-builder with Flathub as the remote for the runtime and
extensions:

    linux/flatpak/generate-sources.sh
    flatpak remote-add --user --if-not-exists flathub https://dl.flathub.org/repo/flathub.flatpakrepo
    flatpak-builder --user --install-deps-from=flathub --force-clean \
        --repo=linux/flatpak/repo linux/flatpak/build-dir linux/flatpak/org.neoscad.NeoSCAD.yml
    flatpak build-bundle linux/flatpak/repo neoscad.flatpak org.neoscad.NeoSCAD
    flatpak install --user neoscad.flatpak && flatpak run org.neoscad.NeoSCAD

The SDK, the two extensions and a release build of the app take about
10 GB. CI's `flatpak.yml` does the same in Flathub's `gnome-51` image,
for x86_64 on `ubuntu-24.04` and aarch64 natively on `ubuntu-24.04-arm`
(the image is multi-arch; no QEMU), and uploads
`NeoSCAD-<commit>-linux-<arch>.flatpak` as an artifact. The x86_64 build
is blocking (it first passed on main on 2026-09-30); aarch64 is
`continue-on-error` until it has passed. The same workflow is a release
publish job ("Install from the release" below).

What was checked when this was added, in an Ubuntu 24.04 container:
`appstreamcli validate --no-net` (AppStream 1.0.2) passes;
`desktop-file-validate` passes; `update-mime-database` maps `*.scad` to
`application/x-openscad`; `flatpak-builder --show-manifest` reads the
manifest with both generated source files; `flatpak-builder-lint
manifest` reports only `finish-args-home-filesystem-access`, and
`flatpak-builder-lint appstream` only that the screenshot's URL does not
exist yet (it does once `linux/data/screenshots/window.png` is on
`main`).

### Install from the release

Every release from v0.2.0 attaches `NeoSCAD-<version>-linux-x86_64.flatpak`
and, when its build passed, `NeoSCAD-<version>-linux-aarch64.flatpak`,
each with a `.sha256` and a GitHub artifact attestation:

    sha256sum -c NeoSCAD-<version>-linux-x86_64.flatpak.sha256
    gh attestation verify NeoSCAD-<version>-linux-x86_64.flatpak -R neoscad/neoscad
    flatpak install --user NeoSCAD-<version>-linux-x86_64.flatpak
    flatpak run org.neoscad.NeoSCAD

The bundle names Flathub as its runtime repository (`flatpak
build-bundle --runtime-repo=https://dl.flathub.org/repo/flathub.flatpakrepo`),
so installing it offers to add Flathub if no remote has the runtime, and
pulls `org.gnome.Platform` 51 from there. The bundle carries no
repository of its own for the app (no `--repo-url`), so `flatpak update`
does not reach the next release: install its bundle the same way (with
`--reinstall` if flatpak reports the app as already installed). The app
says when there is one ("Updates"). The bundle
is built from the tagged commit in `flatpak.yml`, called by
`release.yml` as a publish job (`docs/release.md`); it is not on
Flathub.

### Flathub: not submitted

Flathub's requirements (docs.flathub.org, "Requirements", checked
2026-09-30) rule out submitting this manifest:

- "Flathub manifests must not contain AI-generated or AI-assisted
  content". `linux/flatpak/` was written with an AI agent.
- AI tools must not open or automate submission pull requests, or write
  their descriptions, commit messages or replies.
- AI-generated code in the application must be disclosed, and NeoSCAD is
  largely written with AI tools.
- Breaking these rules can mean rejection and a permanent ban.

So the Flatpak is distributed as a bundle on each GitHub release
("Install from the release", above) rather than through Flathub. A
Flathub listing would need a manifest the owner writes by hand, a pull
request the owner opens, and the disclosure. That's the owner's decision
to revisit, not a step to automate.

## Next

Milestone 2, in order:

1. Done: the customizer, check and measure panels (what they still lack
   against the macOS app is in docs/followups.md, "Linux").
2. Done: file watching.
3. **Zero-copy view**: export the frame as a dmabuf (Vulkan external
   memory) into a `GdkDmabufTexture`, falling back to the copy.
4. Done: the rest of File > Export, with a progress toast and
   cancellation (no options dialog yet: docs/followups.md, "Linux").
5. **Settings**: GSettings for the style, the editor's font size, window
   size and pane positions.
6. **Packaging**: Flathub (above; the release already attaches
   bundles); a GNOME thumbnailer from `client::preview`.
