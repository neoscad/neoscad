# The web worker protocol

The browser demo (`docs/web-demo-plan.md`) runs NeoSCAD in one module
worker: evaluation, geometry, check and measure, export and the language
server. The page talks to it with `postMessage` only. This document is the
contract between the worker (`crates/web`, its reference glue
`crates/web/js/worker.js`) and the page (`web/`, whose mock engine speaks
it too); change both sides together and record the change here.

The worker is single-threaded and runs one request at a time, in the
order they arrive. A request that is running cannot be interrupted from
inside (there is no shared memory without cross-origin isolation, which
v1 does not use), so **cancelling is the page's job: it terminates the
worker and starts a new one** (see "Crashes, cancelling and respawning").

## Envelope

Every request from the page is

```js
{ id: 17, type: "run", ...fields }
```

`id` is any number the page chooses (unique among requests in flight).
Every request gets exactly one reply with the same `id`:

```js
{ id: 17, ok: true, result: { ... } }                 // success
{ id: 17, ok: false, error: { kind, message } }       // failure
```

Replies come in request order. Byte buffers in a result (a scene, an
export) are `ArrayBuffer`s passed in the transfer list, so they move
without a copy; the page owns them afterwards.

The worker also sends two unsolicited messages, which have no `id`:

```js
{ type: "ready", version: "0.1.0", frameLimit, frameWeights, probe }  // send `init`
{ type: "crashed", message: "..." }        // the instance trapped; respawn
```

### Errors

`error.kind` is one of:

| kind | Meaning | The page should |
|---|---|---|
| `cancelled` | A newer request on the same document stopped this one (only possible for requests the worker itself supersedes; rare in one thread). | drop the result |
| `invalidArgument` | A bad request: not JSON, an unknown type or field shape, a request before `init`, a relative path, a bad option or limit, an unknown export format or colour scheme, a stale measurement handle, an edit whose end is before its start. `message` says which. | fix the caller |
| `failed` | The request could not be carried out (a document or file that does not exist, a malformed tar archive). | show `message` |
| `panicked` | A bug in NeoSCAD. Only when a panic could be caught; on wasm32 panics abort, so this is normally `crashed` instead. | show `message`, respawn |
| `crashed` | The wasm instance trapped (a panic, out of memory, the host's stack exhausted). Sent by the glue, followed by a `crashed` message. | respawn |

A model that fails (a syntax error, an empty result, a resource limit) is
**not** an error: the request succeeds with a non-zero `exitCode`, and the
reason is in `diagnostics` and `console`.

## Paths and documents

Paths are absolute, `/`-separated, and exist only in the worker's memory:

- `/doc/` is where the page puts documents (the default is
  `/doc/main.scad`). Relative `include`s and `import`s resolve against the
  document's directory, as on the desktop.
- `/neoscad/libraries/` is on the library path. MCAD is compiled in there;
  BOSL2 is added with `addFiles` under `/neoscad/libraries/BOSL2/`.
- Fonts (Liberation) are compiled in.

A document is open from `open` until `close`; its text is what every
request on its path reads (includes of it too).

## Requests

Field names are camelCase. `?` marks an optional field (omitted or
`null`); `u32`/`u64` are non-negative integers, sent as JS numbers.

### `init`

```js
{ type: "init", limits?: ResourceLimits, seed?: u32 }
→ { version: "0.1.0", limits: ResourceLimits, libraryDirs: ["/neoscad/libraries"] }
```

Must be the first request after `ready`; any other request before it
fails with `invalidArgument`. `seed` is the seed of unseeded `rands()`
(default 0; the page should pass one of its own, e.g. `Date.now() >>> 0`,
kept across respawns, so results differ between visits but stay stable
while the page lives, as in the app). `limits` defaults to the agent
limits with memory at 1 GiB (below). `init` again is `invalidArgument`.

`ResourceLimits` (every field but `depth` `null` for unlimited):

```js
{ timeSeconds: 60, memoryBytes: 1073741824, fragments: 10000, slices: 10000,
  list: 10000000, string: 67108864, rands: 10000000, triangles: 10000000,
  depth: null }
```

`depth` is how many user module calls (and, with the heap evaluator, user
function calls) may be in progress inside one another before evaluation
stops with OpenSCAD's "Recursion detected" error. It cannot be turned
off: `null` (or leaving it out) is the default, 100,000, and 0 is
`invalidArgument`.

The time limit runs on `performance.now()`.

### `setLimits`

```js
{ type: "setLimits", limits: ResourceLimits } → { limits: ResourceLimits }
```

`timeSeconds` must be a positive finite number or `null`.

### `open`, `update`, `edit`, `close`

```js
{ type: "open", path, text? }      → DocInfo
{ type: "update", path, text }     → DocInfo      // replace the whole text
{ type: "edit", path, edits: [Edit] } → DocInfo
{ type: "close", path }            → { closed: bool }

DocInfo = { path: "/doc/main.scad", version: 3, length: 1234 | null }
Edit    = { start: Position, end: Position, text: "..." }
Position = { line: 0, character: 4 }   // 0-based line, UTF-16 column (LSP's)
```

`open` without `text` tracks a file already in memory (added with
`addFiles`). Edits apply in order, each to the result of the previous one;
positions are converted to bytes by `lang::source` (a column past the
line's end clamps to it).

### `run`

The document loop: one evaluation per pause in typing feeds the console,
the view, the editor's markers and (through `parameters`) the customizer.

```js
{ type: "run", path, mode: "preview" | "render" | "force",
  overrides?: [ParameterOverride], parts?: bool, enable?: [string],
  camera?: { vpt: [x,y,z], vpr: [x,y,z], vpd: number, vpf: number },
  scene?: bool,                      // default true
  colorScheme?: "Cornfield",         // the scene's colours; default Cornfield
  previewer?: "openCsg" | "thrownTogether" }   // preview only; default openCsg
→ {
    render: RenderResult,
    console: [ConsoleLine],
    files: [string],                 // files the run read besides open documents, sorted
    language: [string],              // LSP notifications (JSON-RPC text) to feed the client
    scene: PackedScene | null,       // null when `scene: false`, or the run failed with nothing to draw
    fileView: FileView | null        // the `$vp*` the file assigned, when it assigned any
  }
```

- `overrides` are the customizer's values, appended to the text as `-D`
  assignments are; a name that is not an identifier or a value that is not
  finite is dropped.
- `parts` turns on neoscad's `part()` extension (`--enable part`); `enable`
  lists OpenSCAD's experimental features as `--enable` names them
  (`textmetrics`, `object-function`, ...).
- `camera` is the view the model is shown in, for `$vpt`, `$vpr`, `$vpd`,
  `$vpf` (OpenSCAD's GUI passes its view the same way). Default:
  OpenSCAD's default camera.
- `colorScheme` names one of OpenSCAD's render colour schemes, exactly as
  `render::scheme::all()` lists them (`assets/color-schemes/render`:
  `Cornfield`, `Metallic`, `Sunset`, `Tomorrow Night`, `Nord Dark`, ...).
  The colours are baked into the face vertices, so changing the scheme
  means running again. An unknown name is `invalidArgument`.
- `language` holds the `textDocument/publishDiagnostics` notifications for
  the diagnostics of exactly the text this run read, for the editor's
  language client, when that client has opened the document (`lsp`);
  otherwise it is empty and they go out with the client's next `didOpen`
  or `didChange` reply. The server never evaluates by itself
  (`host_diagnostics`).
- `files` are the includes, `use`d libraries, imports and fonts the run
  read (all in memory here, bundled MCAD included): what the page would
  have to re-send to change them.
- `fileView` has only the fields the file assigned:
  `{ vpt?: [x,y,z], vpr?: [x,y,z], vpd?: number, vpf?: number }`. The
  app moves its view only when these change from the previous run's, so a
  live preview does not undo the user's orbit; the page should do the
  same.

`RenderResult`:

```js
{ exitCode: 0, diagnostics: [Diagnostic], echo: ["ECHO: 1"], console: "the stderr text",
  geometry: GeometryStats | null,    // null for a preview or an empty result
  cacheEntries: 12, timings: Timings }

Timings = { parseMs, evaluateMs, geometryMs, totalMs }
  // a run's geometryMs and totalMs include building and packing its
  // scene: a preview's CSG products are real booleans, often most of it
  // (a re-preview reuses the products an edit did not change)

GeometryStats = { dimensions: 2 | 3, bboxMin: [..], bboxMax: [..], area,
  volume?, triangles?, vertices?, manifold?, components?,  // 3D
  contours? }                                              // 2D

Diagnostic = { code: "syntax-error", severity: "error" | "warning" | "deprecated",
  message, text,                     // text: OpenSCAD's line, word for word
  file: string | null, line: u32 | null,
  span: Span | null,                 // 1-based lines, 1-based byte columns, end exclusive
  hints: [{ message, replacement: { span: Span, text } | null }],
  trace: [string] }
Span = { startLine, startColumn, endLine, endColumn }
```

`ConsoleLine` (every line of the console, in order, with editor positions
for click-to-jump):

```js
{ kind: "error" | "warning" | "deprecated" | "echo" | "trace" | "info",
  text: "WARNING: ... in file main.scad, line 3",
  location: { path, startLine, startCharacter, endLine, endCharacter } | null }
                                     // 0-based lines, UTF-16 columns, end exclusive
```

### `PackedScene`

What the viewer draws: `render::packed::PackedScene` (`Scene::pack`,
`crates/render/src/packed.rs`) in its wire form, which the page hands to
`crates/web-view`'s viewer as it is:
`viewer.setModel(new Uint8Array(faces), new Uint8Array(edges), meta, generation)`.

```js
{
  faces: ArrayBuffer,   // PackedScene::faces: every face vertex, 44 bytes each (FACE_VERTEX_SIZE)
  edges: ArrayBuffer,   // PackedScene::edges: every 2D outline segment, 24 bytes each (EDGE_SEGMENT_SIZE)
  meta: string          // PackedMeta::to_json(): JSON text, snake_case (below)
}
```

`meta` parses to `PackedMeta`:

```js
{
  draws: [{ first: u32, count: u32,            // a range of face vertices
            state: { cull: "None" | "Front" | "Back",
                     depth: "Less" | "LessEqual" | "Equal" | "Always",
                     color_write: bool, bias: bool } }],
  image_csg: [{ at_draw: u32,                  // draws that come before the product
                primitives: [{ first: u32, count: u32,
                               op: "Intersection" | "Subtraction", id: u32 }] }],
  bbox: [[x, y, z], [x, y, z]] | null,         // min, max: what View All fits
  edge_color: [r, g, b, a]                     // 0..1, the colour 2D outlines are drawn in
}
```

- A face vertex is little-endian: position (3 × f32), normal (3 × f32),
  colour (4 × f32), then four barycentric bytes (0 or 1; the fourth
  unused). A zero normal marks an unlit vertex. Faces come surface by
  surface in `draws` order, then each image-space CSG product's primitives,
  whose colour's first component is the primitive's `id` (from 1).
- An edge segment is its two end points (2 × 3 × f32, little-endian),
  2D outlines at z = 0, drawn as 2-pixel lines.
- The bytes are exactly what `render::gpu` uploads on the desktop, so the
  viewer copies them into vertex buffers and nothing else;
  `Gpu::upload_packed` (behind `setModel`) checks lengths and draw ranges
  (`PackedScene::validate`) before anything reaches the GPU.
- `meta` stays a string so the page passes it through without knowing its
  shape; this is the one place the protocol is snake_case, because it is
  the renderer's own serde form.

### `parameters`

```js
{ type: "parameters", path } → { groups: [ParameterGroup] }

ParameterGroup = { name: "Dimensions", parameters: [Parameter] }
Parameter = { name, description, control: ParameterControl, defaultValue: ParameterValue }
ParameterValue = { kind: "bool", value: true } | { kind: "number", value: 3 }
               | { kind: "text", value: "abc" } | { kind: "vector", value: [1, 2] }
ParameterControl = { kind: "checkbox" }
  | { kind: "slider", min, max, step: number | null }
  | { kind: "spinBox", min: number | null, max: number | null, step: number | null }
  | { kind: "text", maxLength: u32 | null }
  | { kind: "vector", min: number | null, max: number | null, step: number | null }
  | { kind: "dropdown", options: [{ label, value: ParameterValue }] }
ParameterOverride = { name, value: ParameterValue }
```

From the document's current text. Groups are in file order; "Global"
parameters appear in every group and "Hidden" ones in none, as in
OpenSCAD's customizer.

### `check`

```js
{ type: "check", path, options?: CheckOptions, run?: RunOptions } → CheckReport

RunOptions   = { overrides?: [ParameterOverride], parts?: bool, enable?: [string] }
CheckOptions = { nozzle: 0.4, minWall: 0.8, maxOverhang: 45, bed: [w, d, h] | null,
                 bedTolerance, maxFindings }        // default: `neoscad check`'s
CheckReport  = { exitCode, failed: bool, errors: u32, warnings: u32, info: u32,
  findings: [{ id: u32, severity: "error" | "warning" | "info", code, message,
               part: string | null, point: [x,y,z] | [], bboxMin: [..] | null,
               bboxMax: [..] | null, fix, value: number | null, limit: number | null }],
  truncated: [{ code, count: u32 }], minWall: number | null, parts: [string],
  text: "neoscad check's report", summaryJson: "neoscad check --format json",
  diagnostics: [Diagnostic], console: string }
```

`{ type: "defaults" }` returns `{ checkOptions: CheckOptions, limits: ResourceLimits,
tables: { consoleGroups, printerPresets, exportFormats, previewDelayMs } }`
(the worker's default limits, and the tables every app shows, from
`client`'s `present.rs`) so the page does not copy them. A `run` result
also carries `summary` (the console's one-line summary, as
`client::describe_render` words it) and `timingsText` (its tooltip).
`options` are validated (positive nozzle and wall, overhang 0 to 90,
three positive bed sizes) before anything runs: `invalidArgument`
otherwise.

### `measure`, `section`, `between`, `pick`

```js
{ type: "measure", path, run?: RunOptions } → MeasureReport
MeasureReport = { exitCode, model: SolidStats | null, components: u64 | null,
  manifold: bool | null, model2d: GeometryStats | null, parts: [PartStats],
  measurement: u32 | null,             // a handle for section/between/pick
  diagnostics: [Diagnostic], console: string }
SolidStats = { volume, area, bboxMin, bboxMax, centroid, triangles }
PartStats  = { name: "lid.hinge", instances: u32, context: "difference" | null,
               solid: SolidStats | null }

{ type: "section", measurement, axis: "x" | "y" | "z", offset: number, part? }
→ { plane: "z=5", area, perimeter, contours: u32, bboxMin: [..] | null,
    bboxMax: [..] | null, outline: [[x0, y0, z0, x1, ...], ...] }  // closed loops, model coords

{ type: "between", measurement, a: "part", b: "part" }
→ { a, b, distance: number | null, touching: bool, overlapping: bool,
    overlapVolume, pointA: [x,y,z] | null, pointB: [x,y,z] | null }

{ type: "pick", measurement, origin: [x,y,z], direction: [x,y,z] }
→ { point: [x,y,z] | null }            // first hit on the model's surface
```

The worker keeps **only the latest measurement**; a request naming an
older handle fails with `invalidArgument` (measure again). The picking
ray comes from the viewer (`ray_at`), in model coordinates.

### `export`

```js
{ type: "export", path, format: "stl" | "binstl" | "3mf" | "off" | "obj" | "wrl"
                              | "pov" | "svg" | "dxf" | "pdf",
  options?: { threemfColorMode?: "model" | "noColor" | "selectedOnly",
              threemfColor?: "#rrggbb" | name, threemfMaterial?: "color" | "baseMaterial" },
  run?: RunOptions, creationDate?: "2026-09-29T12:00:00Z" }
→ { exitCode, format, bytes: u64, mime: "model/stl",
    data: ArrayBuffer | null,          // transferred; null when exitCode ≠ 0
    geometry: GeometryStats | null, diagnostics, console, timings }
```

Renders the model and encodes it with OpenSCAD's default options; the
bytes are the command line's for the same model. `creationDate` (3MF and
PDF metadata) comes from the page, because the worker has no wall clock;
default `1970-01-01T00:00:00Z`. The model's title in 3MF and PDF
metadata is the document's file name; the page names the download
itself. A failed export (a 2D model to a 3D format, an empty model) has
`exitCode` 1, `data: null` and the reason in `console`.

### `addFiles`

```js
{ type: "addFiles", files?: [{ path, data: ArrayBuffer | string }],
  tar?: ArrayBuffer, root?: "/neoscad/libraries" }
→ { added: u32 }
```

Writes files into memory: each of `files` at its absolute `path`, and
every regular file of `tar` (an uncompressed ustar archive; gunzip
`bosl2.tar.gz` with `DecompressionStream("gzip")` first) under `root`,
e.g. `BOSL2/std.scad` → `/neoscad/libraries/BOSL2/std.scad`. Later
reads see the new files (a file written again gets a new version, so
cached parses and geometry of the old contents are not reused). Only
regular files are read from the archive (with ustar prefixes and GNU long
names); directories and links are skipped. `added` counts the files
written.
Do **not** put `data` in the transfer list: the page keeps its copy to
replay after a respawn.

### `readFile`

```js
{ type: "readFile", path } → { text: string }
```

A file as the worker reads it (an open document's text, an added file or
the bundled MCAD), for the editor's read-only library tabs after
go-to-definition.

### `lsp`

```js
{ type: "lsp", message: "<one JSON-RPC message>" } → { messages: ["<JSON-RPC>", ...] }
```

The language server (`crates/lsp`) for the editor's
`@codemirror/lsp-client`: pass each message the client sends, and each
returned message back to it. The server does not keep the session's copy
of a document (the page does, with `open`/`update`/`edit`), and its
diagnostics come from `run` (`language`), so the page runs the document
after a pause in typing, as the app does. Library files are indexed
once per worker.

## Crashes, cancelling and respawning

The page owns the worker's life:

- **Coalescing.** At most one `run` is in flight. Edits during it update
  the document; when it replies, the page runs again only if the text
  changed.
- **Cancel** (the user presses Stop, or a run is stale for longer than
  the page tolerates) is `worker.terminate()` and a new worker. There is no
  `cancel` request: the worker could only read it after the run ended.
- **Crash.** A panic aborts on wasm32, and running out of memory or of the
  engine's stack traps. The glue catches the trap, replies to the request
  with `error.kind: "crashed"`, and posts `{ type: "crashed", message }`
  (the panic message when there was one; otherwise "the engine ran out of
  memory at N MiB" for an `unreachable` trap, which is how Rust aborts on
  a failed allocation, or "the engine's stack overflowed" for V8's
  `RangeError`); the instance is unusable after that. The page terminates it and respawns.
- **Respawn** ("engine restarted"): the new worker starts empty. The page
  replays, in order: `init` (same `limits` and `seed`), every `addFiles`
  (from its kept copies), `open` of each document with its current text,
  and the language client's `initialize`, `initialized` and `didOpen`
  (or it restarts the client). Measurement handles do not survive; a
  pending `run` is sent again.
- **Memory never shrinks** in wasm: after a heavy example (the page can
  read `performance.memory` or the worker's `memoryBytes` from the
  `stats` request) respawning returns it to the browser.

```js
{ type: "stats" } → { memoryBytes: u64,   // the wasm memory's current size
                      heapBytes: u64 }    // bytes allocated and not freed; the memory
                                         // limit measures each request's peak of it
```

## Reference glue

`crates/web/js/worker.js` is a minimal module worker that implements this
envelope over the wasm-bindgen exports of `crates/web`
(`scripts/web/build-core.sh` builds them into `dist/web-core/`:
`neoscad_web.js`, `neoscad_web_bg.wasm`, `worker.js` and a
`package.json` marking them ES modules for node). It exports `start`
and `handle` too, which is how `crates/web/test/run.mjs` drives it in
node without a worker:

- it loads `neoscad_web.js` / `neoscad_web_bg.wasm` relative to itself,
  probes the worker's stack (below), posts `ready`, and for each message
  calls `Engine.handle(json, buffers)`;
- the probe: before the engine starts, throwaway instances of the module
  (each from a fresh copy of the glue, `neoscad_web.js?probe=N`) run deep
  recursions (a function, a list comprehension, a `children()` chain, a
  module through `translate`) with no frame budget until the engine's
  stack overflows, each run counting one kind of frame (statement,
  expression, comprehension, geometry module) and reading the count with
  `framesAtLastCheck`. From those the worker sets a weight per kind
  (`setFrameWeights`) under a budget of 1,000,000 (`setFrameLimit`) so
  that each kind stops at half the depth that overflowed, and none deeper
  than the defaults allow; deep recursion then ends with OpenSCAD's
  recursion error in every browser rather than a trap. `ready` carries
  the budget (`frameLimit`), the weights (`frameWeights`: statement,
  expression, call, comprehension, geometry) and what the probe found
  (`probe`: `frames` per probe and kind, or null; `weights`; `ms`).
  `start(module, { probe: false })` skips it;
- the Rust side returns the reply as JSON text in which a buffer is
  `{ "$buffer": n }`, plus the buffers; the glue swaps each placeholder for
  its `ArrayBuffer` and lists them in the transfer list;
- `addFiles`' `ArrayBuffer`s go to Rust as the `buffers` argument in the
  same way (`{ "$buffer": n }` in the request), and strings as they are.

The front end (`web/`) ships it unchanged as `core/worker.js` and speaks
this protocol through `web/src/engine/protocol.js` and `client.js`; its
mock (`web/src/engine/mock-core.js`) answers in the same shapes.
