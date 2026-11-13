# The npm package: `@neoscad/viewer`

`@neoscad/viewer` lets any web page render a SCAD model. It contains the
same wasm engine and viewer as the `/try` demo, an API and a
`<neoscad-viewer>` custom element. It is browser-only ESM.
`web/package/README.md` is the user's quick start. This document covers
the design and how to build it.

## Layout

| Path | What |
|---|---|
| `web/package/src/` | The package's own modules: `engine.js` (engine and models), `scheduler.js` (the per-model queue), `result.js` (result shape and positions), `assets.js` (asset URLs and the cross-origin worker), `viewer.js`, `render.js`, `element.js`, `attributes.js`, and hand-written `index.d.ts` / `element.d.ts` |
| `web/src/engine/*`, `web/src/view/*` | The demo's worker client and backend chooser. The package imports them; it does not copy them |
| `web/package/build.mjs` | esbuild bundles plus the assets into a package directory |
| `scripts/web/build-package.sh` | The real package: core, viewers, fonts, BOSL2, licences, `SOURCE.txt`, the local-path scan, `npm pack` |
| `scripts/web/common.sh` | What `build.sh` and `build-package.sh` share: reproducible tars, the fonts and BOSL2 archives, the third-party notices, the local-path scan |
| `web/package/test/` | Node unit tests against the demo's mock core |
| `web/package/e2e/` | Playwright: test pages on two origins (`serve.mjs`), a bundled app, mock and real builds |

The package version is the workspace version (`[workspace.package]` in
`Cargo.toml`), which `build.mjs` writes into the package's
`package.json`. The engine reports the same version in `init`. `Engine`
warns in the console when the two differ, which means the assets came
from another release. `web/package/package.json` itself is `private`
with version `0.0.0-dev`, so the source directory cannot be published
by mistake.

## Package contents

```
package.json  README.md  LICENSE  THIRD-PARTY-LICENSES.txt  SOURCE.txt
dist/index.js  dist/element.js  dist/index.d.ts  dist/element.d.ts
dist/assets/core/{worker.js, neoscad_web.js, neoscad_web_bg.wasm}
dist/assets/view/{web_view.js, web_view_bg.wasm}          WebGPU
dist/assets/view-webgl/{web_view.js, web_view_bg.wasm}    WebGL2, fetched only when WebGPU fails
dist/assets/fonts.tar.gz                                    fetched on the first text()
dist/assets/bosl2.tar.gz                                    fetched on the first include <BOSL2/...>
```

`dist/element.js` imports `./index.js` (`build.mjs` keeps that import
external). A page that loads both therefore gets one copy of the code
and one shared engine. The bundles are minified for syntax and
identifiers but not whitespace, because esbuild drops the
`webpackIgnore` / `@vite-ignore` comments on the viewer's run-time
`import()` when it minifies whitespace. `build.mjs` fails if those
comments are missing.

Sizes for 0.7.0, built without wasm-opt: 16.1 MB unpacked in 18 files,
and a 7.7 MB tarball. The core wasm is 8.7 MB (3.0 MB gzipped), the
WebGL viewer 3.1 MB (1.1), the WebGPU viewer 0.5 MB (0.2), the fonts
2.3 MB and BOSL2 1.0 MB. The JS is 56 KB plus 11 KB.

## API

```js
import { createEngine, createViewer, render, sharedEngine } from "@neoscad/viewer";

const view = await render(target, source, options);   // target: a canvas, or a container
view.update(newSource, runOptions); view.result; view.export(format); view.dispose();

const engine = createEngine({ assetsUrl, seed, limits, worker, staleMs, lazyFonts });
await engine.ready;                   // {version, limits, libraryDirs}; rejects with EngineStartError
engine.run(source, runOptions);       // one run, never coalesced
engine.export(source, format, opts);  // {data: Uint8Array, ok, exitCode, failure, mime, messages}
engine.parameters(source);            // customizer groups
const model = engine.model({ name, camera });   // a kept document: runs coalesce latest-wins
engine.dispose();

const viewer = await createViewer(target, { assetsUrl, backend, settings });
viewer.setScene(r.scene); viewer.preset("top"); viewer.viewAll(); viewer.resetView();
viewer.camera(); viewer.setFileView(r.fileView); viewer.setSettings({ edges: true });
viewer.image(w, h, { type }) /* Blob */; viewer.kind; viewer.notice; viewer.dispose();

viewer.supportsTransforms;                          // false on the canvas2d fallback
const part = viewer.addScene(r.scene, { matrix }); // beside the others
part.setMatrix(matrix); part.remove();              // 16 numbers, column-major, rigid
const loop = viewer.onFrame((ms, step) => ...);    // loop.pause(), play(), stop()
```

A setting the demo does not use, `background` (`"#rrggbb"`, or `[top,
bottom]` for a vertical gradient; `null` for the scheme's), draws the
view's background in the page's colours instead of the colour scheme's
(`Viewer.setBackground` in `crates/web-view`). A view that stands in for
an image, such as neoscad.org's hero over its still, needs it to fade in
without a seam. The canvas2d fallback ignores it.

Run options are `mode` ("preview" | "render"), `enable` (extension
names; `part` becomes the wire's `parts`), `overrides` (plain JS values,
converted to the tagged `ParameterValue`), `files` (paths relative to the
model, as strings or bytes), `colorScheme`, `signal` and `name`.

A result has this shape:

```js
{ ok, exitCode, summary, scene, console: [{kind, text, location}],
  errors: [Message], warnings: [Message], bbox: {min, max} | null,
  geometry, fileView, timings }
Message = { severity, message, text, path, line, column, endLine, endColumn, code, hints }
```

`result.js` derives the result from the raw run result, not from the
demo's `runResult`, because `runResult` drops `render.diagnostics`. The
`errors` and `warnings` lists come from the diagnostics, by severity,
plus the console's error and warning lines that no diagnostic already
covers (the core prints a diagnostic's `text` word for word as a console
line, so duplicates are found by text). All positions use one
convention: 0-based lines and UTF-16 columns, which is the protocol's
`ConsoleLine` convention and the repo's editor-position rule. A
Diagnostic span has 1-based lines and 1-based byte columns, so the
package converts it through the text of the file it names. The package
has that text for the model and for its `files`. For other files it
subtracts one from the byte column, which is right for ASCII lines.
Paths are given relative to the model's directory. `bbox` comes from the
packed scene's `meta`, since `render.geometry` is null for previews.

`export` rejects with `ExportError` (carrying `failure`) when no file
was written. A file written with errors, for example after a failed
fillet, comes back with `ok: false`. `creationDate` defaults to the
protocol's `1970-01-01T00:00:00Z`, so an export gives the same bytes on
every visit. The demo still passes the current time. The default seed is
0 for the same reason. The demo keeps `Date.now()`.

## Models and the queue

All wire requests name paths. Each model gets its own directory,
`/doc/m<n>/`, with the document inside it named after the model (the
`src` basename, else `main.scad`), so its relative includes and its
`files` resolve there. The first run sends `open`, and later runs send
`update` when the text changed. A file is sent with `addFiles` only when
its content changed. `dispose` sends `close`. Before each run the model
fetches the lazy libraries that the source or its `files` include. When
the core reports `fontsWanted`, the model fetches the fonts and runs
again. `engine.run`, `export` and `parameters` share one scratch
directory, `/doc/run/`. Their jobs run one at a time, so they cannot
collide.

The demo's `EngineClient` assumes one page and one document. It has a
single latest-wins slot, a stale-run kill that fires for any waiting
run, and a cancel that restarts the worker and fails every pending
request. Shared by several elements, it would let one element's edit
replace another element's waiting run, and one element's cancel kill
another element's run. `scheduler.js` therefore sits in front of the
client and gives every job a key, which is its model:

- Jobs start one at a time, first in, first out across models.
- A newer job of a model replaces that model's waiting job, which
  resolves `{superseded: true}`, and keeps its place in the queue.
- A stale kill (`staleMs`, off by default) fires only when a newer run
  of the same model waits.
- Aborting a waiting job drops it. Aborting the job in flight restarts
  the worker, and the other models' jobs, which were never sent, go
  next. The client replays documents and files to the new worker.
- A job that failed because the worker restarted is retried once,
  unless its own run crashed: that model would crash the engine again.

A job covers a model's whole turn (update, files, libraries, run, fonts
and rerun), so no other model's requests come between those steps. The
client is constructed with `staleMs: Infinity`, so its own kill never
fires.

## Assets and the worker

The default `assetsUrl` is `./assets/` beside the bundle
(`import.meta.url`). This works with no bundler, from a CDN, and from
`node_modules` served as it is. The URL is built from a variable rather
than a literal because Vite and webpack rewrite
`new URL("literal", import.meta.url)` as an asset to bundle.

Browsers refuse `new Worker(url)` for another origin. When the worker's
URL is on another origin, the package starts a same-origin `blob:`
module containing `import "<url>"`. The worker's own `import.meta.url`
is then the CDN's, so its wasm resolves there. The engine makes one blob
URL, keeps it for its lifetime and revokes it in `dispose`. Both wasm
loaders already fall back when a server sends the wrong MIME type (the
e2e's second origin serves `.wasm` as `application/octet-stream`).
jsDelivr and unpkg send `Access-Control-Allow-Origin: *` and
`application/wasm` (checked in the plan's review; not re-checked here).

A page with a CDN and a CSP needs `worker-src blob:`,
`script-src <cdn> 'wasm-unsafe-eval'` and `connect-src <cdn>`.
Otherwise it can self-host the assets or pass `worker`, which must
return a new worker on every call. When no worker ever answers `init`,
the client gives up after a burst of restarts, and the engine's error
names the worker URL, CORS and the CSP.

## Several scenes in one viewer

A packed scene has no object identity: its draws are grouped by render
state (`docs/web-protocol.md`, PackedScene), so a viewer cannot move one
object inside it. To animate a mechanism without evaluating it per frame,
a page renders each rigid part as a scene of its own and places each one:
`addScene` returns a handle, and `handle.setMatrix(m)` moves the scene.

- **The viewer.** `crates/web-view` has `addModel(faces, edges, meta) ->
  id`, `setModelTransform(id, Float32Array)` and `removeModel(id)`.
  `setModel` and `clearModel` keep their meaning: they replace all the
  models with one, or with none. The models live in `render::viewport`
  (`Viewport::add_model`, `set_placement`, `remove_model`), so the
  bookkeeping and the drawing are tested natively, on the machine's GPU
  (`crates/render/tests/viewport.rs`).
- **The transform.** `render::placement::Placement` is rigid only: a
  rotation and a translation, checked to 1e-3 (f32 matrices composed in
  JavaScript pass). Each model's matrix is folded into its own copy of
  the frame uniforms, with `clip_from_model` times the matrix and the
  normal matrix times its rotation. A rotation's inverse transpose is
  itself, so the rotation carries the normals. A scale or shear would need
  the true inverse transpose, and a mirror would flip the winding that
  face culling relies on, so both are refused. The shader is unchanged.
- **Drawing.** While no model has been added, frames go through
  `Renderer::draw` exactly as before; the desktop apps never add one.
  With added models, `Renderer::draw_placed` draws them all in one pass,
  in the same order as a single scene (background, lines under, every
  model's faces, every model's outlines, lines behind and over). A model
  at the identity gives the same pixels as the same model shown with
  `setModel`, and a test checks this. The uniform buffers are kept per
  target and rewritten with `queue.write_buffer`, so moving a model
  uploads nothing and makes no uniform buffer. A move marks the view dirty, and
  the page's next animation frame draws once.
- **Bounds.** View All, and the first fit, use every model's box moved by
  its current matrix (the box of the eight moved corners).
- **Image-space CSG.** A preview of a difference or an intersection draws
  each CSG product in a pass of its own, into buffers of its own. Such a
  scene cannot be added, and nothing can be added beside one shown with
  `setModel` (`NotPlaceable`). Render mode has no such products.
- **Picking.** `rayAt` gives a ray in the view's coordinates. The
  engine's picking knows only the scene as it was built, at the
  identity, so for a moved scene the caller must carry the ray into the
  scene's own coordinates by the inverse matrix. The package does not
  pick.
- **The canvas2d fallback** does not place scenes: `supportsTransforms`
  is false and `addScene` throws. A page then shows something else, as
  neoscad.org's hero keeps its still image.
- **`onFrame`** (`src/frames.js`) is a `requestAnimationFrame` loop that
  stops while the canvas is off screen (IntersectionObserver) or the tab
  is hidden. Its clock counts running time only, with steps capped at
  100 ms, so an animation resumes where it stopped.

## Many elements

Each element makes its viewer only when it is on screen with a non-zero
size (an IntersectionObserver plus a ResizeObserver). It disposes the
viewer after `OFFSCREEN_MS` (4 s) off screen, keeping the last scene and
camera, and restores them when it comes back. This matters because
browsers cap WebGL contexts per page (Chromium drops the oldest beyond
16). An element's model runs the first time the element comes into
view. Ten elements on one page share one engine, and the e2e checks it.

Building the package exposed a bug in the shared viewer code: making
several viewers at once called wasm-bindgen's `init` concurrently, which
instantiated the module twice and left the first viewers dead or on
WebGL. `view/index.js` now starts each viewer module's init once
(`initOnce`).

## Changes to the shared modules

Each change defaults to the demo's behaviour:

- `engine/index.js` `createEngine(build, base, {seed, limits, spawn, lazyFonts, staleMs})`.
- `engine/protocol.js` `Requests.init(seed, limits)`: limits are sent
  only when given.
- `engine/client.js`: `limits` goes with every `init`, including
  replays. A new `update(path, text)`. The replay keeps only the latest
  `addFiles` copy of each path, and the latest archive of each library.
- `view/index.js`: a `backend` option narrows the attempts. The dynamic
  import carries the bundler-ignore comments. `initOnce` (above).

## Build, test, release

    scripts/web/build-core.sh
    scripts/web/build-view.sh --no-webgl --out dist/web-view/webgpu
    scripts/web/build-view.sh --out dist/web-view/webgl
    scripts/web/build-package.sh          # -> dist/npm/neoscad-viewer-<version>.tgz

    cd web/package && npm ci && npm test
    node build.mjs --mock --out build-mock && npx playwright test        # needs web/'s npm ci, Node 20+
    E2E_PACKAGE_REAL=1 E2E_PKG_DIR=../../dist/npm/neoscad-viewer-<version> npx playwright test

The e2e serves the test pages on one origin and the package on a second
origin, which plays the CDN. It covers `render()` self-hosted and
cross-origin (the blob worker), errors with positions, a binary STL
export, an abort that leaves another model alone (mock only), ten
elements sharing one engine, attribute changes, and an app bundled with
esbuild that passes `assetsUrl`. Firefox and WebKit run when Playwright
has them installed. The mock build ships no viewer, so it draws with the
canvas fallback. The release steps are in `docs/release.md` ("The npm
package").

## Licence

The package is `GPL-2.0-or-later WITH Classpath-exception-2.0`. This is
the owner's decision for this package only: the repository and every
other artifact remain plain GPL-2.0-or-later. Its `LICENSE` is the GPL
text followed by the exception (`web/package/LICENSE-EXCEPTION.txt`),
with a scope sentence: the exception covers the NeoSCAD code shipped in
the package (its JS and the NeoSCAD-authored code compiled into its
wasm). In plain words:

- A page or app that imports or bundles the package unmodified may use
  any licence for its own code.
- Changes to the package itself stay under the GPL.
- The GPL's source offer still applies to the package. `SOURCE.txt`
  names the commit, the BOSL2 commit and the offer.

The third-party components keep their own licences, listed in
`THIRD-PARTY-LICENSES.txt`.

## Open

Publishing is the owner's step: create the `@neoscad` org on npm, then
`npm publish` (`docs/release.md`). Deferred work is in
`docs/followups.md` ("npm package").
