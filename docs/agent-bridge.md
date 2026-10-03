# Connect your AI agent: the web page's agent bridge

An agent (Claude Code, Claude Desktop, Cursor, VS Code, any MCP client)
works on the model the user has open in the web demo (neoscad.org/try):
it reads and edits the editor's text, and sees and marks the 3D view. The
page is static (GitHub Pages); the agent already runs `neoscad mcp` on the
user's computer, so that process is the bridge. The tools are in
`docs/mcp.md`, "The web page".

| Part | Where |
|---|---|
| The bridge: listener, checks, one tab, relay page | `crates/cli/src/mcp/bridge.rs`, `relay.html` |
| The tools, and the page as the model tools' default | `crates/cli/src/mcp/tools/browser.rs`, `tools.rs` (`Label`, `page_model`) |
| The page's connection (direct or relay) | `web/src/agent/connection.js`, `link.js` |
| The page's answers to each request | `web/src/agent/page.js` |
| The button, dialog, approval bar | `web/src/ui/agent.js`, `app.css` ("Connect your AI agent") |
| Agent edits: one undo step, highlighted | `apple/Editor/web/src/editor.js` (`agentEdit`) |
| The capture of the view as shown | `crates/render/src/viewport.rs` (`copy_as_shown`), `crates/web-view` (`image(w, h, shown)`) |
| Tests | `crates/cli/tests/browser.rs`, `web/test/agent.test.js`, `web/e2e/agent.spec.js`, `web/e2e/phone.spec.js` |

## How it connects

1. `neoscad mcp --browser` listens on `127.0.0.1`, on a free port, and
   makes a 128-bit random token (the OS's generator, `getrandom`).
2. The agent calls `browser_connect`, which gives
   `https://neoscad.org/try/#connect=PORT.TOKEN`; the agent shows it (or
   opens it with `open: true`, or the server with `--open`). The user
   opens it, or pastes it into the dialog of a tab already open.
3. The page reads the fragment and takes it out of the address bar at
   once. It opens `ws://127.0.0.1:PORT/ws?token=TOKEN`, unless the
   browser already reports the loopback permission as `denied`
   (`navigator.permissions.query` for `loopback-network`, Chrome 145 and
   later, then `local-network-access`, Chrome 142-144; a name the browser
   does not know throws and counts as not denied). A denied permission
   would fail the attempt anyway, so the page goes straight to step 4.
4. If the browser will not allow that (below), the dialog says so and
   offers **a connection window**. In Chrome and Edge it also says how to
   allow the direct way next time (the icon at the left of the address
   bar, Site settings, "Apps on device", or "Local network access" before
   Chrome 145, set to Allow). The button opens
   `http://127.0.0.1:PORT/relay#TOKEN` as a popup: a page of the bridge's
   own, whose WebSocket is same-origin. It passes messages to and from the
   tab with `postMessage`. Its title and text say to keep it open while
   the agent works and that closing it disconnects.
5. The bridge sends `welcome` (the MCP client's name, so the page says
   "Claude Code connected"). The page sends `hello` (its file and
   browser). From then on the bridge sends requests (`read`, `edit`,
   `reveal`, `camera`, `capture`, `annotate`, `console`) and the page
   answers.

The agent does not need to poll while the user opens the link:
`browser_connect` with `wait_seconds` (at most 120) blocks until a tab has
connected and said hello, or the time runs out. Without a tab, the other
page tools wait up to 5 s for one (a reload, or a user still in the
connection window) before answering with how to connect. When the page
is not connected, `browser_connect` tells the agent what to pass on: to
click "Open a connection window" if the page says it cannot reach
neoscad directly. Connected through the window, it says the user must
keep that window open.

A reload reconnects directly with the tab's last link (sessionStorage,
per tab). If that fails it stays quiet: the agent may be gone. A link with
`&via=relay` skips the direct attempt, for a browser known to block it and
for the tests.

### Why WebSocket, and why a relay window rather than serving the page

WebSocket is one connection in both directions with no CORS preflight. The
bridge checks `Origin` itself, and a sync crate (`tungstenite`, no async
runtime; the CLI has none) does the framing. Long-polling HTTP meets the
same browser rules (it is the same kind of request to 127.0.0.1) and adds
CORS and polling.

The brief proposed a fallback that serves the whole /try app from the
bridge's origin, from a local bundle or by proxying neoscad.org. That
would need a bundle shipped with the CLI (the wasm core and viewers, tens
of MB) or an HTTPS client in it. It would also move the user to another
origin, where their saved edits (localStorage is per origin) are not. The
relay window needs about 50 lines of HTML, keeps the user on the real
page with their work, and worked in every browser tested. Its costs: a
click (popups need one), a small window that must stay open (closing it
disconnects, and the page says so), and it depends on the page not
sending `Cross-Origin-Opener-Policy: same-origin`, which would cut
`window.opener`. GitHub Pages sends none, and the page's CSP meta tag
cannot.

## Browsers

Verified with Playwright 1.63 on macOS, on 2026-09-29, two ways:

- **As neoscad.org:** the built bundle served over HTTPS as
  `https://neoscad.org/try/`, with the real website's site.json and
  theme.css. The page's origin is exactly `https://neoscad.org`. Firefox
  and WebKit reach it through a local CONNECT proxy (the bridge's
  127.0.0.1 bypasses the proxy). Chrome reaches it through
  `--host-resolver-rules` with `--ip-address-space-overrides=...=public`,
  so Local Network Access treats the page as a public site, as the real
  one is. Each run starts the real `neoscad mcp --browser` with its
  default page URL, then calls `editor_read` and `view_capture` over MCP.
- **Served from 127.0.0.1** (`web/e2e/agent.spec.js`): every tool, the
  relay, approvals, replacement, light and dark, in Chromium, Firefox and
  WebKit, against both the mock build and the real engine and viewer.
  One Chromium test makes the page a public site
  (`--ip-address-space-overrides=127.0.0.1:PORT=public`): headless
  Chromium 153 denies the direct socket, after which
  `navigator.permissions.query` reports `denied` for both
  `loopback-network` and `local-network-access`, and the next link goes
  straight to the connection window without a direct attempt. Real Chrome
  after a person clicks "Block" was not checked.

| Browser | Direct `ws://127.0.0.1` from https://neoscad.org | Connection window | Verified |
|---|---|---|---|
| Firefox 155 | works, no prompt | works | yes, both ways |
| Chrome 154 (stable), Chromium 153 | **blocked** (`ERR_BLOCKED_BY_LOCAL_NETWORK_ACCESS_CHECKS`) unless the user allows Local Network Access when Chrome asks; with the permission granted, it works | works, no prompt (a top-level window on 127.0.0.1 is not a request to the local network) | yes: denied (headless), granted (`grantPermissions(["local-network-access"])`), and relay |
| WebKit 26.6 (Playwright) | **blocked** as mixed content, always; the socket gets `error` and never `close` | works | yes, both ways |
| Edge | expected as Chrome (same Chromium LNA) | expected to work | **unverified**: Edge is not installed |
| Safari 27 (the real app) | expected as WebKit | expected to work | **unverified**: automating Safari needs `safaridriver --enable` (an admin prompt) |
| Phones | not offered: the bridge runs on a desktop | | the dialog at 412 px says so (`phone.spec.js`) |

The limits of the emulation:

- The certificate is self-signed, with the errors ignored. The page was
  still a secure context (`isSecureContext` true in every engine).
- Headless Chrome answers the LNA prompt with "deny". What a person sees
  is Chrome's prompt, and the page's hint ("If the browser asks whether
  this site may reach apps or devices on this computer, choose Allow")
  was not checked against its exact wording.
- Playwright's WebKit is not the Safari app. Safari's mixed-content and
  popup rules are WebKit's, but this was not run in Safari itself.

A separate probe (plain pages, no app) confirmed the same matrix. It
showed that an iframe of the relay does not work in WebKit (mixed
content) or in public-address Chrome (LNA), which is why the relay is a
window.

The page's CSP allows `connect-src ws://127.0.0.1:*`. Without that, the
direct socket would be refused in every browser before any of the above
applies.

## Security

What an attacker would want is to drive the agent's tools on the user's
behalf, or to read or change the user's model. The bridge only answers
tool calls that come from the agent over stdio. The page only does what
those calls ask. So the thing to protect is the pairing: which tab may
connect.

- **Token:** 128 random bits, compared in constant time, in the link's
  fragment, which browsers never send to a server (not to GitHub Pages,
  not in `Referer`). The page removes it from the address bar and history
  on load, and keeps it only in sessionStorage for a reload of that tab.
  It lives as long as the `neoscad mcp` process.
- **Origin:** the socket is accepted only with `Origin` equal to the
  page's origin (`https://neoscad.org`, or `--browser-url`'s) or the
  bridge's own (`http://127.0.0.1:PORT` or `http://localhost:PORT`, the
  relay window). A page on another site that somehow had the token would
  still be refused. So would a native process that is not a browser but
  sends a matching Origin with the right token: at that point it can read
  the user's files anyway.
- **Host:** every request must name `127.0.0.1:PORT` or `localhost:PORT`
  (421 otherwise), so a DNS-rebinding page, whose requests carry its own
  host name, cannot reach the relay page or the socket.
- **Loopback only:** it binds `127.0.0.1`, never `0.0.0.0`.
- **The relay page** posts only to the page's origin (the `targetOrigin`
  of every `postMessage`), and accepts messages only from its opener on
  that origin. It is sent with `frame-ancestors 'none'`,
  `X-Frame-Options: DENY`, a CSP that allows only its own socket,
  `no-store` and `no-referrer`. The page in turn accepts relay messages
  only from the window it opened, on the bridge's origin.
- **One tab:** a new connection with the token replaces the old one,
  which is closed with code 4001, and the old tab says "Another tab took
  over". Refusing the new one instead would strand a user who reloaded,
  or whose first tab is already gone, and the new tab had to present the
  token anyway.
- **Bounds:** request heads up to 16 KiB and 5 s; messages up to 32 MiB
  (a capture's PNG); tool calls time out (15 s, 90 s for a capture that
  waits for a preview, 150 s for an edit waiting for the user's Apply).
  An agent's marks are capped at 500 items and 20,000 points.
- **Edits** are versioned: the page counts every change of the text and
  every switch of example. An edit made on text the agent read earlier is
  refused, so it cannot overwrite what the user just typed. Each edit is
  one undo step, highlighted in the editor. "Ask me before applying" makes
  each one wait for Apply or Reject, and a rejection reaches the agent as
  a refusal to act on.
- **User control:** the button always shows the state ("Claude Code
  connected", a pulse while a request runs, with what it is doing in its
  tooltip). Disconnect is in the dialog, and closing the relay window or
  the tab ends it.
- **Files:** the page tools touch no files. The model tools on the page's
  text run it as a document under `base_dir` with the page's file name,
  reduced to a plain `.scad` name (`page.scad` otherwise), so the page
  cannot name a path. The session's roots still fence every `include`.

Not protected against: a local process running as the user (it can read
the MCP process's stderr, where the link is logged, or just edit files);
the user pasting their link into a site that then copies it into
neoscad.org (the origin check stops any other origin from using it).

## The tools, and why these

The model tools (`evaluate`, `render`, `snapshot`, `check`, `measure`,
`format`) already answer every question about a model. So a connected
page just becomes their default model: no `source: "browser"` argument to
learn, and no schema growth. The page tools are only what needs the page:
its text and version (`editor_read`), a way to change it that the user
sees (`editor_edit`), and a way to show them a place (`editor_reveal`).
For the view: its camera (`view_camera`), exactly what they see
(`view_capture`) and a way to point (`view_annotate`). And the page's
console (`console_read`), which can differ from a native run's (libraries
fetched in the page). Add `browser_connect` to get started. Sizes are in
`docs/mcp.md`. They are listed only with `--browser`, so sessions without
it pay nothing.

`view_capture` draws the view offscreen at the requested size, with the
user's camera, grid and annotations (`Viewport::copy_as_shown`). A WebGPU
or WebGL canvas cannot be read back after it is presented. The export
image (`copy_for_image`) leaves the grid and marks out, which are the
point here.

Positions: agents get and give 1-based lines and 1-based byte columns,
the unit of every diagnostic, so `at` edits can target a diagnostic's
span. The editor counts UTF-16 units. The conversion happens once, in
Rust, through `lang::source` (`utf16_position`, `offset_at_utf16`).
Most edits use `{old, new}`, which needs no positions.

## Desktop apps

The macOS, Linux and Windows apps get the same tools on their open
documents, with no link and no flag: plain `neoscad mcp` finds a running
app by itself (owner decision, 2026-10-02; `--no-app` turns it off). The
design and the options weighed are in
`docs/audits/agent-connection-desktop.md` (Option C). This is the shared
foundation. The macOS app's controls (the toolbar control, the consent
switch, the setup sheet, the approval bar) are built on it
(`apple/App/Agents`; the user's flow is in `docs/mcp.md`, "The desktop
apps").

| Part | Where |
|---|---|
| The protocol, and the answer to each request from the app's documents | `crates/client/src/agent.rs` (pure, WASM-clean; tests in `agent_tests.rs`) |
| Sockets and pipes with owner checks (shared with `neoscad serve --socket`) | `crates/agent-link/src/transport.rs`, `transport/win.rs` |
| Where an app listens, where the command line looks | `crates/agent-link/src/discovery.rs` |
| The app's listener (`AgentLink`) | `crates/agent-link/src/link.rs` |
| The macOS and Windows apps' API (UniFFI) | `crates/ffi/src/agent.rs`; the Linux app uses `agent_link` and `client::agent` directly |
| The Linux app's side: consent, the host that hops to the GTK main loop, the button, page and requests | `crates/linux-app/src/agent.rs`, `src/app/agent.rs`, `src/app/window/agent.rs` (`docs/linux-app.md`, "AI agents") |
| Setting up clients: finding and running `claude`, Claude Desktop's file | `crates/agent-link/src/setup.rs` (`ffi` exports it; the Linux app calls it) |
| The command line's side: discovery, connections, documents | `crates/cli/src/mcp/app.rs` |
| The tools (shared with the web page) | `crates/cli/src/mcp/tools/browser.rs` (`Surface`) |
| Tests | `crates/agent-link/tests/link.rs`; `crates/cli/tests/app.rs` (the real `neoscad mcp` against a test app over the real socket) |
| The Windows app's host and controls | `windows/NeoSCAD.Host/AgentConnection.cs`, `AgentDocumentHost.cs`, `AgentSetup.cs`; `windows/NeoSCAD.App/MainWindow.Agents.cs`; tests in `windows/NeoSCAD.Tests/AgentTests.cs` and `AgentMachineTests.cs` (the real `neoscad mcp` against the C# host); `docs/windows-app.md`, "AI agents" |

### How it connects

1. Nothing happens until the user has allowed agents in the app: the app
   calls `set_allowed(true)` with the user's consent, then `start()`.
   Before that there is no socket and no thread, and `start` refuses.
2. The app listens at a fresh address of its own in this user's
   rendezvous (below): a Unix socket made 0600 in a 0700 directory, or a
   named pipe whose security descriptor admits this user alone.
3. `neoscad mcp` looks there once at startup, before it answers
   anything (so the first `tools/list` already has the app's tools and
   the instructions their sentence), then every 2 s on a thread of its
   own: a directory listing, nothing measurable. It connects to every
   app it finds, checking the socket's owner and directory (the pipe's
   owner on Windows) before it sends anything.
4. The command line sends `welcome` (its version, the protocol version
   and, once `initialize` has given it, the MCP client's name); the app
   sends `hello` (app, version, platform, protocol, its open documents).
   Then come requests `{id, method, params}` and their answers, as with
   the web page, one JSON message per line (32 MiB at most) instead of
   WebSocket frames.
5. When the first app connects or the last one goes, the server sends
   `notifications/tools/list_changed` (it declares `listChanged: true`
   only when it looks for apps). Claude Code refetches the list on it
   ([Claude Code MCP](https://code.claude.com/docs/en/mcp), "Dynamic tool
   updates", as cited by the audit; not re-checked here). Once an app has
   connected in a session, its tools stay callable while unlisted, so a
   call made while the app restarts waits for it instead of failing as
   an unknown tool.

Restarts need nothing: a request made while no app is connected waits up
to 5 s for one (the user may be restarting it), looking again every
200 ms, and the restarted app's new address is found by the next look.
A new agent session connects the same way. Nothing is keyed to a session
or a token.

### Where an app listens

| Platform | Directory (Unix) or pipe | Notes |
|---|---|---|
| macOS | `~/Library/Application Support/NeoSCAD/run/app-<random>.sock` | Not `$TMPDIR`, which an MCP client may not pass on. When a long home path would make the socket path longer than 103 bytes (`sun_path`), `/tmp/neoscad-<uid>/`, which the command line searches too |
| Linux | `$XDG_RUNTIME_DIR/neoscad/app-<random>.sock`; without `XDG_RUNTIME_DIR`, `/tmp/neoscad-<uid>/` | Never an abstract socket: those have no permission checks, and the Flatpak's `--share=network` reaches them all ([Flatpak sandbox permissions](https://docs.flatpak.org/en/latest/sandbox-permissions.html)) |
| Flatpak | `$XDG_RUNTIME_DIR/app/org.neoscad.NeoSCAD/app-<random>.sock` | Sandboxed when `FLATPAK_ID` is set or `/.flatpak-info` exists. A host `neoscad mcp` searches this directory too |
| Windows | `\\.\pipe\neoscad-<SID>-app-<pid>-<random>` | The command line lists the pipe namespace for its user's prefix. Each app process (one per window) has its own |

`NEOSCAD_AGENT_DIR` replaces the search and listen places with one
directory (on Windows, a tag in the pipe names), as the tests use. The
name is random so that two app processes never collide, including two
Flatpak sandboxes that both see their app as process 2. An app removes
its socket when it stops, and on start removes sockets in its own
directory that nothing answers at (left by a crash).

**The Flatpak.** The Flatpak page says the sandbox has "no access to any
host files except the runtime, the app, `~/.var/app/$FLATPAK_ID`, and
`$XDG_RUNTIME_DIR/app/$FLATPAK_ID`. Only the latter two being writable"
(retrieved 2026-10-02), which reads as the host's own directory made
visible in the sandbox. That a host `neoscad mcp` reaches a socket the
sandboxed app made there is **unverified**: no Linux machine with Flatpak
was used for this work. A `neoscad` run inside the sandbox (`flatpak run
--command=neoscad org.neoscad.NeoSCAD mcp`) sees the same directory, so
if the host cannot reach it, that command in the client's config is the
way.

### Documents, and which one a request acts on

The app registers each open document (`document_opened(id, file, path)`,
again after Save As), says when its window is focused
(`document_focused`, stamped with the wall clock so that documents of
several app processes order together) and when it is closed. A request
without `document` acts on the most recently focused document of all
connected apps: the one the user is looking at when they type "make the
teeth smaller" into their agent. The agent can name another: each
document has a number, given in the order the app opened them and stable
while the app runs, which `editor_read` shows ("also open (pass
document): 1 gear.scad"), and every editor and view tool takes
`document`. An agent asked to choose every time would ask the user a
question the focus already answers.

The model tools given neither `path` nor `source` run the focused
document's text (unsaved changes included) under its real path, so its
includes resolve beside it; while a document is open its directory is
readable (owner decision 5), never writable. An unsaved document runs as
a plain name under the working directory, as the web page's does. While a
model tool runs on the app's document, the app is told (`activity`), so
it can show "Claude Code is checking the model".

### The app's API

Shared by all three apps (`client::agent`, `agent_link`; through UniFFI
for Swift and C#):

- `AgentLink(host, appVersion)` makes nothing run. `setAllowed(bool)`
  (the user's consent; off stops the link and tells each agent why),
  `start() -> address`, `stop()`, `disconnect(clientId)`, `status()`,
  `setObserver(observer)`, `documentOpened(id, file, path?)`,
  `documentFocused(id)`, `documentClosed(id)`. Releasing the link stops
  it. `AgentLink.withDir(host, appVersion, dir)` listens in a test
  directory.
- `AgentHost`, which the app implements: one call per request, on the
  link's threads (the host hops to its main thread and may block there).
  `read(document) -> AgentDocumentState` (version, text, selection,
  customizer overrides, `part()` switch, last run, console lines),
  `edit(document, AgentEditRequest) -> Applied(version) | Stale(version)
  | Declined`, `reveal(document, from, to)`, `camera(document, change)`,
  `capture(document, maxSide)`, `annotate(document, lines, markers)`.
  Positions are the editor's (0-based lines, UTF-16 columns), ready for
  `agentEdit` and `revealRange`.
- `AgentObserver.statusChanged(status, sequence)`: whether agents are
  allowed and listened for, and each connected client's self-reported
  name and current activity ("is editing"); keep the highest
  `sequence`. `agentStatusLine(status)` gives the control's text
  ("Claude Code is editing", "2 agents connected").
- On `Viewport`: `captureAsShown(maxSide)` (`copy_as_shown`: the user's
  camera, grid and marks, drawn offscreen), `applyAgentCamera(change)`
  and `setAgentAnnotations(lines, markers)`, a layer of the agent's own
  that the check and measure panels' `setAnnotations` no longer
  replaces, nor the reverse.

### Security

The audit's model ("Security and privacy"), as built:

- **Same user only.** No TCP, no network listener, no token. The socket
  is 0600 in a directory that must be the user's and not writable by
  others, and the app also refuses a peer whose credentials
  (`SO_PEERCRED`, `getpeereid`) name another user, which only root could
  be. The pipe's security descriptor grants this user alone, and remote
  clients are rejected. The command line checks the socket's or pipe's
  owner before it sends anything, so another user's look-alike is never
  told anything.
- **Off until the user allows it.** No socket exists before
  `setAllowed(true)` and `start()`; every request is refused while
  consent is off (`client::agent::handle_request` checks it on each
  request, whatever connected); turning it off closes the socket and
  every connection at once.
- **Visible, and one click to end.** The status names every connected
  client and what it is doing. The name is the client's own claim (MCP's
  `clientInfo`), not an identity. `disconnect` sends `bye` with
  `reconnect: false`, and that `neoscad mcp` does not connect to that app
  again until its session or the app restarts.
- **The app only shows.** Edits go into the buffer as one undoable,
  highlighted step, on the version the agent read (a stale one is
  refused); the app saves, exports and runs nothing for the agent.
  Bounds as on the web: 32 MiB messages, captures 64 to 2048 pixels, 500
  marks and 20,000 points; and 10,000 edits a request, at most 8 agents
  and 16 requests in flight per agent.
- **Files.** Reads widen by the open documents' directories only, and
  only while they are open; writes stay in the roots (`docs/mcp.md`,
  "Safety").

Not protected against: a process running as the user, which can connect
like any agent. It can already read and write the user's files; what it
gains is the unsaved text and pictures of the view, which is why the
link is off until the user allows it, and shown while in use.

### Cost

An app with agents not allowed has no thread and no socket; allowed and
idle, one thread blocked in `accept`. `neoscad mcp` lists one to three
directories every 2 s. A request is one local socket round trip plus
the host's work: in debug builds on an Apple-silicon Mac, `read` through
the listener took 116 µs (`crates/agent-link/tests/link.rs`), and
`editor_read` from an MCP client through `neoscad mcp` to the test app
and back 0.6 to 0.8 ms (`crates/cli/tests/app.rs`).
