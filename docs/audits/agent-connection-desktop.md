# Audit: connecting an AI agent to the desktop apps

Scope: how the macOS app (`apple/`), the Linux app (`crates/linux-app`)
and the Windows app (`windows/`) let a user connect an AI agent, against
what the web demo's "Connect your AI agent" does (`web/src/ui/agent.js`,
`web/src/agent/`, `crates/cli/src/mcp/bridge.rs`,
`crates/cli/src/mcp/tools/browser.rs`, `docs/agent-bridge.md`), and a
design that gets the apps to "it just works". Read 2026-10-02 at
`efb0e22`. Nothing was built or run: a timed benchmark had the machine.
Everything about the apps comes from their source and docs. Outside
claims were checked against the URLs cited, on that date.

The owner's direction for the recommendation: put ease, UX and
performance on the desktop first, whatever the implementation effort;
get as close to zero setup as each platform allows; no manual JSON; the
connection survives restarts; a live view of what the agent is doing;
edits the user can see and undo; nothing that costs anything while idle.

## Summary

- **None of the three apps has any agent UI.** There is no menu item,
  button, setting or help link. An agent reaches a model only through
  `neoscad mcp` on files on disk, and none of the apps ships that CLI.
- **Today, on Linux and Windows, an agent's edit to the open file is
  invisible and is lost on the next save.** Both apps leave the
  document out of their file watching and save without looking at the
  disk. This is the most important finding, and it needs fixing
  whatever is decided about the agent UI.
- **The web bridge's work mostly carries over.** The shared editor
  bundle already has `agentEdit`, which applies an edit as one
  highlighted undo step. The render crate already has `copy_as_shown`
  and annotations. The CLI already has the tools, and per-user sockets
  with ownership checks (`transport.rs`).
- **Recommendation:** bundle the CLI in each app. Have each app listen
  on a per-user local socket, not TCP, which plain `neoscad mcp` finds
  by itself: no flag, token or link. Add a "Connect your AI agent"
  sheet that sets up each client in one click, through the client's own
  install link or command. The open-file fixes and the sheet ship
  first; the live bridge follows.

## Firm ground: what the code does today

1. **No agent UI in any app.**
   - macOS: the menus are built in code (`apple/App/MainMenu.swift`).
     The app menu has About, Check for Updates, Settings, Services,
     Hide and Quit (`:41-65`). The Help menu is created empty
     (`:19-21`). The document window has no `NSToolbar`
     (`apple/App/Document/SCADDocument.swift:291-307`: a plain
     `NSWindow`, and the only toolbars are SwiftUI rows inside panels).
     The Settings window holds only the update choices
     (`apple/App/Updates/SettingsWindow.swift`).
   - Linux: the header bar has Open and New on the left, and Preview,
     Render, the panel toggle and the main menu on the right
     (`crates/linux-app/src/app/window.rs:133-175`). The main menu
     (`crates/linux-app/src/app/mod.rs:454-536`) has no agent item.
   - Windows: there is a `MenuBar` (File, Edit, Design, View, Help) and
     no `CommandBar` (`windows/NeoSCAD.App/MainWindow.xaml:33-121`).
     The Help menu has only the update items.
   - `grep -i "mcp\|agent\|claude"` over the three apps finds nothing
     agent-related. The one hit is the macOS Shortcuts intents running
     under "the agent resource limits"
     (`apple/App/Intents/SCADIntents.swift:1-6`). Those are App Intents
     for Shortcuts (render, snapshot, check a file), not an MCP surface.
2. **No app ships the CLI.**
   - macOS: the DMG carries the CLI beside the app, not inside it
     (`docs/release.md`, "Artifacts"). The cask deliberately has no
     `binary` stanza because "the app bundle carries no command-line
     tool" (`packaging/homebrew/neoscad-app.rb:12-15`). The app has no
     "Install Command Line Tool" item.
   - Windows: the app's MSI installs `NeoSCAD.exe` and its runtime only
     (`windows/installer/NeoSCAD.wxs:112-125`). The CLI's separate MSI
     is the one that adds itself to `PATH`
     (`crates/cli/wix/main.wxs:135-137`). The app's installer
     deliberately registers no `App Paths` entry, so that `neoscad`
     stays the CLI's name (`docs/windows-app.md`, "Installer").
   - Linux: the Flatpak builds and installs only `neoscad-gtk`
     (`linux/flatpak/org.neoscad.NeoSCAD.yml:66-67`).
   - The website tells users to install the CLI separately: brew,
     `install.sh` or scoop (neoscad-website `agents.html:161-206`).
3. **File watching leaves the document itself out on every platform.**
   The core's `run_files` filters out the document and every open
   buffer (`crates/client/src/document.rs:514-523`). The apps watch
   what remains: includes, imports and fonts (macOS
   `apple/App/Document/FileWatcher.swift`; Linux
   `crates/linux-app/src/watch.rs:1-13`; Windows
   `windows/NeoSCAD.Host/FileWatch.cs`, `docs/windows-app.md`, "File
   watching"). An agent editing an included file through `neoscad mcp`
   therefore re-runs the open document live in all three apps.
4. **An agent's edit to the open file itself:**
   - **Linux:** no reload. Save writes the window's copy over the disk
     without checking it (`window.rs:1381-1390`, `write_to` at
     `:1420-1424`, `run::write_atomic`). The agent's edit never shows
     up, and is silently overwritten on the next Ctrl+S.
   - **Windows:** the same. `DocumentSession.Save` goes to
     `AtomicWrite`, which is `File.Move(tmp, path, overwrite: true)`
     (`windows/NeoSCAD.Host/DocumentSession.cs:194-208`), with no
     modification-time check and no reload.
   - **macOS:** the app relies on `NSDocument`, which presents its file
     (`NSFilePresenter`). `autosavesInPlace` is true
     (`SCADDocument.swift:289`). AppKit is expected to reload a clean
     document when the file changes, and to warn about a conflict on
     saving a dirty one. Neither is tested here (no test in
     `apple/AppTests` changes the file behind the document), and
     Apple's page for `presentedItemDidChange` could not be retrieved
     (see "Not verified").

     A reload goes through `read(from:)`, then `textReplaced`, then
     `editor.load` (`SCADDocument.swift:317-331`), which clears the
     editor's undo history (`apple/AppTests/EditorTests.swift:181`,
     "revertingLoadsTheTextAndClearsTheHistory"). Even at best, an
     agent's file edit on macOS replaces the text, with no highlight
     and nothing to undo.
5. **The shared editor already supports agent edits.** All three apps
   load the same CodeMirror bundle (`apple/Editor/web`; Linux through
   WebKitGTK, `crates/linux-app/src/bridge.rs:1-16`; Windows through
   WebView2). The bundle exports `NeoSCADEditor.agentEdit`: one
   isolated undo step, highlighted for 8 s, the user's selection kept,
   the view scrolled to the change (`apple/Editor/web/src/editor.js:377-411`).
   It also exports `selectionPositions` (`:415`) and `revealRange`
   (`:318`, already used by macOS and Linux for jumping to a console
   line). `agentEdit` dispatches an ordinary transaction, so each
   host's existing `changes` handling counts it as an edit with no new
   code. No native host calls `agentEdit` today.
6. **The viewport pieces exist in Rust, not yet in `ffi`.**
   - `render::Viewport::copy_as_shown` is the view as shown, with the
     grid and marks (`crates/render/src/viewport.rs:1014`).
   - `ffi` exposes only `copy_for_image`, which leaves the grid and
     marks out (`crates/ffi/src/inspect.rs:616-629`).
   - `ffi`'s `Viewport.set_annotations` exists
     (`crates/ffi/src/inspect.rs:563-592`), but the Check panel's
     overlay already uses it. Agent marks would replace the check marks
     unless they get a layer of their own.
7. **The CLI's bridge is browser-specific in two places, both
   deliberate.**
   - `--browser-url` accepts only `http` and `https`
     (`crates/cli/src/mcp/bridge.rs:516-519`), so there is no
     `neoscad://` link.
   - The text `browser_connect` gives the agent tells the user to open
     the link "in a desktop browser"
     (`crates/cli/src/mcp/tools/browser.rs:414`).
   - The origin check does let any process that has the token connect
     as the relay origin (`http://127.0.0.1:PORT`, `bridge.rs:327-337`).
     So a native app could join today's bridge as if it were a tab,
     given a pasted link. This is useful only as a spike (Option B0
     below).
8. **The CLI already has per-user local sockets with ownership checks.**
   - `neoscad serve --socket` uses `$XDG_RUNTIME_DIR/neoscad/serve.sock`,
     else `<temp>/neoscad-<uid>/serve.sock`, else
     `\\.\pipe\neoscad-<SID>` on Windows (`docs/serve-protocol.md:36-60`).
   - The directory is 0700 and the socket 0600.
   - Clients check the socket's and pipe's owner before sending
     anything (`crates/cli/src/transport.rs:161-195`,
     `docs/serve-protocol.md:425-440`).

   This is exactly the rendezvous a native bridge needs, already
   reviewed for squatting (agent-surface audit, finding 8).
9. **The setup snippets exist in three hand-kept copies:**
   `web/src/ui/agent.js:23-54` (`SETUPS`, five clients, all with
   `--browser`), neoscad-website `agents.html` ("Add it to your agent",
   `:286-370`, without `--browser`), and `docs/mcp.md` ("Setup"). They
   already differ in places. For example, the VS Code note mentions
   `code --add-mcp` in `agent.js` but not on `agents.html`.
10. **The macOS app is not sandboxed** (`apple/App/NeoSCAD.entitlements`).
    It can run `claude`, write client config files, open other apps' URL
    schemes and create a Unix socket. It registers no URL scheme of its
    own (`apple/App/Info.plist` has no `CFBundleURLTypes`). The Windows
    app is an unpackaged MSI install, so it has no sandbox either. The
    Flatpak has `--filesystem=home` and `--share=network`, but no
    `--talk-name=org.freedesktop.Flatpak`
    (`linux/flatpak/org.neoscad.NeoSCAD.yml:21-37`).

## Findings, by importance

### 1. Linux and Windows silently lose an agent's edits to the open file (high; data loss)

**What our code does.** See firm ground 3 and 4. With a `.scad` open
in the Linux or Windows app, an agent (or any other editor) changing
that file through `neoscad mcp` changes nothing on screen. The next
Save writes the app's older copy over it.

**OpenSCAD, for comparison.** OpenSCAD's "Automatic Reload and Preview"
reloads the main file when it changes on disk, which is how OpenSCAD
users pair it with an external editor. That is not re-cited here,
because this audit is about our apps' behaviour, not conformance.

**Who it affects.** Every user who follows `agents.html` today with the
Linux or Windows app open on the same file. That is the workflow the
website's MCP setup implies.

**Suggested change.** Watch the document's own file. On a change:

- if the document is clean, reload it as one undoable replacement
  (`agentEdit` with a single whole-text edit, so Undo restores the
  previous text);
- if it is dirty, show a bar: "This file was changed by another
  program. Reload / Keep mine".

Before saving, compare the modification time and size with those at
the last load or save; on a mismatch, ask before overwriting. The
diff-and-apply logic is pure and belongs in `client`, shared by all
three apps. macOS should switch to the same path instead of relying on
`NSDocument`'s revert, so the agent's change keeps undo there too. Add
a hosted app test for each platform.

### 2. No app makes the agent connection discoverable (high; the owner's observation)

**What our code does.** See firm ground 1. A user of the apps never
learns that agents can work with NeoSCAD. The web page has a prominent,
shimmering button (`web/src/ui/agent.js:94-109`).

**Suggested change.** A "Connect your AI agent…" entry point in every
app (spec under "Recommendation"). It can ship before the live bridge,
and is worth shipping even with only file-based MCP, once finding 1 is
fixed.

### 3. Setup depends on a separately installed CLI being on PATH (high; setup)

**What our code does.** See firm ground 2. Each client config the
snippets give runs a bare `neoscad`.

- On macOS, a GUI client such as Claude Desktop does not inherit the
  shell's `PATH`, which is why `agent.js:33` adds "If it cannot start
  neoscad, give its full path (from `which neoscad`)".
- No app installs the CLI.
- An app and a separately installed CLI can be different versions,
  which matters once they share a protocol.

**Suggested change.** Bundle the CLI with each app, and write absolute
paths into client configs (see "The CLI and PATH").

### 4. The agent cannot see the app's unsaved text, its view, or point at anything (medium; capability)

**What our code does.** File-based `neoscad mcp` sees what is saved.
The `editor_*` and `view_*` tools exist only for the web page.

**Who it affects.** Everyone who uses the app and an agent together.
This is the main advantage /try has over the desktop.

**Suggested change.** Option B below.

### 5. The setup content is duplicated three times and drifting (low; maintenance)

**Suggested change.** One table of per-client setups, with the server's
command as a parameter (bare `neoscad`, or the app's absolute path).
It lives in `client`, is exported through `ffi` to Swift and C#, and
is read directly by the Linux app and the wasm core. A small script
checks `agents.html` against it. The table needs no I/O, so it
respects the library-crate rule.

## The gap against /try

| | /try (web) | macOS app | Linux app | Windows app |
|---|---|---|---|---|
| Discoverable entry point | Toolbar button, "fresh" shimmer on first visit (`agent.js:94-109`) | none | none | none |
| Install `neoscad` | link to the download page | separate DMG tarball or brew formula; not in the app | separate (`install.sh`, brew, .deb, .rpm); not in the Flatpak | separate CLI MSI or scoop; not in the app MSI |
| Add to a client | copyable snippets for 5 clients (`agent.js:23-54`) | none | none | none |
| Pairing | agent calls `browser_connect`, user opens or pastes the link; a new link every `neoscad mcp` start (`docs/followups.md`, "The bridge's link changes…") | n/a | n/a | n/a |
| Live status | button text "Claude Code connected", pulse, tooltip with the current activity (`agent.js:65-73, 348-425`) | none | none | none |
| Agent edits visible and undoable | `editor_edit`: one highlighted undo step, versioned, optional Apply/Reject (`page.js:83-101`, `agent.js:430-447`) | file edits: reload (expected, unverified) that clears undo | not shown; lost on save | not shown; lost on save |
| Unsaved text | `editor_read` | no | no | no |
| View capture and marks | `view_capture` (`copy_as_shown`), `view_annotate` | no | no | no |
| Camera, reveal, console | yes | no | no | no |
| Includes changed by the agent re-run | n/a (page) | yes (FSEvents) | yes (GFileMonitor) | yes (FileSystemWatcher) |

## Options, ranked by the owner's criteria

Criteria, in order:

- **Ease:** steps from a fresh install to the agent working.
- **UX:** what the user sees while the agent works.
- **Performance:** idle cost, and latency per call.

Effort and risk are listed, but they do not decide the ranking.

### Option C: a local app bridge that plain `neoscad mcp` discovers (recommended)

Each app process listens on a per-user local socket. There is no TCP
listener, no token and no link.

**Where the socket lives.**
- **macOS:** `~/Library/Application Support/NeoSCAD/run/`.
  - Not `$TMPDIR`: Rust's `temp_dir()` falls back to `/tmp` when
    `TMPDIR` is unset, and an MCP client may start servers with a
    trimmed environment. The CLI and the app would then look in
    different places.
  - The path must stay under 104 bytes (`docs/serve-protocol.md`), so
    long home paths need checking.
- **Linux:** `$XDG_RUNTIME_DIR/app/org.neoscad.NeoSCAD/` for the
  Flatpak, and `$XDG_RUNTIME_DIR/neoscad/` otherwise.
- **Windows:** a pipe `\\.\pipe\neoscad-app-<SID>-<pid>`.

**How the CLI finds and talks to it.**
- `neoscad mcp`, with no flag, looks there at startup. While no app is
  connected it looks again every 1 to 2 s; a `stat` costs nothing
  measurable. It connects to every app it finds.
- It checks the owner as `transport.rs` already does, in both
  directions.
- It speaks the bridge's existing messages (`welcome` and `hello`, then
  `read`, `edit`, `reveal`, `camera`, `capture`, `annotate`, `console`)
  as newline-delimited JSON instead of WebSocket frames.

**What the agent gets.**
- The `editor_*` and `view_*` tools, plus `documents`, which lists the
  open windows. Each takes an optional `document` argument; the
  default is the window focused most recently.
- The model tools default to the focused document's buffer, as they do
  for a connected page. They use the document's real path, so its
  includes resolve beside it.

**What the app shows.**
- Status: "Claude Code connected".
- Activity: "Claude Code is editing", including the CLI's own tool
  calls such as rendering or checking. The CLI sends them as
  `activity` notes once an app is connected.
- An Apply/Reject bar when the user has asked for one.
- Highlighted, undoable edits.

**Ease.** After the one-click client setup (Option A), there are no
steps at all. Asking "change the teeth" works on what is open. App
restarts, client restarts and new sessions all reconnect by
themselves, because nothing is keyed to a session; this also removes
/try's "new link every start" problem.

**Performance.**
- Idle: one thread blocked in `accept` in the app, and nothing at all
  until the user has enabled agents.
- Per call: one local socket round trip.
- `view_capture` draws offscreen at the requested size, as on the web.
- The CLI runs model tools in its own session. Routing them to the
  app's warm core instead would avoid evaluating twice and is a later
  step; it needs the app and the CLI to be the same version, which
  bundling gives.

**Where the code would live.**
- **Rust, shared:**
  - The protocol and the document-side handlers go in `client`: position
    conversion through `lang::source`, the version check, building the
    `read` answer from a `DocumentLoop`, the annotation layer. This code
    is pure and stays WASM-clean.
  - The listener goes in a new non-library crate, for example
    `crates/agent-link`, using the transport code moved out of
    `crates/cli/src/transport.rs`. `ffi` (macOS, Windows), `linux-app`
    and `cli` link it.
  - The CLI's `Bridge` becomes one implementation of a "surface" trait
    beside an app surface, so `tools/browser.rs` serves both.
- **Native:** the status control, the sheet, the approval bar, and the
  calls into the editor (`agentEdit`, `selectionPositions`,
  `revealRange`) and the viewport (`copy_as_shown` and an agent
  annotation layer, both new in `ffi`).

**Effort.** L across all three apps: the CLI refactor, the shared
crate, three hosts, and tests including a determinism-free socket test
per OS.

**Risk.** Medium. The tool list has to change while a session runs
(below). The Windows pipe lifecycle needs care. On macOS the app is
multi-window in one process. Windows is one window per process
(`windows/NeoSCAD.App/App.xaml.cs:35`), so the CLI must handle several
app processes.

**The tool list.**
- Listing the app tools only while an app is connected keeps the
  file-only sessions' context unchanged. Today the browser tools add
  2,510 bytes (`docs/mcp.md`).
- That means sending `notifications/tools/list_changed`; the server
  now declares `listChanged: false` (`crates/cli/src/mcp/mod.rs:454`).
- Claude Code documents that it refetches the list on that notification
  in interactive sessions
  ([Claude Code MCP](https://code.claude.com/docs/en/mcp), "Dynamic tool
  updates"). Cursor, VS Code and Claude Desktop were not checked.
- The fallback: list the app tools whenever an app socket exists at
  `tools/list` time.

### Option A: discoverability and one-click client setup, with no live bridge (first step; needed by C anyway)

A "Connect your AI agent…" sheet with a row per client. Each row
**uses the client's own install path, so the client asks for consent
and owns its config format**:

| Client | One-click action | Verified |
|---|---|---|
| Claude Code | Run `claude mcp add --scope user neoscad -- <cli> mcp` (user scope: "All your projects", stored in `~/.claude.json`) after showing the exact command | flags from [Claude Code MCP](https://code.claude.com/docs/en/mcp), "scopes" table; `claude mcp add-json <name> '<json>' --scope user` also documented |
| Cursor | Open `cursor://anysphere.cursor-deeplink/mcp/install?name=neoscad&config=<base64 JSON>` | [Cursor install links](https://cursor.com/docs/context/mcp/install-links): "JSON.stringify the configuration then base64 encode it" |
| VS Code | Open `vscode:mcp/install?<URL-encoded JSON>` (`{"name":"neoscad","command":...,"args":["mcp"]}`), or run `code --add-mcp '<json>'` | [VS Code MCP developer guide](https://code.visualstudio.com/api/extension-guides/ai/mcp); `--add-mcp` in [VS Code MCP servers](https://code.visualstudio.com/docs/copilot/customization/mcp-servers) |
| Claude Desktop (macOS, Windows) | Merge `mcpServers.neoscad` into `claude_desktop_config.json` (macOS `~/Library/Application Support/Claude/`, Windows `%APPDATA%\Claude\`) after consent, keeping a backup; then "Quit and reopen Claude to finish" | paths and restart from [Connect to local MCP servers](https://modelcontextprotocol.io/docs/develop/connect-local-servers); "Claude Desktop is available for macOS and Windows" (no Linux row) |
| Other | Copy the command and a JSON block, with the absolute path filled in | |

An `.mcpb` bundle would give Claude Desktop its own install dialog
instead of a config edit ("open the file with Claude for macOS and
Windows to show an installation dialog",
[mcpb README](https://github.com/modelcontextprotocol/mcpb)). It
supports binary servers. Whether its manifest can point at a binary
outside the bundle, rather than carry a second copy of the CLI, was not
checked.

**Detection.** The app shows "Installed" or "Not found" per client:
`claude` through a login shell (`$SHELL -lc 'command -v claude'`, since
a GUI app's `PATH` lacks the user's), the Cursor and VS Code apps
through their URL schemes' handlers, and Claude Desktop through its
config directory. "Added" can be confirmed for Claude Code by `claude
mcp get neoscad`, and for Claude Desktop by reading the config file;
for the deep links it cannot.

**On its own**, Option A gives file-based MCP: the agent edits saved
files, and finding 1's fix makes the app show them. Effort S to M per
app, plus the shared setup table. Risk low.

### Option B0: join today's `--browser` bridge as a tab (spike only)

The app parses a pasted `https://neoscad.org/try/#connect=PORT.TOKEN`
link and connects with `Origin: http://127.0.0.1:PORT`, which
`origin_kind` accepts (firm ground 7). There are no CLI changes, so it
is a quick way to port and test the page-side handlers (`page.js`) in
one app. As a product it fails every criterion: the agent must call
`browser_connect`, the user must paste a link, it breaks on every
restart, and the agent is told to open a browser. Don't ship it.

### Option D: the app hosts the MCP server itself (rejected)

The client would start `NeoSCAD.app/Contents/MacOS/NeoSCAD --mcp`, or
connect over HTTP to a server the app runs. The first ties the agent to
a GUI process's lifetime, and on Windows to a GUI-subsystem exe's
stdio, which is unverified. The second needs the app running before the
client starts and adds a network listener, which is the attack surface
Option C avoids. Neither is easier for the user than C, because in C
the client already starts `neoscad mcp`.

### Option E: a `neoscad://` URL scheme (an addition, not an alternative)

`neoscad://open?path=…` lets an agent show the user a file in the app,
and `neoscad://connect` could open the setup sheet from `agents.html`.
It is useful, but it pairs nothing in C, because C needs no pairing.
Any action a URL triggers needs a confirmation, since any web page can
open the scheme.

## Where "Just Works" meets platform limits

| Limit | Platform | How the design handles it |
|---|---|---|
| A GUI process's `PATH` lacks Homebrew and `~/.local/bin`, and so do GUI clients | macOS | Configs get an absolute path, never a bare `neoscad`. The path is to a stable symlink the app refreshes on every launch (`~/Library/Application Support/NeoSCAD/bin/neoscad`, pointing into the bundle), so moving or updating the app does not break it. `claude` is found through a login shell |
| `TMPDIR` can differ, or be missing, between the app and a server a client starts | macOS | The socket goes under `~/Library/Application Support/NeoSCAD/run/`, not the temp directory |
| A Flatpak cannot put a binary on the host `PATH`, and cannot run host `claude` without `--talk-name=org.freedesktop.Flatpak`, which amounts to a sandbox escape | Linux | The Flatpak ships `neoscad` in `/app/bin`. Configs use `flatpak run --command=neoscad org.neoscad.NeoSCAD mcp`. The Claude Code row shows a copy button instead of running `claude`. A host-installed CLI (brew, `install.sh`, .deb) also works and looks in the Flatpak's socket directory |
| A CLI run through `flatpak run` sees only what the sandbox sees (`--filesystem=home`) | Linux | Projects outside home are invisible to it. The sheet says so and offers the host CLI. Whether `flatpak run` keeps the caller's working directory was not checked |
| Reaching the socket across the sandbox | Linux | Flatpak documents `$XDG_RUNTIME_DIR/app/$FLATPAK_ID` as writable inside the sandbox ([Flatpak sandbox permissions](https://docs.flatpak.org/en/latest/sandbox-permissions.html)). That it is the same directory on the host was not stated on that page; it needs a test |
| `--share=network` "also grants access to all host services listening on abstract Unix sockets … and these have no permission checks" (same page) | Linux | Use a path socket in the app's runtime directory, never an abstract one |
| Opening `cursor://` and `vscode:` from a Flatpak | Linux | Through the OpenURI portal (`gtk::UriLauncher`). Whether the portal opens arbitrary schemes was not checked |
| Writing other apps' config files | all | Only Claude Desktop's, after consent and with a backup. All others go through the client's own command or deep link |
| Pipe names are global | Windows | Owner check on connect (as `transport.rs` does for `serve`), and a per-process name so several app processes coexist |
| Two CLIs on one machine (the app's and brew's or scoop's) | macOS, Windows | Configs written by the app name the app's copy. The `hello` carries a protocol version, and on a mismatch the app says which `neoscad` is too old |
| Claude Desktop needs a restart to load a server (source above) | macOS, Windows | The sheet says so after adding |

## Security and privacy

Compared with /try:

- **What gets simpler.** The web bridge needs a token, an origin check,
  a host check and a relay page, because a browser sits between the
  agent and the page and any website can try to reach `127.0.0.1`
  (`docs/agent-bridge.md`, "Security"). Option C has no browser and no
  TCP port. A path socket in a user-only directory, or a pipe checked
  for its owner, admits only the same user's processes. Those can
  already read and write the user's files, and so the user's models. A
  token adds nothing against them, and losing it would cost every
  restart a re-pairing.
- **Same-user processes still matter, because the app adds something
  new.** Unsaved text, and pictures of the view, can now be read by any
  same-user process. Hence:
  - **Off until the user turns it on.** The listener starts only after
    the user has chosen Allow once (consent below). Before that there is
    no socket, which also means no idle cost.
  - **Visible.** The status control shows every connected client by its
    self-reported name, and the activity as it happens. The name is a
    label, not authentication; the UI must not imply it is verified.
  - **One switch** in Settings to turn it off, which closes the socket
    at once, and "Disconnect" per client.
- **The app must not:**
  - listen on TCP, or use an abstract socket on Linux;
  - accept a peer of another user. Check `getpeereid` /
    `SO_PEERCRED`, or the pipe client's token, as well as the socket's
    permissions;
  - run anything a message names;
  - save, export or write files for the agent. Edits go into the
    buffer. On macOS autosave then saves them, as it would the user's
    typing; elsewhere the user saves. File writes stay the CLI's,
    fenced by its roots (`docs/mcp.md`, "Safety");
  - accept an edit on a stale version;
  - grow past the web bridge's bounds: 32 MiB messages, capture size up
    to 2048, 500 marks and 20,000 points (`docs/agent-bridge.md`).
- **Roots.** For an app document, the CLI should be able to read beside
  the document's own path so its includes resolve. That widens the read
  fence by one directory per open document, which the user chose by
  opening it. Writes stay in the roots. This is a decision for the
  owner (below).
- **Consent, at first enable:**

  > **Let AI agents work on your open models?** Agents on this
  > computer that use NeoSCAD (Claude Code, Cursor, VS Code and others)
  > will be able to read and edit the models open in NeoSCAD and see
  > the 3D view. What they read goes to the agent's AI service.
  > [Allow] [Not Now]

  Edits need no separate consent, because they are visible and
  undoable. "Ask me before applying the agent's edits" is a setting, as
  on the web (`agent.js:202-205`), and is off by default as there.
  Captures need no prompt; the activity line shows "is looking at the
  view".

## The CLI and PATH

| Install | `neoscad` on PATH? | Proposal |
|---|---|---|
| macOS DMG | no; the tarball is separate, installed by hand (`docs/release.md`, "The CLI, and MCP") | Put the universal CLI in `NeoSCAD.app/Contents/Helpers/neoscad`, signed and notarized with the app. That adds about 35 MB unpacked and 16 MB compressed (the CLI, bare and as a tarball, `docs/release.md`, "Size"); measure the DMG. Add an optional menu item, "Install Command Line Tool…", that links it into `/usr/local/bin` (admin prompt) or `~/.local/bin`. Configs never depend on it |
| Homebrew cask | no (no `binary` stanza, on purpose) | Keep it that way; it avoids a conflict with the `neoscad` formula. Decision below |
| Homebrew formula, `install.sh`, .deb, .rpm, Nix | yes | Works with C as it is |
| Windows app MSI | no | Ship `neoscad.exe` (console subsystem) beside `NeoSCAD.exe` in `Program Files\NeoSCAD` and use its absolute path. Adding to `PATH` is a decision, because the CLI MSI already does |
| Windows CLI MSI or scoop | yes (MSI: `crates/cli/wix/main.wxs:135`) | Works with C as it is |
| Flatpak | no, and it cannot be | `/app/bin/neoscad` and `flatpak run --command=neoscad org.neoscad.NeoSCAD mcp`, as above |

The Flatpak's loopback question for the web bridge does not arise in C,
which uses a socket file. The Flatpak shares the host network
(`--share=network`), so the existing TCP bridge would also work from
inside it; it is just not needed.

## Recommendation

**Build order:**
1. Finding 1 on all three apps (S to M; independent; ship now).
2. The bundled CLI and the shared setup table (M).
3. Option A's sheet in all three apps (S to M each).
4. Option C (L).

Steps 2 and 3 can be built while C is designed, and C's UI reuses the
sheet.

**Share rather than rewrite:**
- the `SETUPS` content (`agent.js:23-54`) and `IDEAS` (`:56-63`), moved
  into the shared table;
- the `DOING` strings (`:65-73`);
- the status and failure texts;
- `agentEdit` in the editor bundle;
- the CLI's tool descriptions and position conversion.

The web keeps its own connection code (WebSocket and relay); the
native apps never need it.

**UI spec.** The control has the same states on every platform:
- idle: "Connect your AI agent";
- connected: "Claude Code connected" (several clients: "2 agents
  connected");
- working: a pulse, and the activity in the tooltip or subtitle, for
  example "Claude Code is editing";
- off: hidden behind the menu item only, after the user turned agents
  off in Settings.

*macOS (AppKit and SwiftUI):*
- An `NSToolbar` on the document window, which also gives Preview and
  Render their buttons. A trailing item shows the sparkle mark and the
  label "Agent". When connected, it shows the client's name with a
  status dot. Clicking opens a popover: the status, the last few
  activities, Disconnect, and "Ask me before applying edits".
- App menu: "Connect Your AI Agent…" below "Settings…". Help menu:
  "Using NeoSCAD with AI Agents", which opens `agents.html`.
- The sheet has three parts:
  1. "NeoSCAD's command-line tool is included" (status, and "Install
     Command Line Tool…").
  2. The client rows (Option A table) with "Add" buttons and their
     states: "Added", "Not found", "Restart Claude to finish".
  3. "Things to ask" (`IDEAS`).
- Approval: a bar under the toolbar, "Claude Code wants to change the
  tooth count. [Reject] [Apply]".
- Settings gains an "Agents" pane: Allow agents to work on open models
  (switch), Ask before applying edits (switch), Connected agents (list).

*Linux (GTK 4 / libadwaita):*
- A header-bar button with `pack_end`, before the main menu. Idle: a
  symbolic icon with the tooltip "Connect your AI agent". Connected: an
  `AdwButtonContent` with the client's name and a status dot. It opens
  a `GtkPopover` with the same content as macOS.
- Main menu: "Connect Your AI Agent…" in the app section, opening an
  `AdwDialog`.
- Preferences: an "Agents" `AdwPreferencesGroup`.
- Approval: an `AdwBanner` with the button "Apply" and a close action
  that rejects. First connection: an `AdwToast`, "Claude Code
  connected".
- The Claude Code row shows the command with a copy button, because a
  Flatpak cannot run host `claude`; Cursor and VS Code open through
  `GtkUriLauncher`.

*Windows (WinUI 3):*
- The window has a `MenuBar`, not a `CommandBar`. Put the agent button
  at the right end of the menu row (a second column in the row's
  `Grid`), styled as a subtle `Button` with the sparkle glyph. It opens
  a `Flyout` with the status, activity, Disconnect and the ask-first
  `ToggleSwitch`.
- Help menu: "Connect Your AI Agent…" (a `ContentDialog` with the client
  rows) and "Using NeoSCAD with AI Agents".
- Approval: an `InfoBar` like the update bar
  (`MainWindow.xaml:126-131`), with an Apply `ActionButton` and close
  meaning Reject.

**Setup content.** One table, rendered by all four surfaces with
`<cli>` filled in:
- the app's absolute path (macOS symlink, Windows install directory),
  or the `flatpak run` form;
- on the web, plain `neoscad` with `--browser`.

The server's arguments for the native apps are just `["mcp"]`: with
Option C, app discovery is part of plain `neoscad mcp`.

## Open questions for the owner

1. **Should plain `neoscad mcp` discover the app?** (Recommended: yes,
   behind an opt-out flag.) The alternative is an `--app` flag in every
   config. That costs one more thing to get right, and buys nothing
   unless app discovery is unwanted.
2. **Should agent access be off until first consent?** (Recommended:
   yes.) Or on by default, with the indicator as the only control?
3. **Should the app bundle include the CLI?** It adds roughly 16 MB to
   the macOS download and a similar amount on Windows. Should the cask
   then gain a `binary` stanza, and so conflict with the formula?
4. **Should the Windows app MSI add its `neoscad.exe` to `PATH`**, given
   that the CLI MSI also does?
5. **Should the CLI widen its read roots to an open app document's
   directory?** (Security, above.)
6. **May NeoSCAD edit `claude_desktop_config.json`** (with consent and a
   backup)? Or should it ship an `.mcpb` bundle, or only a copy button?
7. **Should the listener and transport code live in a new non-library
   crate (`agent-link`)?** That would be added to `CLAUDE.md`'s list of
   crates allowed to use `std::fs` and sockets, with the protocol logic
   staying in `client`.
8. **Is a Flatpak that runs host `claude`
   (`--talk-name=org.freedesktop.Flatpak`) acceptable?** Flathub is
   already skipped, so its review would not object. Recommended: no; the
   copy button is enough.

## Checked and found fine

- Includes, imports and fonts that an agent changes on disk re-run the
  open document in all three apps (firm ground 3).
- The editor bundle's `agentEdit` already does what a native bridge
  needs: one isolated undo step, a highlight, the selection kept, the
  view scrolled (`editor.js:377-411`). Each host already counts it as
  an ordinary edit through its `changes` handling.
- The macOS app is unsandboxed with the hardened runtime and no
  exceptions, so running `claude`, opening URL schemes and creating a
  socket need no new entitlement (`NeoSCAD.entitlements`).
- `transport.rs`'s owner checks cover squatting on both Unix and
  Windows pipes (agent-surface audit, finding 8), and can be reused
  as they are.
- The web bridge's Origin, Host and token checks and its bounds are as
  `docs/agent-bridge.md` describes (`bridge.rs:45-66, 316-337`); nothing
  here needs them changed.

## Not verified

- **macOS `NSDocument` behaviour when its file changes on disk.**
  Expected: a clean document reloads, and a dirty one warns on save.
  Apple's `presentedItemDidChange` page returned no content to the
  fetch, and the app was not run.
- **Whether `$XDG_RUNTIME_DIR/app/$FLATPAK_ID` is the same directory on
  the host.** The Flatpak page lists it as writable inside the sandbox
  but does not say so.
- **Whether `flatpak run` keeps the caller's working directory**, and
  how long its start-up takes.
- **Whether the OpenURI portal opens `cursor://` and `vscode:` links.**
- **Whether Cursor, VS Code and Claude Desktop honour
  `notifications/tools/list_changed`** from a stdio server. Only Claude
  Code's documentation was read, and it describes interactive sessions.
  Its note that newer-revision notifications arrive "over a stream it
  holds open" was not checked against a stdio server.
- **Whether an `.mcpb` manifest can run a binary outside the bundle.**
- **Whether Claude Code has an install deep link.** None was found.
- **A Windows GUI-subsystem exe as a stdio MCP server** (only relevant
  to Option D).
- **The bundled CLI's effect on the DMG and MSI sizes.** It was
  estimated from `docs/release.md`, not measured.
