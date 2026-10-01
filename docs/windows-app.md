# The Windows app

A native Windows front end on the shared Rust core, the counterpart of the
macOS app (`apple/`): C# on WinUI 3 (Windows App SDK), Fluent controls,
Mica, the system's light or dark theme. It lives in `windows/` and, like
the macOS app, is thin: the document loop, the editor's UTF-16 edits, the
console's sentences, export formats and examples all come from `client`
through `crates/ffi` (`docs/architecture.md`, "`client` is the port
boundary"; `docs/audits/shared-core.md`).

This is **milestone 1**: one window per process that edits, previews,
renders and exports a model. Milestone 2 has begun with packaging: an
unsigned MSI per architecture, with an app icon and the `.scad`
association (see "Installer"). What it does not do yet is listed under
"Next".

## Layout

| Path | What |
|---|---|
| `windows/NeoSCAD.sln` | The solution: the four projects below |
| `windows/NeoSCAD.Bindings/` | The generated C# binding of `crates/ffi` (`Generated/neoscad_ffi.cs`, not checked in) and the core's native library for the platform, copied to every project that references it. `net10.0` |
| `windows/NeoSCAD.Host/` | Host logic that is not UI, tested on any OS: `DocumentSession` (the window's loop, text copy, dirty state, save, export), `EditorSync` and `EditorProtocol` (the editor bridge), `EditorPage` (what the editor's origin serves), `LanguageBridge` (the in-process language server), `StartupAction`, `AppLog` (the `--log` file), `PanelScale` (the viewport's display-scale arithmetic). `net10.0` |
| `windows/NeoSCAD.App/` | The WinUI 3 app: `MainWindow` (menus, panes, pickers, dialogs), `Editor/EditorHost.cs` (WebView2), `Viewport/ViewportPanel.cs` (the `SwapChainPanel`), `WinUiHost.cs` (DispatcherQueue timer and dispatcher). `net10.0-windows10.0.19041.0`, unpackaged, self-contained |
| `windows/NeoSCAD.Tests/` | xUnit tests of `NeoSCAD.Host` and of the binding against the real core. `net10.0` |
| `windows/installer/NeoSCAD.wxs` | The MSI's WiX 5 source (see "Installer") |
| `windows/NeoSCAD.App/Assets/NeoSCAD.ico` | The app icon, built by `scripts/windows/make-icon.py` and committed |
| `windows/uniffi.toml` | uniffi-bindgen-cs settings (namespace `NeoSCAD.Native`, public types, `NeoScad` for the free functions) |
| `scripts/windows/build-core.ps1` | The core's DLL, the binding and the editor bundle, before `dotnet build` |
| `scripts/windows/docker-test.sh` | The binding and host tests on Linux in Docker (from a Mac) |
| `scripts/windows/launch-screenshot.ps1` | Launch the built app with `--log`, capture its window, check it stayed up (CI) |
| `scripts/windows/build-msi.ps1` | Publish the app, stage its licences, build the MSI |
| `scripts/windows/licence-rtf.ps1` | The installer licence page's RTF (preamble, GPL, Windows App SDK licence), dot-sourced by `build-msi.ps1` |
| `scripts/windows/test-scripts.ps1` | Parse every `.ps1` here and check the licence page's RTF; any OS with pwsh, no build |
| `scripts/windows/make-icon.py` | The multi-size `.ico` from the macOS icon's art |
| `.github/workflows/windows-app.yml` | CI: build, test, launch, screenshot and log |
| `.github/workflows/windows-installer.yml` | CI, on demand and as a release publish job: build both MSIs, install, check, launch, uninstall; in a release, attest and attach them |

## Build on Windows

Needs Rust (rustup with the MSVC build tools), Node 18+ and the .NET 10
SDK; the WebView2 runtime (part of Windows 11, and of Edge on Windows 10).

    pwsh scripts/windows/build-core.ps1            # -Arch arm64 on ARM
    dotnet test windows/NeoSCAD.Tests/NeoSCAD.Tests.csproj -c Release
    dotnet build windows/NeoSCAD.App/NeoSCAD.App.csproj -c Release -r win-x64 -p:Platform=x64
    windows\NeoSCAD.App\bin\x64\Release\net10.0-windows10.0.19041.0\win-x64\NeoSCAD.exe [FILE | --example ID] [--log LOGFILE]

Or open `windows/NeoSCAD.sln` in Visual Studio 2022+ with the "WinUI
application development" workload, after `build-core.ps1`.

`build-core.ps1` runs `cargo rustc -p neoscad-ffi --crate-type cdylib`
(the crate's own crate types stay `staticlib` + `lib`, so macOS builds do
not link a dylib), puts `neoscad_ffi.dll` in `windows/native/<rid>/`,
generates the binding from that DLL, and builds the CodeMirror bundle in
`apple/Editor/web/dist`, which the app copies to `Editor\` beside the
executable.

## Installer

`pwsh scripts/windows/build-msi.ps1 [-Arch x64|arm64]`, after
`build-core.ps1`, writes `dist/windows/NeoSCAD-<version>-windows-<arch>.msi`:
an unsigned, per-machine MSI for each architecture. It runs `dotnet
publish` (self-contained), stages the licence files beside the app, and
runs `wix build` on `windows/installer/NeoSCAD.wxs`.

- **WiX 5.0.2**, a .NET tool the script installs into `dist/windows/tools`.
  The CLI's MSI uses WiX 3.14.1 through cargo-dist, but the app is a
  few hundred files in nested folders. WiX 5's `Files` element harvests
  a folder at build time; it is absent from WiX 4.0.5 and present in 5.0.2
  (`HarvestFilesCommand.cs` in wixtoolset/wix at `v5.0.2`). WiX 3 would need
  `heat.exe` plus an XSLT to keep the exe out of the harvest, since the exe
  carries the shortcut. WiX 6 and later require accepting the Open Source
  Maintenance Fee EULA (the wixtoolset/wix README at `v6.0.0`; not at
  `v5.0.2`). The tool comes from NuGet on x64 and arm64 alike, with no
  hashed zip download (`.github/build-setup.yml`).
- **MSI, not MSIX.** An MSIX installs only if it is signed with a
  certificate the PC trusts, and the owner does not buy Authenticode
  certificates (`docs/release.md`, "Windows is unsigned"). Double-clicking
  the MSI shows SmartScreen's warning, then UAC names an unknown
  publisher.
- **Per-machine**, into `Program Files\NeoSCAD`. Only administrators can
  write there, so the .NET and Windows App SDK runtime inside the app can't
  be changed by a user-level process. The CLI's MSI (`InstallScope='perMachine'`)
  and OpenSCAD's own installer (which registers `.scad` under `HKCR`,
  `cmake/nsis/mingw-file-association.nsh:159` in the reference checkout)
  are per-machine too. The cost is the UAC prompt.
- **One page: the licence.** `WixUI_Minimal` (WiX's UI extension) shows
  a licence agreement, then Install. `build-msi.ps1` writes its RTF:
  - a preamble;
  - NeoSCAD's GPL;
  - the Windows App SDK's licence, taken from the package that was built with.

  `scripts/windows/licence-rtf.ps1` writes it. Everything outside
  printable ASCII becomes an RTF escape (`\uN?`), and CRLF from a Windows
  checkout is folded. `scripts/windows/test-scripts.ps1` checks the result
  on any OS: all three sections present, escapes, balanced braces. The
  workflow runs it before building. A silent install (`msiexec /qn`) shows no pages.
- **Upgrades.** The `UpgradeCode` (`1A80E3D5-…`, not the CLI's) is fixed.
  Every build has a new ProductCode, so installing any other build is a
  major upgrade that removes the old one first. A downgrade is refused.
  The ProductVersion is only the numeric `x.y.z`, so a release and its
  rcs (0.2.0-rc.1, 0.2.0) share one version; `<MajorUpgrade
  AllowSameVersionUpgrades="yes">` makes a same-version build replace
  the installed one instead of installing beside it as a second product
  (WiX's default). MSI can't tell those builds apart, so whichever is
  installed last wins, an rc after its release included. The setting
  would draw ICE61's warning, but `wix build` runs no ICE validation
  (that is `wix msi validate`, which the build doesn't run).
- **Version.** `Cargo.toml`'s `[workspace.package] version` sets the MSI's
  ProductVersion (its numeric part, `-d Version=`) and the exe's version.
  `windows/Directory.Build.props` reads the same line for `Version`,
  `FileVersion` (`0.1.1.0`) and `AssemblyVersion`.
- **Start menu:** an advertised shortcut, `NeoSCAD`, with the app icon.
- **Icon.** `NeoSCAD.ico` has 16 to 64 px as 32-bit DIBs and 256 px as
  PNG. It is cropped to the ring from `apple/App/AppIcon.icon/Assets/art.png`,
  because that layer is framed at 72% for macOS's mask. The icon is set on
  the exe (`ApplicationIcon`), on the window (`AppWindow.SetIcon`, from
  `Assets\NeoSCAD.ico` beside the exe) and on the uninstall entry
  (`ARPPRODUCTICON`).
- **`.scad`.** The ProgId `NeoSCAD.scad` ("OpenSCAD model") uses the exe's
  icon and opens `"NeoSCAD.exe" "%1"`, which `StartupAction.Parse` takes as
  the file to open. NeoSCAD is listed under `.scad\OpenWithProgids` and
  `Applications\NeoSCAD.exe\SupportedTypes`, so it appears in "Open with"
  whatever the default is. `.scad`'s default value is set only when it is
  empty or already `NeoSCAD.scad`, so an OpenSCAD association is never
  taken over, and uninstalling can't remove one. A choice the user
  made in Windows (`UserChoice`) wins over all of this. Not registered:
  `App Paths` (the Run box would then start the app for `neoscad`, the
  CLI's name) and Default Apps capabilities.
- **Licences.** The MSI installs `LICENSE`, `NOTICE` and
  `packaging/licenses/` beside the app. It also installs
  `licenses/third-party/<package>-<version>/`: the licence and notice
  files of every NuGet package the app was restored from, taken from
  `obj/project.assets.json`. That covers the .NET runtime pack (MIT, with
  its third-party notices) and the Windows App SDK (Microsoft's licence
  and its `NOTICE.txt`), both redistributed inside the self-contained app.
  The script fails if either is missing. See "Licence questions" below.
- **WebView2** is not bundled. It is part of Windows 11 and of current
  Windows 10. Before starting the editor, the app asks
  `CoreWebView2Environment.GetAvailableBrowserVersionString()`
  (`NeoSCAD.Host/WebViewRuntime.cs`). If no runtime is installed, the
  editor pane explains, and a dialog offers "Download WebView2", which
  opens <https://developer.microsoft.com/microsoft-edge/webview2/>. The
  rest of the window works without it.

**The Windows App SDK's terms.** Its licence (`license.txt` in the
`Microsoft.WindowsAppSDK` package, section 3) allows redistributing the
files it binplaces, including in a self-contained app. It also requires
distributors to "require distributors and external end users to agree to
terms that protect it and Microsoft at least as much as this agreement".
The installer's licence page is how users agree (owner decision,
2026-09-30). It presents the SDK's licence alongside the GPL, and the
agreement covers those components.

## Bindings

The binding is generated by
[uniffi-bindgen-cs](https://github.com/NordSecurity/uniffi-bindgen-cs) in
library mode from the built core, as the Swift one is by
`crates/uniffi-bindgen`, and is not checked in.

**Version.** `crates/ffi` pins `uniffi = "=0.32.2"` for the Swift binding.
The latest uniffi-bindgen-cs release, `v0.11.0+v0.31.0`, targets 0.31
(checked with `gh api repos/NordSecurity/uniffi-bindgen-cs/releases`,
2026-09-30), and a 0.31 generator does not load a 0.32 library (its
PR #176 says method checksums changed in 0.32). Pinning `ffi` back to 0.31
would regenerate the Swift binding under the macOS app. Instead the build
pins the generator to the head of
[NordSecurity/uniffi-bindgen-cs#176](https://github.com/NordSecurity/uniffi-bindgen-cs/pull/176),
"Upgrade to uniffi-rs 0.32.0" (open, mergeable, from
`dennisameling/uniffi-bindgen-cs`, commit `0fc022aa`), in
`build-core.ps1`'s `$BindgenRev`. Built against uniffi 0.32.0, it reads
the 0.32.2 core: the contract version and checksums agree (the tests
below call every kind of export). When #176 is released, switch
`$BindgenRepo`/`$BindgenRev` to the upstream tag.

**Two generator quirks the app works around** (found by the tests, both in
`windows/NeoSCAD.Tests/CoreTests.cs`):

- *Constructor overloads.* Every object gets a `public T(ulong pointer)`
  constructor that wraps a raw Rust pointer. `DocumentController`'s own
  constructor takes `ulong? delayMs`, so `new DocumentController(150)`
  binds to the pointer overload and the first call crashes the process.
  Pass `null` (the core's default) or cast: `new DocumentController((ulong?)150)`.
- *Empty-list defaults.* `#[uniffi(default = [])]` record fields
  (`DocumentRequest.overrides`, `.enable`) come out as `= null`, which the
  converters would dereference. Build such records with explicit arrays;
  `DocumentSession.Run` does.

Errors arrive as `CoreException.Failed` and friends, whose `Message` reads
`@message=…`; `CoreErrors.Describe` takes the field instead.

## Architecture

The same shape as the macOS app, with Windows parts:

| | macOS | Windows |
|---|---|---|
| Shell | SwiftUI/AppKit, NSDocument | WinUI 3, one `MainWindow` per process, `DocumentSession` |
| Document loop | `DocumentLoop.swift` + core `DocumentController` | `DocumentSession` + the same controller; `DispatcherQueueTimer` for the 150 ms pause |
| Editor | CodeMirror bundle in WKWebView, `neoscad-editor:` scheme handler | the same bundle in WebView2, served from `https://app.neoscad.example` and answered by `WebResourceRequested` (see "Why not the scheme") |
| Editor messages | `webkit.messageHandlers.editor` | `window.NeoSCADHost` (set by a document-created script) over `chrome.webview.postMessage` |
| 3D view | wgpu Metal into a `CAMetalLayer` | wgpu Direct3D 12 into a `SwapChainPanel` |
| Language features | `crates/lsp` in-process | the same, `LanguageBridge` |
| Dirty state | NSDocument change count | edits minus undos; title `*name - NeoSCAD` |

**The viewport surface.** `Viewport::attach_swap_chain_panel(panel, w, h,
scale_x, scale_y, readable)` (`crates/ffi/src/viewport.rs`) makes a wgpu surface from
the panel's `ISwapChainPanelNative` pointer
(`wgpu::SurfaceTargetUnsafe::SwapChainPanel`); `crates/ffi/src/layer.rs`
states the pointer contract, and `host.rs` opens the GPU on DX12 on
Windows (Metal on Apple). The app takes the pointer from CsWinRT's native
object reference with `QueryInterface` for IID
`63aad0b8-7c24-40ff-85a8-640d944cc325`, the WinUI 3 interface wgpu-hal
declares, and draws on `CompositionTarget.Rendering` when
`needs_draw()`. A child HWND was the other option; a `SwapChainPanel`
composes with XAML (rounded corners, Mica, overlays) and has no airspace
problem.

**Display scaling.** A `SwapChainPanel` shows its swap chain one buffer
pixel per DIP unless told otherwise, so a swap chain sized in DIPs is
stretched (blurry at 150%) and one sized in pixels overflows the panel.
The app passes the panel's size in DIPs with its `CompositionScaleX` and
`CompositionScaleY`, on attach and on every `SizeChanged` and
`CompositionScaleChanged` (`resize_swap_chain_panel`). The core sizes the
swap chain in physical pixels (DIPs times scale, truncated) and, after
every configure of the surface, sets the ratio of DIPs to buffer pixels
on it with `IDXGISwapChain2::SetMatrixTransform`: about the inverse
scale, exact when the buffer was rounded or clamped to the largest
texture. "Every configure" matters because wgpu-hal's configure can make
a new swap chain; attaching, resizing and the reconfigures in
`render::viewport::Viewport::draw` all run the hook
(`render::viewport::OnConfigure`, installed by
`attach_surface_with`). The swap chain is reached through
`wgpu::Surface::as_hal::<Dx12>()` and wgpu-hal's
`dx12::Surface::swap_chain()`; the DXGI call is `layer.rs`'s
`set_swap_chain_scale`, with its safety notes. The `windows` crate it
uses is the version wgpu-hal already builds. The renderer's one scale is
`CompositionScaleX`, so pointer deltas stay in DIPs across and are scaled
by `scaleY / scaleX` down (`PanelScale.PointerDelta`; the same when the
two agree, as they do without a stretching transform above the panel).
With `--log`, the log has the DIP size, both scales, the swap chain's
pixels and the transform set (or the error `SetMatrixTransform` met,
from `Viewport::swap_chain_transform`) on attach and on every scale
change.

**The editor bridge** is the macOS protocol unchanged (documented at the
top of `apple/App/Editor/EditorController.swift` and in
`NeoSCAD.Host/EditorProtocol.cs`): `ready`, `changes` with
`base`/`version`, `command`, `lsp`, `open`, `log`; the host calls
`NeoSCADEditor.load/text/lspReceive/lspSync` with every argument a JSON
literal. The page's Content-Security-Policy header is the macOS app's.

**Threads.** Core calls that evaluate go to the thread pool
(`CoreService.Run`); results come back through the `DispatcherQueue`, and
a result is shown only while `DocumentController.IsCurrent` holds. Quick
document calls (`update`, `edit`, `close`) stay on the UI thread so edits
reach the session in order.

## Diagnostics

`--log FILE` appends one timestamped line per start-up event
(`NeoSCAD.Host/AppLog.cs`): the WebView2 environment and
`CoreWebView2Initialized` (with its exception), each navigation
(`NavigationStarting`, `ContentLoading`, `NavigationCompleted` with its
`WebErrorStatus`), each request the editor's origin answers, the
language server's messages and failures, an external-scheme launch, `ProcessFailed`, the page's script errors, rejected promises
and policy violations (reported by `EditorPage.HostScript`), the
editor's `ready`, the view's attach, the first render result, a UI-thread
heartbeat for the first 30 s, and unhandled exceptions. Lines are
flushed as written, so a killed process keeps them.

CI runs `scripts/windows/launch-screenshot.ps1`: it launches the app on
the default example (`csg`) with `--log`, sizes its window to 1400×900,
captures that window alone with `PrintWindow(PW_RENDERFULLCONTENT)` (a
screen grab showed the runner's console over it on x64, and the first-run
privacy screen over everything on `windows-11-arm`), prints the log into
the job output, and uploads the PNG and the log as
`neoscad-windows-<arch>-screenshot`.

## Testing off Windows

`scripts/windows/docker-test.sh` builds the core as a Linux `.so` and the
binding in `rust:<pinned toolchain>`, then runs `dotnet test` on
`NeoSCAD.Tests` in `mcr.microsoft.com/dotnet/sdk:10.0` against it: the
binding's checksums, records, objects, a C#-implemented observer, the
UTF-16 edits, and the document loop end to end (a pause runs a preview
whose console reaches the session; save; STL export). The WinUI project
needs Windows.

The Rust side of the Windows path is checked with

    cargo clippy --target x86_64-pc-windows-msvc -p neoscad-ffi --all-targets --no-default-features -- -D warnings

in `rust:1.98.1` (`--no-default-features` turns off `ffi`'s new
`mimalloc` feature, whose C sources need MSVC, as for `neoscad-cli`).

## What milestone 1 covers

- File: New, Open, Save, Save As (`FileOpenPicker`/`FileSavePicker`),
  Examples (the core's `examples()`), Export STL, Export Image (the view
  as PNG, `Viewport::image`), Exit; a save prompt before discarding edits.
- Design: Preview (F5), Render (F6), also from the editor's own keys.
- View: View All, Reset View, the seven standard views; orbit (left
  drag), pan (right or middle drag), zoom (wheel).
- Live preview 150 ms after typing pauses; the console (errors and
  warnings coloured); the status line (`describe_render`); editor markers
  from the language server; title with the dirty state; light and dark
  following the system (the view switches between Cornfield and Tomorrow
  Night and re-runs, as on macOS); Mica.

## Verified, and not

Verified on the Mac: the Rust changes (fmt, clippy, workspace tests,
the macOS app's build and tests, wasm-check), the Windows cross-clippy of
`neoscad-ffi`, the binding generated from the macOS and Linux builds of
the core, the tests above on linux-arm64, and the app's C# type-checked
against Windows App SDK 2.5.1 in the Linux .NET SDK (with stand-ins for
the XAML-generated fields; the XAML compiler and PRI tools only run on
Windows). `actionlint` passes on the workflow.

Verified in CI (September 2026, `windows-2025` x64 and `windows-11-arm`):
- the app builds and launches;
- WebView2 serves the editor, and the text loads without CRs;
- the language server answers;
- the `SwapChainPanel` draws with DX12, and the swap-chain transform is set;
- the csg example previews.

Screenshots and logs are the run's artifacts.

Not verified, because the runners run at 100% scaling with no real
display:
- that the view is sharp above 100% (see "Display scaling");
- keyboard accelerators while WebView2 has focus;
- file dialogs and export through the UI.

**The installer** (September 2026) was checked off Windows only; no MSI
has been built yet:
- `scripts/windows/make-icon.py` ran on the Mac, and its `.ico` decodes to
  the eight sizes, drawn and looked at;
- `docker-test.sh` passes 58 of 58 tests, the new `WebViewRuntimeTests`
  among them;
- the app's C# type-checks against Windows App SDK 2.5.1 in the Linux
  .NET SDK, with stand-ins for the XAML fields; an unknown member in
  that build fails it, so the check is real;
- MSBuild evaluates `Version` 0.1.1 and `FileVersion` 0.1.1.0 from
  `Cargo.toml`;
- `build-msi.ps1`'s licence staging ran under pwsh on a Linux restore
  of the app for `win-x64`;
- PowerShell parses `build-msi.ps1` and the new workflow's scripts;
- `NeoSCAD.wxs` is well-formed XML, and WiX 5.0.2's compiler on Linux
  raised no schema errors. It did stop at two path errors that look like
  Linux artefacts: WiX warns that it "only supports Windows" and
  that behaviour after the warning is undefined;
- `actionlint` 1.7.12 passes on the workflow.

Unverified until `windows-installer.yml` runs: the MSI build itself (ICE
validation included), install, the shortcut, the uninstall entry, the
association, launching from `.scad`, and uninstall.

### Why not the scheme

The macOS and Linux apps serve the bundle from `neoscad-editor://app/`.
WebView2 under WinUI 3 never raised `WebResourceRequested` for that
scheme, even though it was registered with `CoreWebView2CustomSchemeRegistration`. Every
navigation ended `ConnectionAborted` with no request seen, for every
filter pattern and source kind tried. So Windows serves an `https` origin
the app answers itself, as Microsoft's own guidance for local content
does. `.example` is reserved (RFC 2606), so the name resolves nowhere.
The page's `<meta>` policy names the other apps' scheme; `EditorPage`
rewrites it to `script-src 'self'` when serving.

## Next (milestone 2)

- Check display scaling on real hardware (see "Display scaling"): the
  change was written and cross-checked off Windows, never run. Look for
  a crisp model at 125%, 150% and 200%, a frame that fills the panel
  exactly, moving the window between monitors of different scales, and
  the `view: attached` / `view: rescaled` lines in the `--log` file.
- The installer (see "Installer"): run `windows-installer.yml` and look
  at its screenshots. It is a release publish job (`docs/release.md`,
  "The cross-platform release"), not yet run in a release. Settle the
  licence questions above.
  Check by hand what CI can't: SmartScreen and UAC on a downloaded MSI,
  the Explorer icon of a `.scad` file, and the no-WebView2 dialog on a
  Windows 10 without the runtime.
- Multiple windows (one `DocumentSession` each), recent files, autosave.
- The panels: customizer, check, measure (all in `client` already).
- File watching (`FileSystemWatcher` on the run's `files`, into
  `DocumentController.FilesChanged`).
- Keyboard shortcuts from inside the editor (forward WebView2's
  accelerator keys), Edit menu (undo/redo/find through `NeoSCADEditor`),
  editor font size, go-to-definition opening files, library viewer.
- A UI smoke test (WinAppDriver or UI Automation) replacing the
  screenshot; make the CI job blocking.
- Move `$BindgenRev` to an upstream uniffi-bindgen-cs release.
