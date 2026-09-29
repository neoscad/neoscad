# NeoSCAD web demo (front end)

The page around the wasm engine: the macOS app's editor (the CodeMirror
bundle in `apple/Editor/web`, with an injected host), console, 3D view and
inspector. Plan: `docs/web-demo-plan.md`.

    npm ci                        # here and in apple/Editor/web
    npm test                      # unit tests (node 18+)
    npm run build                 # dist/: a mock build for development
    npm run serve                 # http://127.0.0.1:8123/try/
    npx playwright test           # e2e against dist/ (node 20+; Chromium)
    ../scripts/web/build.sh       # the release bundle in ../dist/web/

| Path | What |
|---|---|
| `src/engine/protocol.js` | The worker protocol as the page assumes it: request builders and normalisers. Aligning with `docs/web-protocol.md` changes this file and the mock. |
| `src/engine/client.js` | The worker's lifecycle: coalesced runs, cancel and crash respawn, replay. |
| `src/engine/mock-*.js`, `fixtures.js` | The mock engine used until the wasm core is built. |
| `src/view/` | The 3D view: the WebGPU viewer's wrapper, and a canvas fallback. |
| `src/ui/`, `src/model/` | Panels, and the customizer's logic. |
| `examples/` | The picker's examples (`README.md` there has their sources and licences). |

E2E options: `E2E_DIR` (the bundle to serve), `E2E_SITE` (a copy of the
website to serve at the root), `E2E_SHOTS` (a directory for screenshots).
