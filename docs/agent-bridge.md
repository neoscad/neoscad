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
   once. It opens `ws://127.0.0.1:PORT/ws?token=TOKEN`.
4. If the browser will not allow that (below), the dialog says so and
   offers **a connection window**. That button opens
   `http://127.0.0.1:PORT/relay#TOKEN` as a popup: a page of the bridge's
   own, whose WebSocket is same-origin. It passes messages to and from the
   tab with `postMessage`.
5. The bridge sends `welcome` (the MCP client's name, so the page says
   "Claude Code connected"). The page sends `hello` (its file and
   browser). From then on the bridge sends requests (`read`, `edit`,
   `reveal`, `camera`, `capture`, `annotate`, `console`) and the page
   answers.

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
