# The Linux desktop app

`crates/linux-app` (package `neoscad-linux-app`, binary `neoscad-gtk`) is
the Linux counterpart of the macOS app (`apple/`): GTK 4 and libadwaita
around the same Rust core, following the GNOME HIG. It is Rust through
and through, so it calls `crates/client`, `session` and `render` directly;
there is no UniFFI layer as there is for Swift (`crates/ffi`).

This page covers milestone 1: what it does, how it is put together, how
to build and run it, and what comes next.

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
  `client::export_formats()`.
- **Style**: follows the system's light or dark preference through
  `AdwStyleManager`, or forced from the menu's Style submenu. The editor
  follows through `prefers-color-scheme`, and the view switches between
  the macOS app's scheme pair (Cornfield, Tomorrow Night) and builds the
  model again in the new colours.
- **Shortcuts**: a `GtkShortcutController` on the window, in the capture
  phase so the web view does not take them first: Ctrl+N, Ctrl+O, Ctrl+S,
  Ctrl+Shift+S, Ctrl+Shift+E (Export STL), Ctrl+W, Ctrl+Q, F5, F6,
  Ctrl+Shift+V (View All). Editing keys are left to CodeMirror.

## How it is put together

The crate is a library plus a binary. The library (`src/*.rs`) is the host
logic that is not GTK glue, and builds and is tested on every platform:

| Module | What |
|---|---|
| `bridge.rs` | The editor protocol's state machine: message parsing, versions, applying changes to the host's copy (`client::EditorText`), resyncs, the scripts sent to the page |
| `resources.rs` | The `neoscad-editor:` scheme: which files are served, the per-load CSP nonce, where the bundle is looked for |
| `host.rs` | The session configuration (disk plus MCAD in memory, library path, fonts, clock, seed, limits) and the GPU |
| `document.rs` | A window's document: path, text, edited state (undo-aware), titles |
| `run.rs` | A document run and an export, off the main thread |
| `view.rs` | The viewport drawing into a texture, device pixel sizes, drag mapping |

The window (`src/app/`) is compiled only with the `gtk` feature.

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

There is no language server yet: the page's LSP client is answered by a
stub (`initialize` with no capabilities, other requests "method not
found"), so markers, completion and hover come in milestone 2.

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
`../share/neoscad/editor` or `editor` beside the binary, else the
checkout's `apple/Editor/web/dist` when run from `target/`.
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
  resources, titles and edited state, runs and exports without a GPU.
- `linux/smoke.sh BIN [MODEL]` (Linux, Xvfb): opens the model and waits
  for its preview; with `TYPE=1` it types into the editor and waits for
  the second preview; with `SHOTS=DIR` it saves light and dark
  screenshots. It kills the app above 2 GB of memory.
- CI's `linux-app` job (ubuntu-24.04) runs clippy and the tests with the
  `gtk` feature, builds the editor bundle and runs the smoke test with
  typing.

## Next

Milestone 2, in order:

1. **Language features**: a `crates/lsp` server per window over the
   shared session, as `LanguageClient.swift` does, with the run's
   diagnostics handed to it (`lsp::Options::host_diagnostics`); go to
   definition opening the user's files and library files read-only.
2. **Customizer, check and measure panels**: `AdwPreferencesGroup`-style
   side panels over `client`'s parameter groups, `edit_parameter`,
   parameter sets, `check` and `measure`, with the view's overlay
   (`client::view_overlay`).
3. **File watching**: re-run when an include or import changes on disk
   (`GFileMonitor` over the run's files, `DocumentLoop::files_changed`).
4. **Zero-copy view**: export the frame as a dmabuf (Vulkan external
   memory) into a `GdkDmabufTexture`, falling back to the copy.
5. **The rest of File > Export** (3MF, OBJ, OFF, SVG, DXF, PDF, the
   snapshot sheet), with a progress toast and cancellation.
6. **Settings**: GSettings for the style, the editor's font size, window
   size and pane positions.
7. **Packaging**: a Flatpak manifest (GNOME runtime, which carries GTK,
   libadwaita and WebKitGTK), the desktop file, AppStream metadata and
   icon under `org.neoscad.NeoSCAD`, the editor bundle installed to
   `share/neoscad/editor`; then a GNOME thumbnailer from
   `client::preview`.
