# Audit: what the macOS app holds that the Linux and Windows ports would re-implement

Scope: `apple/App`, `apple/Core`, `apple/QuickLook`, `apple/Thumbnail`,
`apple/Editor/web` against `crates/ffi`, `crates/client`, `crates/session`,
`crates/lsp` and the web demo (`web/src`). Read 2026-09-30 at 7c4ebc0; no
code changed. Line numbers are of that commit.

## Where things stand (firm ground)

The heavy logic is already in Rust and already shared by two hosts.
`crates/client` (lib.rs:1-16) shapes document runs, console lines, the
customizer, check, measure and export as records; `crates/ffi` declares
them to UniFFI as `remote` types (document.rs:52-176, inspect.rs:87-320)
and `crates/web` sends the same records as JSON (`docs/web-protocol.md`).
The camera lives in Rust (`Viewport`, viewport.rs:168-424). One call,
`Core::run_document` (document.rs:213), evaluates, publishes diagnostics
through a `with_foreign` listener, swaps the model into the viewport and
returns console, files read and customizer state. Parameter sets, check,
measure, sections, picking and export are core calls with their own cancel
tokens (document.rs:339-388, inspect.rs:203-520). The CodeMirror bundle
(`apple/Editor/web/src`, 1037 lines) is consumed unchanged by the web demo
(`web/src/editor-host.js:1-6`).

So the Swift app is thin (3,800 lines outside the generated binding, mostly
AppKit/SwiftUI). What remains platform-neutral is glue and presentation,
small pieces each, but each is already written twice (Swift and `web/src`)
and would be written a third and fourth time.

## UniFFI constraints that shape the proposals

- `crates/ffi/Cargo.toml:30` pins `uniffi = "=0.32.2"`. The local crate
  source has async exports and foreign-implementable traits
  (`uniffi_macros-0.32.2/src/export/trait_interface.rs`, `util.rs:263`),
  which `ffi` already uses (`DocumentListener` document.rs:198,
  `ProgressListener` inspect.rs:80). Swift sees these as protocols.
- C#: `NordSecurity/uniffi-bindgen-cs` (README and CHANGELOG fetched
  2026-09-30): latest release `v0.11.0+v0.31.0` targets uniffi **0.31.0**,
  not 0.32.2; the CHANGELOG lists async methods (v0.8.4), "Traits/WithForeign"
  and async traits (v0.9.0), callback interfaces (v0.11.0), and one limit
  (strings/byte[]/lists capped at 2^31). Whether its generator accepts
  0.32.2 metadata and honours `#[uniffi(default = …)]` on record fields
  (used at document.rs:126-143, inspect.rs:87-97, 410-425): **unverified**.
  Owner decision: pin `ffi` to what the C# generator targets before the
  Windows work, or budget a bindgen-cs bump.
- Rule for everything below: synchronous methods on objects, records of
  plain fields (`Vec<f64>` points, as today), state changes through
  `with_foreign` traits, no Rust `async` (each host already runs core
  calls on its own pool: `apple/Core/Engine.swift:22-26, 224-237`). No
  clock in library crates (CLAUDE.md), so timers stay in the host and the
  core exposes the delay and the state machine.

## Candidates, by value to the ports

### 1. The document loop state machine (M, medium risk)

What: run once after a pause; supersede older runs; keep the core's buffer
in step; re-run the last mode when a watched file changes; keep edited
customizer values and drop those of parameters that vanished.
Where: `apple/App/Document/DocumentLoop.swift:30-150` (schedule, `run`,
`filesChanged`, `overrides` sorted by name, `refreshParameters`),
`SCADDocument.swift:148-187` (`corePath`, `coreInSync`, `requestCount`,
`lastMode`, 150 ms delay), `:316-360` (edit path, byte-length in-sync
check), `Inspect.swift:19-45` (`panelPath`, parts toggle). The web repeats
it with 300 ms and its own `superseded` flag (`web/src/app.js:47, 498-501, 672`).
Where to: `crates/client` (`document_loop.rs`), exported by `ffi` as an
object `DocumentController { open, replace_text, edit, set_parameter(name,
Option<ParameterValue>), reset_parameters, apply_parameter_set, set_parts,
request(mode), file_changed(paths), saved_as(path), close }`, with
`due() -> Option<RenderMode>` and `preview_delay_ms()` for the host's
timer, and a `DocumentObserver` (`with_foreign`) receiving a
`DocumentState` record (report, console, groups, edited values, selected
set, watched files). `run_document` stays the engine underneath.
Risk: `DocumentLoopTests` (9, e.g. `onePauseRunsOnceAndFeedsMarkersAndConsole`,
`aChangedIncludeOnDiskRerunsTheDocument`) must pass unchanged. The
untitled-path policy (`SCADDocument.swift:243-267`) needs
`NSDocumentController.currentDirectory` (host), but its "unique among open
documents and not on disk" loop can be `Core::untitled_path(dir, base)`.

### 2. Editor edits in UTF-16 (S, low risk)

`apple/Core/TextOffsets.swift` (74 lines, 6 tests) converts CodeMirror's
UTF-16 edits to UTF-8 `TextEdit`s; `SCADDocument.swift:337-345` checks the
byte length agrees. CLAUDE.md says positions convert only through
`lang::source`; this is a second conversion and C# would be a third. Add
`Core::edit_utf16(path, Vec<Utf16Edit>) -> DocInfo` in `client` (the
session holds the text; `lang/src/source.rs:91-126` has the helpers). Both
ports embed the same bundle (WebView2, WebKitGTK), so the `bridge.js`
contract (`EditorController.swift:282-357`: `ready`, `changes` with
`base`/`version`, `resync` on mismatch) should be documented, not moved.
`EditorTests` (19) cover it.

### 3. Console summary and filter model (S, low risk)

`ConsoleView.swift:116-143` builds "Rendered in … ms: 3D, bbox …, volume …,
N triangles, manifold" and the timings tooltip; `:146-175` maps
`ConsoleKind` to four filter groups. `web/src/ui/console.js:9-30` is a
copy. Add `RenderResult.summary: String` (or `describe_render(result,
mode)`) and `ConsoleLine.group: ConsoleGroup` in `client/document.rs`.

### 4. Customizer edit semantics (S, low risk)

`CustomizerView.swift:122-131, 207-223, 255-257` (a value equal to the
default is no override; slider snap to the step grid from `min`, kept to
the step's decimals; clamp; `%g` field format) and `DocumentModel.value(of:)`
(`SCADDocument.swift:106`). `web/src/model/customizer.js:1-40` says in its
header it mirrors the Swift. The core has `%g` already
(`client/document.rs:345`) and clamps when applying sets. Add
`edit_value(parameter, value) -> Option<ParameterValue>`,
`format_number(f64)`, and `parameter_set_path(doc_path)` for the
`name.json` rule at `DocumentLoop.swift:154-156`.

### 5. Check panel presentation (S, low risk)

`CheckPanel.swift:18-38` six printer presets ("written from memory";
copied at `web/src/ui/check.js:12-17`), `:42-81` defaults and
`apply(preset)` (`minWall = 2 × nozzle`), `:85-111` load-time validation,
`:264-274` summary line, `Inspect.swift:275-278` `firstError`. Add
`printer_presets()`, `CheckSettings::apply_preset`, `validated()` and
`CheckReport.summary` in `client/inspect.rs` (which validates ranges at
`:119-135`). Persistence keys (`check.*`, `export.*`, `EditorFontSize`)
stay per host; the values and bounds come from the core.

### 6. Measure and annotation building (M, low-medium risk)

`MeasurePanel.swift:44-100` (pick distance, target box, section slider
range, axis index/name, `mm()`, `extent`, `vector`), `:236-241`
`betweenText`; `Inspect.swift:195-266` turns findings, section outline,
closest pair and picks into `ViewLine`/`ViewMarker`, with box edges and
colours duplicating `session/snapshot.rs:98-104`. `web/src/ui/measure.js`
(360 lines) does it again. Move into the viewport as overlay state:
`Viewport::set_findings(report, selected)`, `set_section`, `set_between`,
`set_picks`, plus `MeasureResult.section_range(axis, part)` and
`BetweenResult.text`. `set_annotations` stays for tests. `PanelTests`
(`aThinWallIsAnErrorFindingTheViewTurnsTo`, `aClickInTheViewPicksASurfacePoint`) guard it.

### 7. Export format table and outcome (S, low risk)

`Export.swift:23-85` (title, core id, extension, dimension) copies
`session/export.rs:12-70` plus two image kinds; `SCADIntents.swift:31-53`
is a third copy; `web/src/engine/protocol.js:122-127` a fourth (four
formats only). `Export.swift:148-168` derives the alert from the console;
`:171-179` picks SVG after a 2D render. Add `export_formats() ->
Vec<ExportFormatInfo { id, title, extension, dimension, kind }>`,
`ExportResult.failure_reason`, `suggest_format(last_dimensions, preferred)`
in `client/inspect.rs`. Atomic write and stages are core already
(`inspect.rs:427-450`). `ExportTests` (4) cover the Swift.

### 8. Quick Look / thumbnail path (S-M, low risk)

`apple/Core/QuickLook.swift:70-158` (tight limits, a core per request,
notes for too-large, timed-out, unreadable files, first error, empty) and
`:167-195` (parsing "Can't find include file '…'" out of message text)
are what a GNOME thumbnailer or a Windows preview handler needs verbatim;
`QuickLookPage.html` (`:267-297`) and `escape` are pure strings. Add
`Core::preview_picture(path, text, PictureOptions) -> PreviewOutcome { png,
notes, unreadable }` with `unreadable` from diagnostics, not message text,
and `preview_html(outcome, title)`. The 6 s watchdog stays host (some
steps ignore cancellation, `QuickLook.swift:16-19`). `QuickLookTests` (7).

### 9. Small shared lists (S) and product decisions

- Light/dark scheme pair `ViewportController.swift:24-25`; `viewport.rs`
  has no `color_scheme_names()`, so a port cannot list schemes.
- `LanguageClient.swift:32` 150 ms debounce repeats `cli/src/lsp.rs:8`.
- Examples: the macOS app has none; the web loads a manifest
  (`web/src/examples.js`). Owner decision: ship them in `assets/` with
  `examples()` so the ports get File > Examples for free.
- Agent bridge: no Swift UI; the web's `web/src/agent/*` (885 lines of JS)
  talks to `neoscad serve`. A shared client-side state machine does not
  exist; owner decision, not a port blocker.

## Stays in Swift (platform-specific by nature)

`NSDocument`, autosave, windows/tabs, menus (`SCADDocument.swift:281-529`,
`MainMenu.swift`, `NeoSCADApp.swift`); `FileWatcher.swift` (FSEvents; ports
use inotify / `ReadDirectoryChangesW`, the core already names the files);
`MetalView.swift` and `ffi/layer.rs`; `EditorWebView`, `EditorSchemeHandler`,
script message handlers; `LanguageClient` threading; `LibraryViewer` (its
"user's own file" rule already uses core `library_dirs()`); App Intents;
save panels, alerts, progress sheet; `UserDefaults`; `Engine.swift` (a
40-line thread-pool adapter each host writes once).

## Web demo duplicates that would also come from the core

`ui/console.js:9-30` (3), `model/customizer.js` (4), `ui/check.js:12-27`
(5), `ui/measure.js` annotation building (6), `engine/protocol.js:122-127`
(7), `app.js` debounce/supersede (1). Each becomes a field or call on the
existing JSON protocol once it is in `client`.

## Checked and found fine

Camera, presets, view settings, colour scheme: Rust. Parameter parsing,
grouping, controls, sets with OpenSCAD's clamping: `lang::customizer` and
`client`. Console lines in editor positions: `client/document.rs:480`.
Check/measure/export options and validation: `client/inspect.rs`. Editor
diagnostics: `crates/lsp` behind `LanguageServer`; Swift relays strings.
Run cancellation and supersession: `session`. Annotation drawing,
`look_at`, `ray_at`: Rust.

## Status (2026-09-30)

- Step 1, tables and strings: done. `client/src/present.rs`
  (`describe_render`, `describe_timings`, `console_group(s)`,
  `export_formats`, `export_failure_reason`, `suggest_export_format`,
  `printer_presets`, `PrinterSettings` with `apply_preset`, `validated`,
  `check_options`, `check_summary`, `first_error`); Swift switched.
- Step 2, customizer and paths: done (`edit_parameter` with
  `ParameterEdit`, `format_number`, `snap_to_step`, `parameter_set_path`,
  `Client::untitled_path`, `color_scheme_names`,
  `DEFAULT_PREVIEW_DELAY_MS` = 150).
- Examples (owner decision, added): done. `client/src/examples.rs` embeds
  `web/examples/` and its manifest; File > Examples in the macOS app.
- Step 3, `edit_utf16`: done. `lang::source::byte_offset_of_utf16`,
  `client/src/text.rs` (`Client::edit_utf16`, `EditorText`);
  `TextOffsets.swift` and its tests deleted, the tests ported to Rust.
- Step 4, the view's overlay: done. `client/src/overlay.rs`
  (`view_overlay`, `pick_distance`, `section_range`),
  `Viewport::set_overlay`; `between_text` and `mm()` stay in Swift
  (locale-formatted numbers).
- Step 5, `DocumentController`: done. `client/src/document_loop.rs`,
  `ffi/src/controller.rs` with `DocumentObserver`; `DocumentLoop.swift`
  keeps the timer. The web page's loop is not switched (followups).
- Step 6, preview: done. `client/src/preview.rs`,
  `Core::preview_picture`, `preview_html`; `QuickLook.swift` keeps the
  watchdog.
- Step 7 (C# bindgen CI) and step 8's owner decisions: not started here.

## Recommended extraction plan

Each step is one builder task: add to `client` + `ffi`, regenerate the
binding (`scripts/apple/build-core.sh`), switch the Swift call site, delete
the Swift copy, keep `xcodebuild … test` green, update the web protocol
where the same record crosses.

1. Tables and strings (3, 7, 5): `describe_render`, `ConsoleLine.group`,
   `export_formats`, `failure_reason`, `suggest_format`, `printer_presets`,
   `CheckReport.summary`. Switch `ConsoleView`, `Export.swift`,
   `SCADIntents`, `CheckPanel`.
2. Customizer semantics and paths (4, 9): `edit_value`, `format_number`,
   `parameter_set_path`, `untitled_path`, `color_scheme_names`.
3. `edit_utf16` (2): delete `TextOffsets.swift`, port its six tests to Rust.
4. Viewport overlay (6): `Inspect.swift` shrinks to request wiring;
   `MeasurePanel` keeps widgets only.
5. `DocumentController` (1) with the observer trait; `DocumentLoop.swift`
   becomes timer + observer. Run `DocumentLoopTests` and
   `PipelineLatencyTests` before and after.
6. `preview_picture` + `preview_html` (8); `QuickLook.swift` keeps the
   watchdog and the `QLPreviewReply` adapter.
7. Decide bindgen-cs alignment (pin or bump) and add a CI job generating
   the C# bindings from `ffi`, so later API additions are checked against
   both generators.
8. Owner decisions: examples in `assets/`, a shared agent-link state
   machine, and naming `client` as the port boundary in `docs/architecture.md`.
