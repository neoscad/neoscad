# NeoSCAD web demo (front end)

The page around the wasm engine: the macOS app's editor (the CodeMirror
bundle in `apple/Editor/web`, with an injected host), console, 3D view and
inspector. Plan: `docs/web-demo-plan.md`; the worker protocol:
`docs/web-protocol.md`.

    npm ci                        # here and in apple/Editor/web
    npm test                      # unit tests (node 18+)
    npm run build                 # dist/: a mock build for development
    npm run serve                 # http://127.0.0.1:8123/try/
    npx playwright test           # e2e against dist/ (node 20+; Chromium)

The release bundle, with the real engine and viewer:

    scripts/web/build-core.sh                                   # -> dist/web-core
    scripts/web/build-view.sh --no-webgl --out dist/web-view/webgpu
    scripts/web/build-view.sh --out dist/web-view/webgl
    scripts/web/build.sh          # -> dist/web/neoscad-web-<version>-<sha>/ + tarball
    scripts/web/sync-website.sh dist/web/neoscad-web-<...>.tar.gz [WEBSITE_DIR]

`build.sh` packages the core and viewer builds; it does not build them,
so run the first three after changing Rust. `wasm-opt` (binaryen) is
optional; `WASM_OPT` names it (`build-core.sh` also takes
`WASM_OPT=docker`).

E2E against a bundle synced into a copy of the website (the real engine;
`real.spec.js` runs only then):

    E2E_DIR=SITE/try E2E_SITE=SITE E2E_SHOTS=DIR npx playwright test

| Path | What |
|---|---|
| `src/engine/protocol.js` | The worker protocol as the page speaks it: request builders, error kinds, and the conversions (editor edits to LSP positions, customizer values to tagged `ParameterValue`s, section outlines to points). The one place that knows the wire. |
| `src/engine/client.js` | The worker's lifecycle: coalesced runs, cancel and crash respawn, and the replay a new worker gets (`init` with the same seed, `addFiles`, `open`, the language server's setup). |
| `src/engine/index.js` | Which worker the build starts (`core/worker.js`, or the mock), and lazy BOSL2 (`bosl2.tar.gz`, gunzipped and sent as `addFiles`' `tar`). |
| `src/engine/mock-*.js`, `fixtures.js` | The mock engine for `npm run build` and the unit tests; it speaks the real wire, packed scenes included. |
| `src/view/index.js` | The 3D view: the WebGPU-only viewer (`view/`), the WebGL2 build (`view-webgl/`, fetched only when WebGPU is missing or fails), else `canvas2d.js`, with a notice. |
| `src/ui/`, `src/model/` | Panels, and the customizer's logic. |
| `src/agent/`, `src/ui/agent.js` | "Connect your AI agent": the page's end of `neoscad mcp --browser` (a WebSocket to 127.0.0.1, directly or through the relay window), its answers to the agent's tools, and the dialog. Design and browser matrix: `docs/agent-bridge.md`. |
| `examples/` | The picker's examples (`README.md` there has their sources and licences). |
| `e2e/` | Playwright: `app.spec.js` (either engine), `real.spec.js` (the wasm core and viewer: backends, schemes, picking, every example's timings, the heavy example's cancel), `phone.spec.js`, and `share.spec.js` (links, the embed view, framing and the top bar's widths; Chromium, Firefox and WebKit). |

## Links and embeds

The page reads its fragment (never sent to the server), `src/share.js`:

| Fragment | Opens |
|---|---|
| `#example=<id>` | A bundled example (`examples/manifest.json`), with the visitor's own edits to it if they have any. |
| `#code=<payload>[&name=<file>]` | The source in the link, as a document of its own named `<file>` (default `untitled.scad`, `.scad` added). The payload is the base64url (RFC 4648 §5, no padding) of the UTF-8 source, or `z:` and the base64url of the source deflated with raw DEFLATE (`CompressionStream("deflate-raw")`; in node, `zlib.deflateRawSync`). At most 64 KB decoded. |
| `#embed=1&code=…` or `#embed=1&example=<id>` | The embed view (`src/embed.js`), for an iframe: below. |

A `#code=` link is taken out of the address bar once read, like an
agent's `#connect=`. Its document is in the example picker as
"<file> (from the link)" while the page is open, and nothing saves it:
the visitor's examples, their edits and their last example are as they
were, and a reload opens those. A payload that is too long, not
base64url, damaged or not UTF-8 opens the usual example, with a banner
saying why. The Export menu's **Copy link** and **Copy embed link** make
such links for the document as it is (always its text, never
`#example=`, since whoever opens it may have edited that example
themselves; customizer values are not carried).

The embed view is the 3D view alone, previewed on load (a heavy example
waits for its Preview button), with "Open in NeoSCAD" opening the same
model in the whole page in a new tab. It loads no editor (so the
language server never starts), no agent bridge and no inspector, and
neither reads nor writes the visitor's storage. A blog post frames it:

    <iframe src="/try/#embed=1&code=z:…" title="…" loading="lazy"></iframe>

with `frame-src 'self'` in that page's CSP. The page refuses to run in
a frame from another origin (`framing()` in `src/share.js`): the site is
static, so it can send neither `frame-ancestors` nor `X-Frame-Options`,
and a meta CSP cannot carry `frame-ancestors`. A host that can send
headers should also send `Content-Security-Policy: frame-ancestors 'self'`
for `/try/`.

Things the page does because of the wire:

- Face colours are baked into the scene by the worker, so a colour
  scheme change runs the model again with `colorScheme`; the viewer only
  changes the background and lines itself.
- The view follows a file's `$vp*` (`fileView`) only when they change
  from the previous run, so live previews do not undo the user's orbit.
- Measurement handles do not survive a respawn: section, distance and
  pick ask for a new measurement then.
- Playwright runs the full Chromium (`channel: "chromium"`): the default
  headless shell has `navigator.gpu` but no adapter, so the WebGPU viewer
  would never be the one tested.

E2E options: `E2E_DIR` (the bundle to serve), `E2E_SITE` (a copy of the
website to serve at the root), `E2E_SHOTS` (a directory for screenshots
and `timings.json`), `E2E_OUT` (Playwright's output directory),
`NEOSCAD_BIN` (a `neoscad` built from this checkout, for `agent.spec.js`,
which skips without it; `E2E_BRIDGE_LOG=1` shows its stderr and the
page's console). `agent.spec.js` and `share.spec.js` run in Firefox and WebKit too:
`npx playwright install firefox webkit`, then
`npx playwright test agent share --project chromium --project firefox --project webkit`.
