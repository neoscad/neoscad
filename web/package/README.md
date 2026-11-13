# @neoscad/viewer

Show an OpenSCAD model in any web page. NeoSCAD's engine, compiled to
WebAssembly, runs the model in a worker, and its viewer draws the result
with WebGPU, or WebGL 2 where WebGPU is missing. You get an interactive
3D view, the console output and errors, and STL or 3MF export. There is
no server and no iframe.

The package works in browsers only, as ES modules. The design and the
whole API are in
[docs/js-package.md](https://github.com/neoscad/neoscad/blob/main/docs/js-package.md).

## Quick start: the element

```html
<script type="module" src="https://cdn.jsdelivr.net/npm/@neoscad/viewer@0.7.0/dist/element.js"></script>

<neoscad-viewer src="bracket.scad" mode="render" edges
                style="width: 600px; height: 400px"></neoscad-viewer>

<neoscad-viewer>
  <script type="text/x-openscad">
    difference() { cube(10, center = true); sphere(6.5); }
  </script>
</neoscad-viewer>
```

Attributes:

| Attribute | What it does |
|---|---|
| `src` | The model's URL, relative to the page. Its file name names the model in errors. |
| inline `<script type="text/x-openscad">` | The model's text, used when there is no `src`. |
| `mode` | `preview` (the default) or `render`. |
| `enable` | Language extensions, separated by spaces or commas: `sketch`, `query`, `exact`, `fillet`, `part`, and OpenSCAD's experimental features. |
| `color-scheme` | A render colour scheme such as `Cornfield` (the default), `Metallic` or `Sunset`. |
| `axes`, `scales`, `grid`, `edges`, `crosshairs`, `orthographic` | View settings. Writing the bare attribute turns a setting on; `axes="false"` turns off the axes, which are on by default. |
| `view` | A starting view: `top`, `bottom`, `left`, `right`, `front`, `back` or `diagonal`. |
| `backend` | `auto` (the default), `webgpu`, `webgl` or `canvas2d`. |
| `assets-url` | Where the package's `dist/assets/` is served, if not beside `element.js`. |
| `name` | The model's file name, if `src` does not give one. |

Changing an attribute runs the model again. The view attributes only
redraw. The element fires `neoscad-result` after each run, with the
result as `detail`, and `neoscad-error` when the model or the engine
fails. It has a `source` property (setting it replaces the model's
text) and the methods `update()`, `export(format)` and
`image(width, height)`.

All the elements on a page share one engine. An element's model runs the
first time the element scrolls into view. An element that stays off
screen for a few seconds gives its GPU context back, because browsers
allow only about 16 WebGL contexts on a page. When the element comes
back into view, it redraws with the same picture and camera.

## Quick start: the API

```js
import { render, createEngine } from "@neoscad/viewer";

const view = await render(document.getElementById("model"), "cube([20, 10, 5]);");
view.result.errors;     // [{message, path, line, column, ...}], lines from 0, UTF-16 columns
await view.update("cylinder(h = 10, r = 4);");

const engine = createEngine();
const r = await engine.run(source, { mode: "render", overrides: { width: 30 }, files: { "lib/part.scad": partText } });
const { data } = await engine.export(source, "binstl");   // a Uint8Array
```

`render()` takes the options of `createEngine`, `createViewer` and
`run`. Pass an `AbortSignal` as `signal` to cancel a run.

## Serving the assets

The assets are about 16 MB, in `dist/assets/`: the core in `core/`, the
WebGPU viewer in `view/`, and the WebGL viewer in `view-webgl/`. A
browser with WebGPU downloads about 3.2 MB gzipped (the core and the
WebGPU viewer). A page loads the WebGL viewer only
when WebGPU fails. `fonts.tar.gz` loads the first time a model draws
text, and `bosl2.tar.gz` the first time a model includes BOSL2. The
package looks for its assets beside `dist/index.js`, which works in two
cases:

- **From a CDN** (jsDelivr, unpkg). The worker is on another origin, so
  the package starts it from a `blob:` module, which may import across
  origins. If the page has a Content-Security-Policy, it must allow
  `worker-src blob:`, plus `script-src <cdn> 'wasm-unsafe-eval'` and
  `connect-src <cdn>`.
- **From `node_modules`, served as it is.**

Bundlers (Vite, webpack, esbuild) do not copy the assets. Copy
`node_modules/@neoscad/viewer/dist/assets` into the app's public folder
and pass its URL:

```sh
# Vite serves and copies public/ as it is; other bundlers have a static folder too.
cp -R node_modules/@neoscad/viewer/dist/assets public/neoscad
```

```js
render(el, source, { assetsUrl: "/neoscad/" });
```

If a CSP allows no `blob:` workers, serve the assets from the page's own
origin, or pass `worker: () => new Worker(url, { type: "module" })`. That
function must return a new worker on every call, because the engine
restarts its worker after a crash or a cancel.

## Licence

NeoSCAD's code in this package (its JavaScript and the NeoSCAD code
compiled into its WebAssembly) is under the GPL, version 2 or later, with
the Classpath exception (`GPL-2.0-or-later WITH Classpath-exception-2.0`;
`LICENSE`). In plain words:

- A page or app that imports or bundles the package unmodified may use
  any licence for its own code.
- Changes to the package itself stay under the GPL.
- The GPL's source offer still applies to the package: `SOURCE.txt` says
  where the source of this build is.

The third-party parts keep their own licences, which
`THIRD-PARTY-LICENSES.txt` lists: MCAD (LGPL-2.1), the Liberation fonts
(OFL-1.1), BOSL2 (BSD-2-Clause), Manifold (Apache-2.0), Clipper2
(BSL-1.0), libtess2 (SGI Free Software License B), and the Rust crates.
