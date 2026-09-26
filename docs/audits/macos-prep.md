# Research: preparing phase 8 (the macOS app)

Research only; no code changed. The local toolchain is Xcode 27.0
(27A266a), the macOS 27.0 SDK, macOS 27.0 (26A428) on arm64, XcodeGen
2.44.1 (Homebrew), and rustc 1.98.1 with the targets `aarch64-apple-darwin`
and `wasm32-unknown-unknown`. The status of each claim is marked:

- **verified locally**: a command was run, or the file was read;
- **retrieved**: a source was fetched and quoted;
- **unverified**: neither.

## Recommendations at a glance

1. **Project definition: XcodeGen `project.yml` now; plan to emit Xcode's
   JSON `project.xcproj` later.** Xcode's JSON format is real. But it is
   the default only from Xcode 27.2, which is in beta. The installed 27.0
   cannot create one from the command line. Apple's reference library is
   at 0.1.0, and XcodeGen's JSON output is an open PR. Keep the committed
   spec in XcodeGen and switch its output format when those land.
2. **Bridge: UniFFI 0.32** (proc-macros) over a new `crates/ffi`,
   `crate-type = ["staticlib"]`, arm64 only, packaged as an XCFramework
   by a script. Meshes never cross the bridge, because the Rust renderer
   draws them. PNG thumbnails cross once, as a copy; that is fine.
   **Every export must return `Result`**, or a Rust panic traps the app.
3. **Viewport: wgpu 30** from a `CAMetalLayer` that an `NSView` owns
   (`SurfaceTargetUnsafe::CoreAnimationLayer`). The view drives
   `configure` on resize and scale changes. Pace frames with
   `NSView.displayLink(target:selector:)` (macOS 14+), not
   `CAMetalDisplayLink`.
4. **Editor: CodeMirror 6 bundled with esbuild into the app**, served
   through a `WKURLSchemeHandler`. Swift and JS talk through
   `WKScriptMessageHandlerWithReply`. Language features come from the
   official `@codemirror/lsp-client`, whose `Transport` is two string
   callbacks, so an in-process Rust LSP needs no network server.
5. **Not sandboxed for the first build** (Developer ID, hardened
   runtime, notarization). The Quick Look extensions are sandboxed, as
   Xcode's templates set, and render self-contained files plus bundled
   libraries.

## 1. Project definition

### Does Xcode 27 have a native JSON project format? Yes, with caveats

- **The format exists.**
  - Apple's page "Updating your Xcode project configuration file format"
    says: "In Xcode 27.2 and later, the default project configuration
    file is a smaller, hierarchical, self-describing JSON file with a
    `.xcproj` extension. Xcode 27 and later supports both file formats"
    and "The JSON file format `.xcproj` is compatible with Xcode 27 and
    later." *(retrieved: `developer.apple.com/tutorials/data/documentation/xcode/updating-your-xcode-project-configuration-file-format.json`)*
  - The Xcode 27.2 release notes, which are titled **"Xcode 27.2 Beta
    Release Notes"**, say: "Xcode now supports a JSON-based project
    format (.xcproj) ... Enable it in the file inspector. Projects using
    .xcproj also open in earlier versions of Xcode 27. (184661114)"
    *(retrieved, same JSON endpoint)*
- **How you switch: through the UI only.** Apple's page says: "In the
  Project navigator, select the project, and in the File inspector,
  choose JSON from the Project Format pop-up menu." It documents no
  command-line conversion. *(retrieved)*
- **What the installed Xcode 27.0 has.** *(verified locally)*
  - `xcodebuild -help` lists a `-convert-project FORMAT` option. Its
    formats are "Xcode 2.4 … Xcode 26.3, Xcode 27.0", and `json` is
    rejected ("Could not find requested format 'json'").
  - Converting an XcodeGen project to "Xcode 26.3" gives `objectVersion
    = 100`, and to "Xcode 27.0" gives `objectVersion = 110`. Both are
    still OpenStep-plist `project.pbxproj`.
  - The man page does not document `-convert-project`.
  - The 27.0 binaries already carry the JSON codec and the conversion UI.
    `DevToolsCore` has `projectWithJSONData:`, `usesJSONEncoding` and the
    file name `project.xcproj`. `Xcode3UI` has `_convertToJSONProjectFormat`
    and the alert "The JSON project format relies on brand new
    functionality in Xcode 27, and will render %@ inoperable on earlier
    versions of Xcode."
  - I did not exercise opening a `.xcproj` in 27.0: Apple's repository
    has no sample files, and I did not write one by hand.
- **The tooling around it is days old.** *(retrieved: GitHub API)*
  - `apple/xcode-project-format` (Apache-2.0) was created 2026-09-15.
    Its one tag is `0.1.0`. It "ships `xcprojformatter`" and needs Swift
    6.1 and macOS 14.
  - `tuist/XcodeProj` merged an "[Experimental]" writer (PR #1177,
    2026-09-17).
  - XcodeGen: PR #1653 "Add support for Xcode 27.2's JSON project
    format" is **open**. It adds `options.projectFormat: json` on
    XcodeProj 9.17. Issue #1651 reports that XcodeGen can emit a group
    with two parents, which "Xcode 27.2 rejects ... and cannot open the
    generated project". That happens when `project.yml` sits inside a
    source directory and uses `sources: [../Module]`.
- **XcodeGen with Xcode 27.0 works.** *(verified locally)* XcodeGen 2.44.1
  generated a SwiftUI macOS app (`objectVersion = 77`) that `xcodebuild`
  27.0 built (`BUILD SUCCEEDED`). The current release is 2.46.0
  (2026-07-16) *(retrieved)*; Homebrew offers the upgrade.

### Recommendation

Use **XcodeGen**. Commit `apple/project.yml`, generate
`apple/NeoSCAD.xcodeproj`, and keep it gitignored, as
`docs/architecture.md` already requires.

- Upgrade to 2.46.0.
- Keep `project.yml` outside the source directories to avoid #1651.
- Revisit when **all three** hold: Xcode 27.2 is final, and XcodeGen
  releases `projectFormat: json` (or its equivalent), and
  `xcode-project-format` passes 0.x. Then flip the generator's output
  format; the committed spec does not change.

Writing `project.xcproj` directly, as the committed source of truth, is
possible. But it makes the `.xcodeproj` a hand-edited file, and no
27.0-era tool validates it; `xcprojformatter` ships with 27.2.
**Owner decision** if the preference is Apple's format over a generator.

## 2. Rust → Swift bridge

### UniFFI

- **Version: 0.32.2**, from crates.io (max stable, updated 2026-09-23)
  *(retrieved)*. The changelog's latest section is 0.32.1 (2026-09-08);
  0.32.0 is from 2026-06-30.
- **Build shape.** `uniffi-bindgen-swift` takes the built library and
  generates Swift sources, headers and an "XCFramework-compatible
  modulemap" (`--xcframework --modulemap`) *(retrieved:
  `docs/manual/src/swift/uniffi-bindgen-swift.md`)*. UniFFI's Xcode page
  leaves compiling the staticlib "beyond the scope of this document"
  (`swift/xcode.md`).
  - Plan: `cargo build --release --target aarch64-apple-darwin -p
    neoscad-ffi` → `libneoscad_ffi.a` → `uniffi-bindgen-swift` →
    `xcodebuild -create-xcframework -library ... -headers ...`.
  - A static library, not a dylib: one process, no install-name work,
    and it can be embedded in the app and both extensions.
- **arm64 only.** Secondary reports say macOS 26 Tahoe is the last
  release for Intel Macs, announced at WWDC25, and that macOS 27 runs
  only on Apple silicon. *(retrieved, secondary sources only: Tom's
  Hardware, MacRumors. Apple's own statement was not retrieved.)*
  - The macOS 27 SDK still lists `x86_64` *(verified: `SDKSettings.json`)*,
    and only the arm64 Rust target is installed.
  - A new app for 2026 has little reason to carry a second Rust build.
  - Set `ARCHS = arm64` explicitly: a new target defaults to
    `ARCHS = arm64 x86_64` *(verified: `-showBuildSettings`)*, and an
    arm64-only static library would then fail to link for x86_64.
  - **Owner decision:** the deployment target. macOS 26 would also admit
    Intel Macs, which then need x86_64. macOS 27 makes arm64-only
    natural.
- **Plugging into the build: a prebuilt artifact, rebuilt by a script.**
  - Use `scripts/apple/build-core.sh`, called from a pre-build "Run
    Script" on the core framework target, with input and output file
    lists so Xcode skips it when nothing changed.
  - `ENABLE_USER_SCRIPT_SANDBOXING` is `NO` on the XcodeGen target
    *(verified)*. Keep it `NO` for that target: `cargo` reads the whole
    workspace.
  - Alternative: build the XCFramework only by hand or in CI, and point
    a local Swift package's `binaryTarget` at it. That is simpler for
    Xcode, but stale-binary bugs come easily.
- **Copies.** *(retrieved: `types/bytes.md`, CHANGELOG)*
  - Since 0.32.0, a `&[u8]` argument borrows Swift's `Data` with no copy
    ("The call runs inside `Data.withUnsafeBytes`"). This works foreign
    → Rust only, in argument position only, and not in async functions.
  - `&mut [u8]` (`inout Data`, write in place) is **unreleased**; it is
    in the changelog's Unreleased section.
  - Returned `Vec<u8>` is copied through a `RustBuffer`.

  For NeoSCAD this is enough:
  - Meshes go from the session to wgpu inside Rust, and never cross.
  - A snapshot or thumbnail PNG (tens to hundreds of KB) is copied once.
  - For a Quick Look thumbnail, draw into the `CGContext` that
    `QLThumbnailReply` provides from an RGBA buffer: either return
    `Vec<u8>`, or once `&mut [u8]` is released, have Rust fill a
    Swift-allocated buffer.
- **Panics.** *(retrieved: `swift/templates/macros.swift:196`, `Helpers.swift`)*
  UniFFI turns a Rust panic into `UniffiInternalError.rustPanic`. But a
  function that does not throw is generated as `try! rustCall(...)`, so
  the Swift app traps.
  - Every exported function must return `Result<_, CoreError>`.
  - `crates/ffi` should also wrap each call in `catch_unwind`, as
    `serve.rs:294` does, so session locks and caches survive.
    `session` itself has no `catch_unwind` today *(verified: grep)*.

### Alternatives

- **`swift-bridge` 0.1.59** (last release 2026-01-06) *(retrieved:
  crates.io)*. It is still 0.1.x, has had less activity, and has no
  advantage for our small, coarse API.
- **A hand-written C ABI plus `cbindgen` 0.29.4.** Zero-copy both ways
  and no generator, but every string, error and callback is hand-marshalled.

Choose one of these only if profiling shows the bridge matters. With
meshes staying in Rust, it should not.

## 3. wgpu in an AppKit/SwiftUI view

All of this was **verified locally** from `wgpu-30.0.1`, `wgpu-hal-30.0.1`
and `raw-window-metal-1.1.0` in `~/.cargo/registry`. wgpu 30.0.1 is the
current release *(retrieved: crates.io)* and the one this repository
uses.

- **Creating a surface.** There are two ways.
  - `SurfaceTargetUnsafe::CoreAnimationLayer(*mut c_void)`
    (`wgpu/src/api/surface.rs:430-436`), which reaches
    `Surface::from_layer` (`wgpu-hal/src/metal/surface.rs:46`).
    **Recommended:** Swift creates the `CAMetalLayer` (the view's
    `makeBackingLayer`, with `wantsLayer = true`) and passes
    `Unmanaged.passUnretained(layer).toOpaque()` to Rust; wgpu retains it.
  - `SurfaceTargetUnsafe::RawHandle` with raw-window-handle 0.6.2
    `AppKitWindowHandle { ns_view }`. wgpu-hal then calls
    `raw_window_metal::Layer::from_ns_view` (`metal/mod.rs:165-168`),
    which adds its *own sublayer*. It keeps that sublayer's `bounds` and
    `contentsScale` in step with KVO observers (`raw-window-metal/src/observer.rs`).
    That is convenient, but the observer "panic!s" on an unknown key path
    (`observer.rs:189`), and it hides the layer from the Swift side.
- **Threading.** `Surface::dimensions()` "is safe to call off of the main
  thread" (`metal/surface.rs:187-190`). HDR queries must run on the main
  thread (`surface.rs:60-88`). Plan:
  - Create the layer, handle resize, and run the display link on the
    main thread.
  - Render (acquire, draw, present) either on the main thread or on one
    dedicated render thread that owns the device and queue.
  - Do not share `configure` across threads without a lock.

  Starting on the main thread is simplest: wgpu encoding for a CAD
  viewport is sub-millisecond, and the heavy work (evaluation, booleans)
  already runs on session threads.
- **Resize and display scale.** With a caller-owned layer, the app sets
  `layer.contentsScale = window.backingScaleFactor`. It does so in
  `viewDidChangeBackingProperties`, and in `setFrameSize` or `layout` it
  calls into Rust to `surface.configure` with `bounds × scale` in pixels.
  wgpu-hal reads the size as `bounds × contentsScale`
  (`surface.rs:196-203`). Set `layerContentsRedrawPolicy =
  .duringViewResize` and redraw inside live resize to avoid stretching.
- **Frame pacing and ProMotion.** *(verified in the macOS 27 SDK headers)*
  - `-[NSView displayLinkWithTarget:selector:]` (`NSView.h:662`) returns
    a `CADisplayLink` that tracks the view's screen. The class is
    `API_AVAILABLE(macos(14.0))` (`CADisplayLink.h:19`). It fires at the
    display's rate, so 120 Hz on ProMotion.
  - `preferredFrameRateRange` is annotated only for iOS and tvOS 15
    (`CADisplayLink.h:100`), so whether it can be set on macOS is
    **unverified**. Check at build time.
  - Render only when something changed (a camera move, a new mesh),
    and pause the link otherwise, to save power.
  - `CAMetalDisplayLink` (macOS 14, `CAMetalDisplayLink.h:33`) hands out
    its own drawable. wgpu acquires its own `nextDrawable` through
    `get_current_texture`, so the two do not fit together without
    changes to wgpu. This conclusion is inferred from the API shapes,
    not tested.
  - Present mode: `Fifo`. wgpu-hal maps `Immediate` to
    `setDisplaySyncEnabled(false)` (`metal/surface.rs:261-337`).
- **Examples.** wgpu's own examples go through winit, not an embedding
  view. The pattern above follows from the hal source quoted. No
  official AppKit embedding example was found (I did not search
  exhaustively).

## 4. CodeMirror 6 in WKWebView

- **Versions** *(retrieved: npm registry)*: `@codemirror/view` 6.43.13,
  `state` 6.7.6, `language` 6.12.4, `autocomplete` 6.20.3, `lint`
  6.9.7, `lsp-client` 6.3.0, `codemirror` 6.0.2, `esbuild` 0.28.2.
- **Bundling, offline.** CodeMirror ships as ES modules ("not currently
  practical to run the library without some kind of bundler"; the guide
  recommends rollup or Vite) *(retrieved: codemirror.net/docs/guide)*.
  - Bundle with esbuild into one `editor.js` plus `editor.html` under
    `apple/Editor/web/dist`, copied into the app as a folder resource.
  - Commit `package-lock.json`. Build in the core script or in a
    separate `npm ci && npm run build` step. The build machine needs
    node; the app needs no network.
  - Serve the files with `setURLSchemeHandler(_:forURLScheme:)` on a
    custom scheme such as `neoscad-editor://` (macOS 10.13+,
    `WKWebViewConfiguration.h:227`). `loadFileURL(_:allowingReadAccessTo:)`
    also works (`WKWebView.h:146`). A custom scheme gives a stable
    origin and lets the app answer `fetch()`s later (for example, for
    library files).
- **The Swift↔JS bridge.**
  - JS → Swift with a reply: `WKScriptMessageHandlerWithReply`
    (`WKScriptMessageHandlerWithReply.h:39`); JS awaits
    `window.webkit.messageHandlers.core.postMessage(msg)`.
  - Swift → JS: `callAsyncJavaScript(_:arguments:in:contentWorld:)`,
    which passes arguments as structured values, not string
    concatenation (the header mentions it: `WKJSHandle.h:36-50`).
  - Put the editor's scripts in a dedicated `WKContentWorld`.
  - Set `isInspectable` (macOS 13.3+, `WKWebView.h:691`) in debug
    builds for Web Inspector.
- **Document sync.** CodeMirror's `ViewUpdate.changes` map one-to-one
  onto serve's `update` `edits` (byte offsets). The JS side sends
  UTF-16 offsets, so convert in Swift or in Rust. The Rust session stays
  the source of truth for evaluation, and the Swift document for saving.
- **Language features without a network server.** `@codemirror/lsp-client`
  6.3.0 exports `LSPClient`, `serverDiagnostics`, `hoverTooltips`,
  `serverCompletion`, `formatDocument`, `renameSymbol`, `signatureHelp`
  and `jumpToDefinition`. Its `Transport` is `{ send(message: string);
  subscribe(handler); unsubscribe(handler) }` (`dist/index.d.ts:170`)
  *(verified: package from the npm registry)*.
  - So a Rust LSP (the planned `crates/lsp`, which does not exist yet)
    can run in-process: JS `send` goes to Swift `postMessage`, then an
    FFI `lsp_handle(String)`; server notifications go back through
    `callAsyncJavaScript`.
  - The content already exists: diagnostics with spans and hints, `docs`
    for hover and completion, and `fmt` for formatting.
  - Fix `docs`' library paths and private names first
    (`agent-surface.md` finding 9).
- **Large files.** "CodeMirror doesn't render the entire document, when
  that document is big ... only render that plus a margin around it"
  (the guide's "Viewport" section) *(retrieved)*. BOSL2's largest files
  are a few thousand lines, well within that. Measure the time to open
  them in 8d rather than assume it.
- **Keyboard, IME and accessibility caveats.** These are **unverified**
  and need testing in the spike:
  - AppKit's menu key equivalents (⌘Z, ⌘S, ⌘F) are matched before the
    web view sees the key. Route ⌘Z and ⇧⌘Z to CodeMirror's history,
    not `NSDocument`'s undo manager, or disable the app's Undo item while
    the editor has focus.
  - Composition input goes through contenteditable ("composition ...
    handling" is one of the exceptions to its state model, per the
    guide).
  - VoiceOver over a WKWebView contenteditable is less capable than
    `NSTextView`. The editor otherwise gives the accessibility tree
    CodeMirror's DOM.

## 5. App extras

- **UTI for `.scad`.** OpenSCAD's own `Info.plist` declares
  `CFBundleDocumentTypes` for `scad` but **no UTI**, so Finder types
  `.scad` files as `dyn.ah62d4rv4ge81g25bqu` *(verified: `plutil`,
  `mdls`)*. Quick Look matches "the exact Uniform Type Identifiers ...
  It's not sufficient to list a parent UTI" *(retrieved: Apple, "Providing
  thumbnails of your custom file types")*. NeoSCAD must declare one,
  for example `org.openscad.scad` conforming to `public.plain-text` and
  `public.source-code`. **Owner decision:** import it (someone else's
  format) or export it (claim it). Importing is the polite choice for
  OpenSCAD's format.
- **Quick Look.** *(verified in SDK headers)*
  - Preview: data-based, with `QLPreviewingController
    providePreview(for:completionHandler:)` and a `QLPreviewReply`
    (macOS 12+; `QLPreviewReply.h:60`, `initWithContextSize:isBitmap:drawingBlock:`).
  - Thumbnail: `QLThumbnailProvider provideThumbnail(for:_:)` with
    `QLThumbnailReply(contextSize:drawingBlock:)` (macOS 10.15+).
  - Both can link the same static Rust core and call the session to
    evaluate, render and snapshot offscreen (the `render` crate already
    draws to textures).
  - Xcode 27's Thumbnail Extension template sets `ENABLE_APP_SANDBOX =
    YES` *(verified: its `TemplateInfo.plist`)*. I did not retrieve
    Apple's statement that app extensions *must* be sandboxed; treat
    them as sandboxed.
  - Consequence: an extension can read the previewed file, but not an
    `include <BOSL2/std.scad>` from the user's library folder. Show
    self-contained files and bundled MCAD, and show a "needs libraries"
    placeholder otherwise.
  - Budget memory and time tightly. `agent-surface.md` finding 1 applies
    doubly here, because Finder calls these extensions unasked.
- **Documents.** SwiftUI `DocumentGroup` takes `FileDocument` or
  `ReferenceFileDocument` and gives macOS "document-based menu support"
  *(retrieved: Apple, DocumentGroup)*.
  - I did not retrieve what `DocumentGroup` promises for autosave in
    place and the Versions browser on macOS.
  - The architecture asks for both, and the text lives in the web view.
    So an **`NSDocument` subclass** (`autosavesInPlace = true`), with the
    window content hosted by `NSHostingView`, is the lower-risk choice:
    it gives explicit control over when to pull text from CodeMirror
    for saving.
  - Treat `DocumentGroup` as an 8f spike, not the plan.
- **App Intents / Shortcuts.** `AppIntents.framework` is in the SDK
  *(verified)*. Natural intents are "Render SCAD to STL", "Snapshot SCAD
  file" and "Check SCAD file". They wrap the same FFI calls, so they are
  thin. Details were not researched further.
- **Sandboxing.**
  - Under App Sandbox, an open panel grants a file, or recursively a
    chosen folder. Security-scoped bookmarks persist that across
    launches. "Document-relative bookmarks" cover "supporting files
    those projects reference" (the IDE example). `NSIsRelatedItemType`
    covers related files with different extensions. *(retrieved: Apple,
    "Accessing files from the macOS App Sandbox")*
  - OpenSCAD's model does not fit this well:
    - `include <../x.scad>` can reach anywhere.
    - `OPENSCADPATH` and the user library folder are global.
    - `import()` names arbitrary files.
  - A sandboxed build must ask once for each project folder and each
    library folder, keep the bookmarks, and resolve every read through
    them. The session's `FileSystem` trait (the layer
    `mcp::roots::RootedFs` already wraps) is the right place for that.
  - **Recommendation: not sandboxed for the first build.** Distribute as
    Developer ID with hardened runtime and notarization. Only the App
    Store requires the sandbox ("a requirement for distributing your app
    on the App Store" *(retrieved: Apple, "Protecting user data with App
    Sandbox")*). The architecture targets OpenSCAD compatibility first,
    and the extensions are sandboxed anyway.
  - Keep every file access behind the session's `FileSystem`, so a later
    sandboxed build is a new `FileSystem`, not a refactor. **Owner
    decision:** whether App Store distribution is a goal.

## Proposed skeleton

```
apple/
  project.yml                 # XcodeGen spec (committed; the .xcodeproj is generated, gitignored)
  App/                        # NeoSCAD.app: SwiftUI + AppKit shell
    NeoSCADApp.swift
    Document/                 # NSDocument subclass, window controller, autosave
    Viewport/                 # MetalView (NSView + CAMetalLayer), NSViewRepresentable, display link
    Editor/                   # EditorWebView (WKWebView, scheme handler, message bridge)
    Panels/                   # console, customizer, issues, measure
    Intents/                  # App Intents
    Info.plist, NeoSCAD.entitlements (hardened runtime; no sandbox)
  Editor/web/                 # package.json, package-lock.json, esbuild config, src/*.ts
    dist/                     # built editor.html + editor.js (gitignored; copied as a resource)
  QuickLook/                  # Preview extension (sandboxed)
  Thumbnail/                  # Thumbnail extension (sandboxed)
  Core/                       # NeoSCADCore.framework: generated Swift + a thin Swift API
    Generated/                # uniffi-bindgen-swift output (gitignored)
  Tests/                      # XCTest/Swift Testing: core calls, document round trip
  build/                      # NeoSCADCore.xcframework (gitignored)
crates/ffi/                   # neoscad-ffi: staticlib, UniFFI exports over session + viewport
crates/lsp/                   # in-process LSP over session (diag, docs, fmt)
scripts/apple/build-core.sh   # cargo (aarch64) -> uniffi-bindgen-swift -> xcframework [-> npm build]
```

**Targets** (in `project.yml`):

| Target | Type | Contents |
|---|---|---|
| `NeoSCADCore` | framework | Generated bindings plus the XCFramework |
| `NeoSCAD` | application, arm64 | Depends on the core; embeds the two extensions |
| `NeoSCADQuickLook` | app-extension | Quick Look preview; depends on the core |
| `NeoSCADThumbnail` | app-extension | Thumbnail; depends on the core |
| `NeoSCADTests` | unit tests | |

**Build**:

```sh
scripts/apple/build-core.sh            # also run by NeoSCADCore's pre-build phase (with input/output file lists)
xcodegen generate --spec apple/project.yml
xcodebuild -project apple/NeoSCAD.xcodeproj -scheme NeoSCAD -configuration Debug build
xcodebuild ... -scheme NeoSCAD test
```

`cargo test` stays the gate for the Rust side. `scripts/wasm-check.sh`
must keep passing: `crates/ffi` is native-only, and the library crates
are unchanged.

## Phase 8 sub-steps (one builder session each)

- **8a: Project skeleton.**
  - `apple/project.yml`, an empty `NSDocument` app that opens and saves
    `.scad` as plain text with the UTI declared, `ARCHS = arm64`, and a
    deployment target per the owner's decision.
  - `xcodegen` and `xcodebuild` build from a clean checkout; add to
    `CLAUDE.md`'s build section. No Rust yet.
- **8b: Core bridge.**
  - `crates/ffi` (UniFFI 0.32, proc-macros, every export returns
    `Result`, with `catch_unwind` around each call) and
    `build-core.sh` → XCFramework.
  - `NeoSCADCore` exposes `open`, `update(edits)`, `render` →
    geometry/diagnostics, `snapshot` → PNG.
  - A Swift test renders `cube(10)` and gets volume 1000.
- **8c: Viewport.**
  - `MetalView` with a caller-owned `CAMetalLayer` →
    `CoreAnimationLayer` surface; `render` gains a surface target.
  - Resize and scale handling, a display link, render-on-change, and an
    orbit/pan/zoom camera; the mesh stays in Rust.
  - Measure frame time and 120 Hz on a ProMotion display.
- **8d: Editor.**
  - Bundle CodeMirror 6 (esbuild, lockfile, offline) behind a
    `WKURLSchemeHandler`.
  - The message bridge, and edits to `update` with UTF-16/byte offset
    conversion; save pulls the text.
  - Settle undo, keyboard routing and IME in a checklist.
- **8e: Language features.**
  - `crates/lsp` (diagnostics, hover from `docs`, completion,
    formatting) over the bridge with `@codemirror/lsp-client`.
  - Fix `docs`' library paths and private names first.
- **8f: The document loop.**
  - Evaluate on edit (debounced, superseding) and show the results:
    console panel, customizer basics, and an error gutter.
  - Include-relative resolution, and file watching for included files.
  - Autosave and versions verified.
- **8g: Resource limits and robustness** (`agent-surface.md` finding 1)
  in `session::Options`: vertex, list and time budgets, with
  cancellation on document close. Needed before 8h, because extensions
  and live editing both run untrusted or half-typed code.
- **8h: Quick Look preview and thumbnail extensions** (sandboxed;
  self-contained files and bundled MCAD; offscreen snapshot through the
  core).
- **8i: Panels and export.** Check and measure panels (after the seam
  fix, finding 4), export UI with real errors (finding 5), and App
  Intents for render, export and snapshot.
- **8j: Release plumbing.** Signing, hardened runtime, notarization, a
  DMG, and a smoke test of a release build on a clean machine.

## Not verified

- That Xcode 27.0 opens a `project.xcproj`: Apple says it does, and the
  decoder is present in 27.0's `DevToolsCore`, but it was not exercised.
- Anthropic's image-token formula for snapshot images (part A,
  finding 9).
- Apple's own statement that macOS 27 drops Intel (only secondary
  reports were retrieved).
- Whether `CADisplayLink.preferredFrameRateRange` is usable on macOS
  (the header annotates iOS and tvOS only).
- `DocumentGroup`'s autosave and versions behaviour on macOS.
- The rule that app extensions must be sandboxed (only Xcode's template
  default was seen).
- The WKWebView keyboard, IME and VoiceOver behaviour listed in §4.
