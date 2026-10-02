# `neoscad mcp`: the MCP server

`neoscad mcp` serves NeoSCAD's agent tools over the
[Model Context Protocol](https://modelcontextprotocol.io) on stdio. It
holds one `session::Session` with the same warm caches as `neoscad
serve` (`docs/serve-protocol.md`): the tools call the server's methods
in-process (`serve::Local`), so a one-line edit re-renders in
milliseconds. The implementation is `crates/cli/src/mcp/`; the tests are
`crates/cli/tests/mcp.rs`.

## Setup

Claude Code, with `neoscad` on `PATH`:

```sh
claude mcp add neoscad -- neoscad mcp
```

`claude mcp list` then shows `neoscad: neoscad mcp - ✔ Connected`
(checked with Claude Code 2.1.283 in an isolated `CLAUDE_CONFIG_DIR`).
To allow directories besides the project's, add them as roots:
`claude mcp add neoscad -- neoscad mcp --root ~/models`.

Any client, as a JSON config (`claude --mcp-config FILE` reads this
shape):

```json
{"mcpServers": {"neoscad": {"command": "/path/to/neoscad",
                            "args": ["mcp", "--root", "/path/to/models"]}}}
```

Flags: `--root DIR` (repeatable), `--cache-mb N` (the geometry cache
budget, as for `serve`), `--log FILE` (append every message received
and sent, for debugging a client), `--limit NAME=VALUE` (repeatable:
change a resource limit; see "Safety"), `--enable FEATURE` (repeatable:
one of OpenSCAD's experimental features for every call, as the command
line's `--enable`: `textmetrics`, `object-function`, `import-function`,
`vector-swizzle`; off by default, as in OpenSCAD), `--browser` (let the
web page connect; see "The web page" below) with `--browser-url URL` and
`--open`.

## Protocol

MCP revision **2026-07-28**, the current one
(<https://modelcontextprotocol.io/specification/2026-07-28>, schema
`LATEST_PROTOCOL_VERSION = "2026-07-28"`, retrieved 2026-09-26). The
server is *dual-era* in that revision's terms (`basic/versioning`):

- A request whose `_meta` carries `io.modelcontextprotocol/protocolVersion`
  is served statelessly as 2026-07-28 says: `server/discover`, results
  with `resultType: "complete"` and `_meta` naming the server, caching
  hints (`ttlMs`, `cacheScope`) on discover, list and read results, error
  -32022 with the supported versions for any other version, and -32602
  for a request without `io.modelcontextprotocol/clientCapabilities`.
- A client that opens with `initialize` gets the legacy handshake: the
  revision it asks for if it is 2025-11-25, 2025-06-18, 2025-03-26 or
  2024-11-05, else 2025-11-25; `ping` works; resources not found are
  -32002 (-32602 for modern requests).

Framing is the stdio binding: one JSON-RPC message per line, and nothing
else on stdout. Tool calls run concurrently; `notifications/cancelled`
stops a call at its next evaluation step (or the next ring of a
primitive, slice of an extrusion or geometry node), and no answer is
sent for it. The end of input means the client is gone: the server
cancels the calls still running, sends no answers for them, and exits,
within 2 s even if one is inside a single kernel operation (which
cannot be interrupted). A dead client used to leave the server
computing as an orphan. A script that pipes requests in must read each
answer before it closes the stream.

Claude Code 2.1.283 speaks the modern protocol: it probes with
`server/discover`, then sends `resources/list`, `tools/list` and
`tools/call` with per-request `_meta` (observed with `--log`).

Why hand-rolled: the server needs about ten methods and only stdio;
the rest of the command line is synchronous threads, while the official
Rust SDK (`rmcp`) is tokio-based and, at 3.4.1 (2026-09-23), still
defaults to 2025-11-25. Nothing is added to the dependency tree (the
PNG's base64 is 20 lines).

## Tools

Eight tools, few and orthogonal. Each takes a model as `path` (a
`.scad` file) or `source` (OpenSCAD text, so an agent can iterate
without writing files); `base_dir` is where relative paths and a
source's `include`s resolve (default: the server's working directory).
Inline source is evaluated as `inline.scad` in `base_dir` (messages name
it so) and removed afterwards. `parts: true` turns on the `part()`
extension (`docs/cli-json.md`), and `verbose: true` returns the server's
full JSON result. A `path` ending in `.stl`, `.off`, `.obj` or `.3mf`
(any case) is a mesh, not OpenSCAD: `evaluate`, `render`, `snapshot`,
`check` and `measure` run `import("<path as given>");` in its place
(as `inline-import.scad` in `base_dir`), and say so: the text starts
with ``path is a mesh file: rendered as `import("out/base.stl");` ``
and the structured content (the full JSON too, with `verbose`) has
`"imported": "import(\"out/base.stl\");"`. Parsed as OpenSCAD, the
STL's first line was a syntax error, which an agent in the T2
transcript audit read as a problem in its model.

| Tool | What it answers | Extra arguments |
|---|---|---|
| `evaluate` | errors and warnings with fix hints, `echo()` output; no geometry | |
| `render` | bbox, volume, area, manifold (including edges pinched where two pieces touch, and edges an STL breaks at 32-bit precision), components; optionally writes the model | `export` (a file; format from its extension; `.stl` is ASCII STL), `overwrite` |
| `snapshot` | a PNG contact sheet as MCP image content, plus the geometry summary | `views`, `size` (default `768x768`), `diff_against` (a file) or `diff_source`, `highlight`, `issues`, `dims`, `preview`, `output` (also save the PNG; a `.png` name), `overwrite` |
| `check` | printability findings, each with location and fix; the description asks for the spec's minimum wall as `min_wall` | `bed`, `nozzle`, `min_wall`, `max_overhang` |
| `measure` | model and part bbox, volume, centroid; distance between parts, or the overlap's pieces; sections with each contour's area, bbox, hole and radii; a radius profile with crests and pitch | `part`, `between`, `section`, `axis` (`x`/`y`/`z`, default z), `center` (`[a, b]`, the axis's position, default `[0, 0]`), `profile` (`[from, to, step]` along the axis) |
| `test` | model tests (`docs/model-tests.md`); `path` is a test file or directory, `source` a test file's text | `filter` |
| `format` | `source`: the formatted text; `path`: rewrites the file (only whitespace changes) | `check` (say how many lines would change, write nothing), `diff` (with `check`: the diff itself) |
| `docs` | a builtin's reference, or with `path` a file's definitions; no name: the index | `name`, `full`, `verbose` (the whole index) |

The server's `instructions` (sent once, at discovery or `initialize`)
say when to use which: after an edit, one `check` (with `min_wall`)
gives errors, warnings, echo, geometry and printability, with no
`evaluate` or `render` first; independent calls go in parallel;
`measure` for exact numbers, `snapshot` when the shape is in doubt;
`render`'s `export` reports what it wrote. They used to list
`evaluate`, `render`, `snapshot` and `check` in turn, and agents ran
them one turn each after every edit; a turn costs seconds of model time
and a re-read of the whole context, a tool call milliseconds. The
tool list is 5,488 bytes of compact JSON as `[name, description, input
schema]` arrays, which is what `crates/cli/tests/mcp.rs` measures and
keeps under 5,500 bytes (each description under 300). What a client
receives is larger: 5,768 bytes with the keys (`name`, `description`,
`inputSchema`) and 6,059 with `annotations`, roughly 1,450-1,700 tokens
(estimated at 3.5-4 bytes a token; not measured with a tokenizer). To
make room for `measure`'s `axis`, `center` and `profile`, `base_dir`
lost its description ("Dir for includes") and others were shortened.

Arguments are checked against the schemas before a tool runs: a
wrongly typed argument (`"parts": "yes"`, `"nozzle": "big"`) or one the
tool does not take is refused with `isError` and a message naming the
argument and the type it needs; it used to be treated as absent. Errors
from the parsers the tools share with the command line name the
arguments (`` `size` must be WxH``), not command-line flags.

### Results

Every result has a short text summary, for example:

```text
ok: 1 warning
3D bbox 60 x 40 x 30 mm at [0, 0, 0]..[60, 40, 30], volume 15552 mm³, area 15952 mm², manifold, 1 component, 28 triangles
warning inline.scad:5: Ignoring unknown module 'cub' (did you mean 'cube'?)
```

(`render` of an open box with a stray `cub(2);` on line 5) and the same
facts as `structuredContent`:

```json
{"counts": {"echoes": 0, "errors": 0, "warnings": 1},
 "diagnostics": [{"code": "unknown-module", "hint": "did you mean 'cube'?", "line": 5,
                  "message": "Ignoring unknown module 'cub'", "severity": "warning"}],
 "echo": [], "exit_code": 0,
 "geometry": {"area": 15952.0, "bbox": {"max": [60.0, 40.0, 30.0], "min": [0.0, 0.0, 0.0],
              "size": [60.0, 40.0, 30.0]}, "components": 1, "dimensions": 3,
              "manifold": true, "triangles": 28, "vertices": 16, "volume": 15552.0}}
```

The structured content has the diagnostics without spans
or full text but with `column` (where the span starts; in the text,
`inline.scad:1:11`), `file` only for an included file, the geometry object of
`docs/cli-json.md`, findings without their bboxes, at most 20
diagnostics and 20 echo lines. `check`, `snapshot` and `measure` carry
the model's diagnostics and, when it echoed anything, its `echo` too
(`check` used to keep both to its text, so a `check` read as
warning-free to an agent shown the structured content). Every non-integer number in it has 6
significant digits (render and snapshot used to give Manifold's 17,
check 4 decimals and measure 6), and the text uses the same numbers;
`verbose` keeps full precision. A finding's `fix` appears once per
text: a later finding of the same code and fix has `"fix_as": id` (in
the text, `Fix: as #id`) instead. `measure` asked for a `section`,
`profile` or `between` leaves out the `model` block (`render` gives
it). `check`'s first line gives the thinnest wall as "about N mm
(sampled)": it is the thinnest sample, and the true wall can be a
little thinner (`model.min_wall.sampled`). An `overhang` finding's
point is on its steepest faces, and its message names the heights it
spans and the area steeper than `max_overhang` + 15°, with theirs
("553.74 mm² faces down at up to 90° ..., z 0 to 11.94; 41.1 mm² of it
steeper than 60° (z 11.9)"), since findings carry no bbox here: an
agent swept `max_overhang` to find a ledge the point was 5 mm from.
`docs` of an unknown name, with nothing close, says to add `path` (the
tool's argument, not the command line's `--in`). A pinched solid (two pieces touching along an edge: Manifold says
valid, an STL of it is not manifold) reads `NOT manifold` with a line
saying how many edges and where the first is, and the geometry's
`pinched` carries the `fix`. Slicers read an STL's coordinates as
32-bit floats, and a solid finely tessellated or with surfaces lying on
each other can have vertices that are distinct in 64 bits and one point
in 32: then the solid reads `manifold` but a line `not manifold as an
STL: N triangles collapse at 32-bit precision (as slicers read it),
leaving E edges ...` follows, with the first edge and the fix, and the
geometry carries `stl_precision` (`docs/cli-json.md`). It is computed
on every 3D render (no measurable cost: one sort of the vertices, the
same one the pinch check needs). A `polyhedron()` or imported mesh that
does not bound a solid is a warning at its call in `evaluate`,
`render`, `snapshot` and `check` (codes `polyhedron-inside-out`,
`polyhedron-flipped-faces`, `polyhedron-open`,
`polyhedron-not-manifold`; `docs/cli-json.md`, "Input meshes"), with
the fix as its `hint`; `check` lists them first among its findings.
OpenSCAD says nothing about an inside-out mesh, and the booleans with it
leave pinched edges, so when such a warning is present the pinch's fix
points to it (`fix the polyhedron-inside-out warning (line 12) first:
...`) instead of saying to overlap the parts: on an inside-out thread
sweep, the overlap advice sends an agent in circles, and the signed
volume is what shows the fault. For example,
`render` of a cube polyhedron with its faces counter-clockwise,
subtracted from a slab:

```text
ok: 1 warning
3D bbox 6 x 6 x 2 mm at [0, 0, 0]..[6, 6, 2], volume 104 mm³, area 184 mm², NOT manifold, 1 component, 42 triangles
not manifold as a file: 8 edges shared by more than two faces, the first at [0, 0, 1]: fix the polyhedron-inside-out warning (line 2) first: an inside-out or partly flipped polyhedron is the likely cause, since booleans with it go wrong; if these edges remain after that, overlap the parts that touch by at least 0.01 or separate them
warning inline.scad:2:31: this polyhedron is inside out: all 6 faces point inward (its signed volume is -64 mm³); booleans with it give wrong results (OpenSCAD wants each face's points in clockwise order seen from outside the solid; these are counter-clockwise. Reverse every face's point list, e.g. `faces = [for (f = faces) [for (i = [len(f) - 1:-1:0]) f[i]]]`)
```

A pinch in a result with no volume (under a millionth of a millimetre
times its area) is parts that only touch: an `intersection()` of a lid
seated on its base is the faces where they meet. Its `pinched` object
has `"touch_only": true` and the fix says `the parts only touch (no
overlap): this zero-volume result is the faces where they meet, so
nothing interferes; overlap them by at least 0.01 only if they should
be one solid` (`check`'s `not-manifold` finding has the same fix). The
usual advice ("overlap them") misled an agent inside an interference
probe; a real part's pinch keeps it.

An info-level `stl-precision` finding (faces collapse at 32-bit
precision, every edge still paired) says `so no action is needed`, and
a terse result gives it no `fix` or `fix_as` (in the text, no `Fix:`);
`verbose` keeps the fix. Its fix text read as an instruction, and an
agent spent turns on it. A wall within 0.001 mm of `min_wall` (or of
`nozzle`) is not under it: a floor modelled at exactly 1.2 mm read
"1.2 mm thick, under the 1.2 mm minimum". A wall that is under by less
than the hundredth the message rounds to is given to the
ten-thousandth ("1.1986 mm thick, under the 1.2 mm minimum").

A module from a `use`d file whose top sets `$fn`, `$fa` or `$fs` runs
without those values: special variables come from the caller, in
OpenSCAD as here (the 2026.09.23 nightly echoes `$fn = 0` from such a
module, and the file's plain variables normally). An agent's harness
file that `use`s its parts measures coarse circles this way. So `evaluate` and every tool that renders add a NeoSCAD-only
warning at the first call into such a file, code
`use-special-variables`, never printed on the console:

```text
warning asm_check.scad:5:18: `$fn = 64` at the top of base.scad doesn't apply to its modules when the file is used (OpenSCAD behaviour: special variables come from the caller) (pass `$fn` in the call (`base_part($fn = ...)`) or set it in this file)
```

A variable is left out when the call (or a call it is made from) passes
it, or when the calling file assigns it anywhere; `@expect no-warnings`
ignores this warning.

When the faces are written out as numbers, the diagnostic's hint also
carries the exact edit (`verbose: true`, and the editor's quick fix). `format` with `check` says how many lines
would change (`diff: true` returns the diff). **The structured content must stand on
its own:** Claude Code shows the model the JSON of `structuredContent`
in place of the text when a result has both (observed in the smoke test
below). `format` and `docs`, whose answer is text, send no structured
content. `snapshot` adds the PNG as an `image` content block. A model
that fails (a syntax error, an empty result) is an ordinary result that
says so; `isError: true` is for calls that could not run (a bad
argument, a path outside the roots), with a message that says how to
fix the call.

Differences from `neoscad serve`: an agent's calls do not supersede each
other (the server's `supersede: false`), because parallel calls on one
file (a `check` and a `measure`) are both wanted; the snapshot is
returned, not written, unless `output` is given; an export's or
output's directory is created.

## Safety

- **Files:** the allowed roots are the working directory and each
  `--root`, read and write; the library path (`OPENSCADPATH`, the user
  library directory) and font directories are readable. Tool arguments
  that name files are checked before the call runs, by where they
  resolve: `..` is folded, then every symlink along the path is
  followed, the last one included even when it dangles
  (`mcp::roots::resolve`), so a planted `out.stl -> ~/Library/...` is
  refused like any path outside. A write goes to the resolved path. The
  refusal names the roots and `--root`. The session's file system
  itself is fenced (`mcp::roots::RootedFs`), so a model's `include`,
  `use`, `import()` or `surface()` cannot read outside them either: the
  file simply cannot be opened.
- **Outputs never replace other files:** `render`'s `export` must have
  an export format's extension and `snapshot`'s `output` must be a
  `.png`. An existing file whose extension is not the output's (a
  `.scad` model above all, even through a link) is never replaced; an
  existing file of the same type only with `overwrite: true`. An
  output's directory is created only once every argument has been
  checked.
- **Resource limits:** every call runs under the agent limits (60 s,
  4 GiB of memory (measured and estimated), 10,000 fragments per primitive, 10,000
  slices, 10 million list elements and `rands()` numbers, 64 MiB
  strings, 10 million triangles per result; `docs/cli-json.md`,
  "Resource limits"). A model that would pass one (`sphere(10,
  $fn=100000)` once reached a 43 GB footprint) fails at once with a
  `resource-limit` error that names the limit and the flag that raises
  it; `neoscad mcp --limit fragments=50000` (or `=off`) does.
- **No network** and **no command execution:** the tools evaluate
  OpenSCAD, which has neither. The one exception is `--browser`'s
  listener on 127.0.0.1, which accepts only the web page's origin with
  the link's 128-bit key (`docs/agent-bridge.md`, "Security"), and
  `browser_connect`'s `open`, which runs the system's URL opener on the
  link.
- Writes happen only through `render`'s `export`, `snapshot`'s `output`
  and `format` on a `path`.
- Resources: `neoscad://docs` (the builtin index) and the template
  `neoscad://docs/{name}`. No prompts.

## The web page (`--browser`)

`neoscad mcp --browser` also lets the NeoSCAD web page
(neoscad.org/try) connect, so an agent works on the text the user has
open there, and sees and points at its 3D view. The design, the security
model and the browsers as tested are in `docs/agent-bridge.md`; the code
is `crates/cli/src/mcp/bridge.rs` (the bridge),
`crates/cli/src/mcp/tools/browser.rs` (the tools) and `web/src/agent/`
(the page's side). Setup:

```sh
claude mcp add neoscad -- neoscad mcp --browser
```

The server listens on 127.0.0.1 (a free port) from the start. The agent
calls `browser_connect` for the link (`https://neoscad.org/try/#connect=
PORT.KEY`) and gives it to the user, who opens it or pastes it into the
page's "Connect your AI agent" dialog. `--open` opens it in the default
browser at startup, and `browser_connect`'s `open: true` does from a call
(off by default: an MCP host starts the server with every session, and a
tab opening each time would be a surprise). `--browser-url URL` names
another copy of the page, whose origin is then the only one allowed (a
local build: `--browser-url http://127.0.0.1:8123/try/`). The link is also
on stderr.

Once a page is connected, **`evaluate`, `render`, `snapshot`, `check` and
`measure` given neither `path` nor `source` use the page's text**, with
its customizer values (as `-D` assignments) and its `part()` switch, under
the page's file name in `base_dir`; the result starts with `the web page's
gears.scad (version 12)` and has `"page": {"file", "version"}`. `format`
with neither reformats the page's text in place (one undoable edit, or
with `check` says what would change). `test` and `docs` do not use it.
Without `--browser` none of this is listed or costs context.

| Tool | What it does | Arguments |
|---|---|---|
| `browser_connect` | the link, and whether (and how) a page is connected; what to tell the user | `open`, `wait_seconds` (wait up to this long, at most 120, for a page to connect) |
| `editor_read` | the text with numbered lines, its `version`, the selection, customizer values, the last run's summary, errors and warnings | |
| `editor_edit` | changes the text as one undoable step, highlighted in the editor; refuses a stale `version` | `version` (required), `edits`: `[{old, new}]` (unique match) or `[{at: [line, col, end_line, end_col], new}]`; or `text` (all of it) |
| `editor_reveal` | selects and scrolls to a place, to show the user | `at` `[line, col?, end_line?, end_col?]` or `text` |
| `view_camera` | gets or sets the camera (`$vpt`, `$vpr`, `$vpd`, `$vpf`) | `vpt`, `vpr`, `vpd`, `view` (top ... diagonal; `iso` too), `fit` |
| `view_capture` | a PNG of the view as the user sees it: their camera, the grid, the agent's marks; after any pending preview | `size` (longest side, default 768, 64 to 2048) |
| `view_annotate` | markers and lines in the view, replacing the agent's earlier ones; none clears | `markers` `[{point, label, color}]`, `lines` `[{points, closed, color}]` |
| `console_read` | the console of the last preview or render | |

Positions are 1-based lines and 1-based **byte** columns, as every
diagnostic here gives them; the editor counts UTF-16 units, and the two
meet only through `lang::source`. A stale version, an `old` that occurs
twice, overlapping edits, a column inside a character, and an edit the
user rejects (the page's "Ask me before applying" switch) are `isError`
results that say what to do next. With no page connected, the page tools
wait up to 5 s for one, then answer with how to connect.

The browser tools add 2,510 bytes to the tool list as the model sees it
(`[name, description, input schema]`, compact JSON; 3,081 with the keys and
annotations a client receives, roughly 630 to 880 tokens). The model tools
stay at 5,488. `crates/cli/src/mcp/tools/browser.rs` keeps the browser
tools under 2,600 and each description under 300; the instructions gain
one sentence.

## Smoke test

A quick check that a client can reach the server and see an image:

```sh
claude -p "Use the neoscad snapshot tool on this model: cube(10); Then tell me in one sentence what you see and the volume." \
  --model sonnet --mcp-config mcp.json --strict-mcp-config --tools "" \
  --allowedTools mcp__neoscad__snapshot --output-format stream-json --verbose
```

The model should call `snapshot` with `{"source": "cube(10);"}`,
receive the image and the summary, and describe a 10 mm cube with a
volume of 1000 mm³.

## Agent-loop eval

`scripts/agent-eval/run.py` runs modeling tasks with this server against
Bash and the OpenSCAD command line; see `docs/agent-eval.md`.
