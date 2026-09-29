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
| `e2e/` | Playwright: `app.spec.js` (either engine), `real.spec.js` (the wasm core and viewer: backends, schemes, picking, every example's timings, the heavy example's cancel), `phone.spec.js`. |

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
page's console). `agent.spec.js` runs in Firefox and WebKit too:
`npx playwright install firefox webkit`, then
`npx playwright test agent --project chromium --project firefox --project webkit`.
