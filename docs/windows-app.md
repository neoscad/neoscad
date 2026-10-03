# The Windows app

A native Windows front end on the shared Rust core, the counterpart of the
macOS app (`apple/`): C# on WinUI 3 (Windows App SDK), Fluent controls,
Mica, the system's light or dark theme. It lives in `windows/` and, like
the macOS app, is thin: the document loop, the editor's UTF-16 edits, the
console's sentences, export formats and examples all come from `client`
through `crates/ffi` (`docs/architecture.md`, "`client` is the port
boundary"; `docs/audits/shared-core.md`).

**Milestone 1** is one window per process that edits, previews,
renders and exports a model. **Milestone 2** added packaging (an
unsigned MSI per architecture, with an app icon and the `.scad`
association; see "Installer") and the panels: the customizer, check and
measure, file watching, every export format the core offers, and the
menu's shortcuts inside the editor (see "What milestone 2 covers"). What
it does not do yet is listed under "Next".

## Layout

| Path | What |
|---|---|
| `windows/NeoSCAD.sln` | The solution: the four projects below |
| `windows/NeoSCAD.Bindings/` | The generated C# binding of `crates/ffi` (`Generated/neoscad_ffi.cs`, not checked in) and the core's native library for the platform, copied to every project that references it. `net10.0` |
| `windows/NeoSCAD.Host/` | Host logic that is not UI, tested on any OS: `DocumentSession` (the window's loop, text copy, dirty state, save) and `DocumentSession.Panels.cs` (the customizer, check, measure, the view's overlay, export), `FileWatch` (the run's files on disk), `Shortcuts` (the chords the editor page forwards), `EditorSync` and `EditorProtocol` (the editor bridge), `EditorPage` (what the editor's origin serves, and the page's key script), `LanguageBridge` (the in-process language server), `StartupAction`, `AppLog` (the `--log` file), `PanelScale` (the viewport's display-scale arithmetic), `Updates` (the update check, the MSI's download and the install helper; see "Updates"). `net10.0` |
| `windows/NeoSCAD.App/` | The WinUI 3 app: `MainWindow` (menus, panes, pickers, dialogs; `MainWindow.Updates.cs` the update bar and Help menu), `Panels/` (`CustomizerPanel`, `CheckPanel`, `MeasurePanel`, built in code), `Editor/EditorHost.cs` (WebView2), `Viewport/ViewportPanel.cs` (the `SwapChainPanel`), `WinUiHost.cs` (DispatcherQueue timer and dispatcher). `net10.0-windows10.0.19041.0`, unpackaged, self-contained |
| `windows/NeoSCAD.Tests/` | xUnit tests of `NeoSCAD.Host` and of the binding against the real core. `net10.0` |
| `windows/installer/NeoSCAD.wxs` | The MSI's WiX 5 source (see "Installer") |
| `windows/NeoSCAD.App/Assets/NeoSCAD.ico` | The app icon, built by `scripts/windows/make-icon.py` and committed |
| `windows/uniffi.toml` | uniffi-bindgen-cs settings (namespace `NeoSCAD.Native`, public types, `NeoScad` for the free functions) |
| `scripts/windows/build-core.ps1` | The core's DLL, the binding and the editor bundle, before `dotnet build` |
| `scripts/windows/docker-test.sh` | The binding and host tests on Linux in Docker (from a Mac) |
| `scripts/windows/docker-typecheck.sh` | The WinUI app's C# compiled against the Windows App SDK in Docker, with `xaml-standins.py` in place of the XAML compiler (see "Testing off Windows") |
| `scripts/windows/launch-screenshot.ps1` | Launch the built app with `--log` (and `-Panel`, a side panel open), capture its window, check it stayed up (CI) |
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
    windows\NeoSCAD.App\bin\x64\Release\net10.0-windows10.0.19041.0\win-x64\NeoSCAD.exe [FILE | --example ID] [--log LOGFILE] [--panel customizer|check|measure]

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
publish` (self-contained), builds the `neoscad` CLI into the app's
`bin\` (for AI agent clients; `docs/release.md`, "The Windows app in the
release"), stages the licence files beside the app, and runs `wix build`
on `windows/installer/NeoSCAD.wxs`.

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

## Updates

The app installs new releases itself (owner decisions of 2026-09-30,
`docs/audits/auto-update.md`): "Update available", then Install, then a
silent `msiexec` after one UAC prompt, and the app starts again on the
new version. `NeoSCAD.Host/Updates.cs` has the logic, tested off
Windows; `MainWindow.Updates.cs` the bar and the menu.

- **The check.** Ten seconds after start-up and then hourly, each time
  only when a day has passed since the last check, the app GETs
  `https://neoscad.org/updates/v1/stable.json` (or `rc.json`) and its
  `.minisig` (`update_feed_url`) with `HttpClient`: no cookies, the
  User-Agent `neoscad`, at most 64 KiB each. The core's
  `check_for_update`, the code the CLI and the Linux app use, verifies the
  minisign signature against the keys compiled into the core, the
  channel, the serial (kept per channel in the settings, so an old
  signed feed can't be replayed) and the version, and offers only a
  release with this architecture's MSI (`WindowsX64` or `WindowsArm64`,
  from `RuntimeInformation.ProcessArchitecture`). A build with no key
  (`update_check_available()` false, every build until the release key
  exists) makes no request. Automatic checks are silent; failures go to
  the `--log` file as `update:` lines. Help > Check for Updates… says
  what it found.
- **The bar.** An `InfoBar` under the menu: "NeoSCAD x.y.z is available"
  with Install. Closing it is "Later": automatic checks don't show that
  version again (the menu's check does). A copy that isn't the MSI's
  (not in `%ProgramFiles%\NeoSCAD`, such as a build folder) gets Release
  Page instead of Install, since the MSI would not replace it.
- **Install.** After the usual save prompt, the MSI is downloaded into a
  new folder under `%TEMP%`, refused (and deleted) if it is larger or
  smaller than the signed feed says or its SHA-256 differs. The app then
  starts a hidden, unelevated Windows PowerShell (`-EncodedCommand`,
  which the execution policy doesn't govern) and closes. The helper
  waits for the app's process to end (an installer can't replace files
  a running app holds), checks the hash again, runs
  `msiexec /i <msi> /qn /norestart /l*v install.log` with `-Verb RunAs`
  (the one UAC prompt), and starts the app again, the new version if
  the install worked and the old one if the prompt was declined or the
  install failed. Staying unelevated is what keeps the restarted app
  from running as administrator. `MajorUpgrade` with
  `AllowSameVersionUpgrades` (see "Installer") replaces the old version,
  an rc by its release included.
- **Settings.** Help > "Check for Updates Automatically" (on by default)
  and "Receive Release Candidates" (off). Switching the channel checks
  the other feed at once. They live with the serials in
  `%LOCALAPPDATA%\NeoSCAD\updates.json`.
- **Off switches and testing.** `NEOSCAD_NO_UPDATE_CHECK` or `CI` set
  stops automatic checks (CI's launch test never checks).
  `NEOSCAD_UPDATE_FEED_URL` points at another feed directory (https, or
  http to the loopback address); the signature is still checked against
  the core's keys, so a test feed needs a core built with
  `NEOSCAD_UPDATE_TEST_PUBLIC_KEY` set to a throwaway key's public line.
  `docker-test.sh` passes that variable through; set to the line of
  `crates/client/testdata/update/test.pub`, `UpdateTests` checks the
  signed fixtures against the real core.

Checked off Windows only (October 2026): `docker-test.sh` passes 93 of
93 on linux-arm64, with and without the test key (`UpdateTests`: the
settings, the feed address, the request carrying nothing identifying,
the core accepting the fixtures and refusing a feed signed by another
key, an edited feed and a replayed serial, a download refused for a
wrong size, checksum, scheme or name, and the helper's script); the
host's `UpdateClient` with a real `HttpClient` against a loopback feed
signed with a throwaway key offered the x64 MSI and refused the same
feed signed by another key and edited after signing; and
`docker-typecheck.sh` compiles the bar and the menu. Not run until CI or
a Windows machine does: `TheInstallerScriptParsesInWindowsPowerShell`
(skipped off Windows), the `InfoBar`, the download in the app, UAC,
`msiexec /qn` over an installed copy, and the restart
(`docs/followups.md`, "Windows").

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

**The panels** sit in a `SplitView` pane on the window's right
(`DisplayMode="Inline"`, so the editor and view narrow rather than being
covered), with a `SelectorBar` for Customizer, Check and Measure and a
close button. View > Customizer (Ctrl+Shift+P), Check Panel and Measure
Panel toggle it; Design > Check (Ctrl+Shift+K) and Measure
(Ctrl+Shift+M) open their panel and run at once, as the macOS menu
does. The panels are built in code from the core's records rather than
in XAML, since what they show is data.

- *Customizer.* After every run (and on loading a text) the session
  reads `Core.parameters` and gives each parameter its Fluent control:
  `Slider` with a `NumberBox` for a range, a `NumberBox` with spin
  buttons for a number, `ToggleSwitch`, `TextBox`, `ComboBox`, a row of
  `NumberBox`es for a vector, in an `Expander` per group. Every edit goes
  through the core's `edit_parameter` (snapped, clamped, cut to length;
  the text's own value is no override) into the `DocumentController`'s
  values, and the document previews again with them. **The text is not
  edited**: OpenSCAD's customizer passes values as `-D`-style
  assignments after the text, and so do all three apps. The controls are
  rebuilt only when the parameters change (`ParameterShapes`), so a field
  keeps its focus while values update. Parameter sets are OpenSCAD's
  `name.json` beside the model (`apply_parameter_set`,
  `save_parameter_set`); an untitled document has none.
- *Check.* `Core.check` with the printer's `CheckOptions`
  (`printer_check_options`, presets from `printer_presets`) and the
  customizer's values, detached from the document loop. Findings list
  severity, message, fix and part; selecting one sets the view's overlay
  (`Viewport.set_overlay`: every finding's numbered marker, the selected
  one's box) and turns the view to its point. "Check after each render"
  runs it after F6, not after previews (a check renders too).
- *Measure.* `Core.measure`: volume, area, size, centre of mass,
  pieces and parts. With "Pick points" on, a click on the view (a left
  press that moves under 4 DIPs; it still orbits by that much) casts
  `Viewport.ray_at` into `Measurement.pick`; two picks give
  `pick_distance`, both drawn in the overlay. A third starts a new pair.

**File watching.** After each run, `FileWatch` watches the run's `files`
(includes, uses, imports; the core leaves out the document itself and
other open documents) with one `FileSystemWatcher` per folder, so an
editor that saves by renaming a temporary file over the original is
still seen. A burst of events becomes one `DocumentController.files_changed`
100 ms later: the last render again, or a preview after the pause.

**Export.** File > Export As lists `export_formats()` (binary and ASCII
STL, 3MF, OBJ, OFF, SVG, DXF, PDF, the view as PNG, a snapshot sheet);
a 3D model's formats are disabled after a 2D render and the other way
round. Export… (Ctrl+Shift+E) repeats the last format, or the one that
suits the model (`suggest_export_format`). Geometry goes through
`Core.export_file` with the customizer's values and a `CancelToken`; a
`ContentDialog` shows the stage (`ProgressListener`) with Cancel, after
400 ms so a quick export does not flash one. The core writes through a
temporary file, so a cancelled or failed export leaves the old file. The
core has no AMF writer, so neither does the menu.

**Shortcuts in the editor.** A WinUI `KeyboardAccelerator` sees only
keys that reach XAML; keys typed in WebView2 do not, and WinUI 3's
`WebView2` does not expose the controller's `AcceleratorKeyPressed`.
So the page forwards them, as the bundle already does F5 and F6: a
second document-created script (`EditorPage.KeyScript`) catches the
chords in `Shortcuts.Forwarded` in the capture phase, before
CodeMirror's keymap, and posts the protocol's `command` message. The
menu's chord wins over an editor binding of the same chord, as on macOS
(Ctrl+Shift+K checks rather than deleting a line). A test checks every
forwarded chord is also a menu accelerator. The Edit menu (Undo, Redo,
Select All, Find) calls `NeoSCADEditor.undo()` and friends for a mouse
click; its keys are CodeMirror's own, so its items only show them
(`KeyboardAcceleratorTextOverride`): a live Ctrl+Z accelerator would
undo the editor while the focus is in a customizer field.

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
`neoscad-windows-<arch>-screenshot`. It then does the same on the
`box-lid` example (it has customizer parameters) with `--panel
customizer`, as `neoscad-windows-<arch>-customizer.png`, so the pane is
seen rendering with real controls.

## Testing off Windows

`scripts/windows/docker-test.sh` builds the core as a Linux `.so` and the
binding in `rust:<pinned toolchain>`, then runs `dotnet test` on
`NeoSCAD.Tests` in `mcr.microsoft.com/dotnet/sdk:10.0` against it: the
binding's checksums, records, objects, a C#-implemented observer, the
UTF-16 edits, and the document loop end to end (a pause runs a preview
whose console reaches the session; save; STL export). The WinUI project
needs Windows to build.

`scripts/windows/docker-typecheck.sh` (after `docker-test.sh`, which
generates the binding) compiles the app's C# against the Windows App
SDK's reference assemblies in the same .NET container, as a library with
`EnableWindowsTargeting`. The XAML compiler and `MakePri.exe` only run
on Windows, so `scripts/windows/xaml-standins.py` writes what the XAML
compiler would: a field per `x:Name`, `InitializeComponent`, and every
handler the XAML names subscribed to its event, so a misnamed handler or
a wrong signature fails the build (tried: `Click="OnPanelTab"` fails with
CS0123). It does not check that the XAML loads, or its property names.

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

## What milestone 2 covers

- The customizer, check and measure panels (see "Architecture", "The
  panels"), with the overlay in the view.
- File watching of the run's files.
- File > Export As: every format the core offers, with progress and
  Cancel; Export… (Ctrl+Shift+E) for the last one.
- The menu's shortcuts while the editor has the focus; an Edit menu.
- `--panel NAME` to open a panel at start.

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

**The panels** (September 2026) were checked off Windows only:
- `docker-test.sh` passes 76 of 76 tests on linux-arm64, 18 of them new
  (`PanelTests.cs`) against the real core: a run reads the parameters
  and an edit previews with the override while the text stays as it was;
  a parameter set saved beside the model is applied again; a check
  finds a thin wall, uses the customizer's values, and a selected
  finding reaches the overlay (`view_overlay`); measure gives a cube's
  volume and two picks its height; changing an included file schedules
  a preview, and a watch coalesces a burst and survives a
  rename-over-save; export writes 3MF, OFF, OBJ and STL with their
  stages and the customizer's values, SVG for 2D, refuses STL for 2D,
  and a cancelled export writes nothing; every forwarded chord is a menu
  accelerator in `MainWindow.xaml`;
- `docker-typecheck.sh` compiles the app (warnings as errors) against
  Windows App SDK 2.5.1;
- `test-scripts.ps1` parses `launch-screenshot.ps1`; `actionlint`
  1.7.12 passes on `windows-app.yml`.

Not verified until CI or a Windows machine runs it: the XAML loading
(the `SplitView`, `SelectorBar`, the menu's `KeyboardAcceleratorTextOverride`),
how the panels look, slider dragging, picking in the view, the export
dialog, the forwarded keys in WebView2, and `FileSystemWatcher` on NTFS
(the test ran on Linux's inotify).

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
- Look at the panels on CI's screenshot and on a real machine (see
  "Verified, and not"), and try the shortcuts in the editor.
- Multiple windows (one `DocumentSession` each), recent files, autosave.
- What the macOS panels have and these lack (`docs/followups.md`,
  "Windows"): sections and part distances in measure, the bed and a
  stored printer in check, 3MF colour options, the parts toggle.
- Editor font size, go-to-definition opening files, library viewer.
- A UI smoke test (WinAppDriver or UI Automation) replacing the
  screenshot; make the CI job blocking.
- Move `$BindgenRev` to an upstream uniffi-bindgen-cs release.
