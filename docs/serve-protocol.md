# `neoscad serve`: the protocol

`neoscad serve` is a long-lived process that holds one
`session::Session` (`crates/session`): open documents, parsed files,
the geometry cache, the last CSG products and each document's
top-level statements' evaluation (reused on the next request when
their inputs did not change; output is identical), all warm. It answers
[JSON-RPC 2.0](https://www.jsonrpc.org/specification) requests. The
command line, the MCP server (`docs/mcp.md`, which calls these methods
in-process) and the apps are its clients; the
methods mirror the session's API (`evaluate`, `render`, `export`,
`snapshot`, `check`, `measure`, `format`, `docs`, `test`, `cancel` and
the document methods).

The implementation is `crates/cli/src/serve.rs` (server),
`crates/cli/src/rpc.rs` (framing) and `crates/cli/src/client.rs` (the
command line as a client). This document is the contract.

## Versioning

The protocol is **additive-only**: methods, parameters, result fields
and notifications may be added; none is renamed, removed or retyped.
A client learns what a server has from `initialize` (the `capabilities`
handshake) and should ignore fields it does not know. An incompatible
change would get a new `protocol` number; there has been none.

| `protocol` | Changes |
|---|---|
| 1 | First version (phase 7a). Phase 7b added, additively: `check`, `measure`, `cli.check`, `cli.measure`, the `enable`/`parts` parameters, the snapshot's `highlight` and `issues`, the `check`, `measure` and `features` capabilities, and error -32603 for a request that panicked. Phase 7b-2 added `format`, `docs` and `test`, and their capabilities. Phase 7c added the `supersede` parameter. Hardening (H4) added the `limits` parameter and resource limits (a `resource-limit` diagnostic), the `docs` method's `brief`, and the diagnostic codes `input-not-found` and `output-not-writable`; document versions now count each document's own changes. Parsing each included file once (`9dbb98b`) added `stats`' `parse_cache.fragment_files` and `fragment_bytes`. `enable` now also takes OpenSCAD's experimental features (`textmetrics`, `object-function`, `import-function`, `vector-swizzle`, and on `export`/`cli.export` `predictible-output`). The `docs` method's `file_arg` was added after the CAD run cad-20260929T031249Z. |

## Transports

- **stdio** (`neoscad serve`): requests on stdin, responses and
  notifications on stdout. An editor or MCP host starts the server as its
  child. The server exits at the end of stdin or on `exit`.
- **Unix socket** (`neoscad serve --socket [PATH]`): each connection is an
  independent stdio-like stream. Without `PATH` the socket is the per-user
  default: `$NEOSCAD_SOCKET` if set, else
  `$XDG_RUNTIME_DIR/neoscad/serve.sock`, else
  `<temp dir>/neoscad-<uid>/serve.sock` (on macOS the temp dir is already
  per user). The default socket's directory is created user-only (0700)
  and must belong to the user; every socket is made 0600. A socket path
  is limited to 104 bytes on macOS (`SUN_LEN`). There is no network
  listener.
  - A stale socket (left by a server that did not exit cleanly) is
    removed at start; a live one makes the new server refuse to start.
    Only a socket is ever removed: any other file at the path (a
    mistyped `--socket notes.txt`) makes the server refuse to start and
    leaves the file alone.
- **Windows named pipe** (`neoscad serve --socket [NAME]`): the same
  option and the same stream, over a pipe. `NAME` is `\\.\pipe\NAME` or
  a bare name, which gets that prefix; without it the pipe is
  `$NEOSCAD_SOCKET` (likewise prefixed) if set, else
  `\\.\pipe\neoscad-<SID>`, named by the user's security identifier as
  the Unix default is by the uid. The pipe's security descriptor makes
  the user its owner and grants access to that user alone (the default
  one would let Everyone read); remote clients are refused. A pipe that
  exists already, a running server's or anyone else's, makes the new
  server refuse to start ("a server is already listening"), and nothing
  is left behind to clean up: a pipe goes when its server exits.
- For either:
  - `--idle-timeout SECS` (default 1800, 0 for never): the server exits
    when it has had no connection and no request for that long.
  - `neoscad serve --status [--format json]` prints the running server's
    `status`; `neoscad serve --stop` sends it `shutdown`. Both exit 1
    when no server answers.
  - `--cache-mb N` sets the geometry cache budget (MiB, per colour scheme
    and font set; default 200, OpenSCAD's two default cache sizes).
- `--limit NAME=VALUE` (repeatable, every transport) changes one of the
  resource limits every request runs under; see "Resource limits".

**Platforms.** stdio works everywhere, and so do `neoscad mcp` and
`neoscad lsp`, which use it. `--socket` is a Unix socket on macOS, Linux
and the BSDs and a named pipe on Windows (`crates/agent-link/src/transport.rs`,
shared with the desktop apps' agent link, and `crates/cli/src/transport.rs`
for the server's default address; the pipe is interprocess's synchronous
listener, with no async runtime).
Elsewhere `--socket` fails at once and the command line runs everything
in-process. The Windows code is checked with
`cargo clippy --target x86_64-pc-windows-msvc -p neoscad-cli
--all-targets --no-default-features --features bundled-assets` (in the
`rust:1.98.1` Docker image: mimalloc's C sources need MSVC and the
Windows SDK, which a check off Windows does not have, hence the
`mimalloc` feature), and run by CI's Windows job: the transport's unit
tests, the served-output tests over a pipe, and the release binary
serving an export.

## Framing

LSP's: every message is a header, a blank line and a UTF-8 JSON body.

```
Content-Length: 58\r\n
\r\n
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}
```

`Content-Length` (case-insensitive) is required; other header fields are
ignored. Messages are at most 64 MiB. A body that is not JSON is answered
with error -32700 and the stream continues; a broken header (no number,
over the limit) ends the connection.

## Ordering and concurrency

`initialize`, `status`, `stats`, `documents`, `open`, `update`, `close`
and `cancel` are handled in arrival order before the next message is
read, so `update` then `render` renders the new text. `evaluate`,
`render`, `export`, `snapshot`, `check`, `measure`, `format`, `docs`,
`test` and the `cli.*` methods run concurrently,
each on its own thread; responses may come back in any order (match them
by `id`).

**Cancellation.** A request on a document (by its path, normalised)
cancels the older `evaluate`/`render`/`export`/`snapshot` requests still
running on it, and `update`/`open`/`close` cancel them too (their text is
stale). A cancelled request answers error **-32800** ("cancelled by a
newer request on the document"). It stops at the evaluator's next call or
loop iteration, or before the geometry evaluator's next node (a single
kernel operation is not interrupted); everything it had finished stays
cached for the next request. `cancel` and the `$/cancelRequest`
notification cancel explicitly. The command line's `cli.*` requests are
one-shot: they cancel nothing and are only cancelled explicitly.

## Common parameters

Requests on a model take:

| Parameter | Type | Meaning |
|---|---|---|
| `path` | string | The model (also accepted as `input`): absolute, or relative to `cwd`. Messages name it as given. |
| `cwd` | string | Working directory: relative paths resolve against it and parser messages print relative to it. Default: the server's. |
| `defines` | [string] | `-D` assignments, e.g. `"a=3"`. |
| `quiet` | bool | Only errors in the log. |
| `seed` | int | The seed of unseeded `rands()` (default: the server's, fixed per process). |
| `enable` | [string] | `--enable`'s names, for this request. `"part"` turns on neoscad's `part()` extension (`docs/cli-json.md`, "Named parts"), as `--enable part` does. OpenSCAD's experimental features by their names (`"all"` is every one): `textmetrics`, `object-function`, `import-function` and `vector-swizzle` change evaluation; `predictible-output` sorts an `export`'s mesh file (`docs/cli-json.md`, "Sorted exports"). Other names are ignored. |
| `parts` | bool | The same as `"enable": ["part"]`. |
| `progress` | bool | Send `progress` notifications (default true). |
| `supersede` | bool | Cancel older requests on the same document when this one starts (default true; see "Ordering and concurrency"). `false` lets requests on one file run side by side, as `neoscad mcp` sends them. |
| `limits` | object | This request's resource limits, on top of the server's: `{"fragments": 20000, "time": null, ...}`, each name of "Resource limits" to a number in its unit or `null`/`"off"` for none. An unknown name or a bad value is -32602. |

Results that describe a run carry `exit_code` (0, or the command line's
code: 1 for an error, 3 for a feature neoscad lacks), `diagnostics`,
`echo`, `counts` and `timings_ms`:

- `diagnostics`: errors, warnings and deprecations as structured objects
  (`docs/cli-json.md`, "Diagnostics"), each error with the `TRACE:` lines
  after it in `trace`.
- `echo`: `echo()` output, one printed line each.
- `counts`: `{"errors", "warnings", "echoes"}`.
- `timings_ms`: `{"parse", "evaluate", "geometry", "total"}`, rounded to
  0.1 ms. `parse` includes reading files (from the caches when unchanged).

## Methods

### `initialize`

Params: anything (ignored). Result:

```json
{"protocol": 1,
 "server": {"name": "neoscad", "version": "0.1.0", "binary": "..."},
 "capabilities": {
   "methods": ["initialize", "shutdown", ...],
   "notifications": {"server": ["progress", "diagnostics"],
                     "client": ["exit", "cancel", "$/cancelRequest"]},
   "export_formats": ["stl", "binstl", "off", "obj", "3mf", "wrl", "pov",
                      "svg", "dxf", "pdf", "png", "echo", "ast", "csg"],
   "render_modes": ["render", "force", "preview"],
   "incremental_edits": true,
   "snapshot": true, "check": true, "measure": true,
   "format": true, "docs": true, "test": true,
   "features": ["part"]}}
```

`initialize` is optional (the command line does not send it).

### `shutdown`

Result `null`. A socket server removes its socket and exits after
answering; a stdio server waits for `exit` or the end of input.

### `status`, `stats`

`stats` is the session's numbers; `status` adds the process:

```json
{"pid": 123, "socket": "/…/serve.sock", "protocol": 1, "binary": "...",
 "uptime_s": 60, "idle_s": 2, "idle_timeout_s": 1800, "connections": 1,
 "stats": {
   "documents": 1, "requests": 12, "cancelled": 1, "running": 0,
   "renderers": 1,
   "parse_cache": {"entries": 3, "bytes": 1234, "budget": 268435456,
                   "hits": 9, "misses": 3, "evictions": 0,
                   "lexed_files": 31, "lexed_bytes": 9325744,
                   "fragment_files": 31, "fragment_bytes": 50594868},
   "geometry_cache": {"entries": 100, "bytes": 419112, "budget": 209715200,
                      "hits": 113, "misses": 1105, "evictions": 0}}}
```

Sizes are estimates in bytes. Both caches evict least recently used
entries past their budgets. `geometry_cache.budget` sums the renderers'
budgets, and is never below the configured budget (`--cache-mb`): before
the first render, when there is no renderer yet, it is the budget the
first will get. `requests` counts evaluations, renders,
exports and snapshots (a snapshot with `diff` is two).

`lexed_files` and `lexed_bytes` are the included files kept read and
lexed, and their text and tokens, bounded by a quarter of the parse
budget. `fragment_files` and `fragment_bytes` are the included files
kept parsed and lowered, and their estimated size, bounded by half of
the parse budget: an include between top-level statements
(`include <BOSL2/std.scad>`) comes from here after its first parse.
There is one entry for each included file, the includes it is read
inside and the main file: `BOSL2/std.scad` and each file it includes
make 31. A fragment holds the files it includes as well, so nested
files count in more than one entry's bytes.

### `documents`

Result: the open documents, `[{"path", "version", "length"}]`.

### `open`

Params: `path`, optional `text`. With `text`, the document is an unsaved
buffer: every read of its path (as the main file, an include, a `use`, an
`import()`) sees it. Without, it is tracked and read from disk. Result:
`{"path" (normalised, absolute), "version", "length"}` (`length` is
`null` without a buffer).

### `update`

Params: `path` and either `text` (the full new text) or `edits`:
`[{"start": byte, "end": byte, "text": "..."}]`, applied in order, each
to the result of the one before (byte offsets into UTF-8). Result as for
`open`. An edit out of range, or one that splits a character, is error
-32602 and changes nothing, the version included. `version` counts the
document's changes: 1 for its first text, then one more for each
`open` with text or `update` (0 while it is read from disk).

### `close`

Params: `path`. Drops the buffer (reads go to disk again), the
document's products, and cancels its requests. Result `{"closed": bool}`
(whether it was open).

### `cancel`

Params: `path`. Result `{"cancelled": n}`, the requests stopped.

### `evaluate`

Parse and evaluate. Params: the common ones and `csg` (bool: also return
the node tree as the `.csg` export writes it). Result: `exit_code`,
`aborted` (an evaluation error stopped evaluation early), `csg` (or
`null`), `diagnostics`, `echo`, `counts`, `timings_ms`.

### `render`

Params: the common ones and `mode`: `render` (default: the full
geometry), `force` (`--render=force`: a mesh converted to a solid) or
`preview` (OpenSCAD's preview: only the leaves' geometry and the CSG
products; `$preview` is true). Result: `exit_code`, `geometry` (the
object of `docs/cli-json.md`, or `null` when empty or previewing),
`preview_bbox` (previews only), `cache_entries`, `diagnostics`, `echo`,
`counts`, `timings_ms`.

### `export`

Params: the common ones, `output` (written by the server, relative to
`cwd`), `format` (an OpenSCAD format identifier; default: the output's
extension), `force` (bool), `options` (`-O` settings, e.g.
`["export-svg/fill=true"]`), `enable` (`["predictible-output"]` for a
sorted mesh file). Result: `exit_code`, `output`, `format`,
`bytes` (written), `geometry` (`null` for the non-mesh formats),
`diagnostics`, `echo`, `counts`, `timings_ms`. A model that fails (wrong
dimension, empty, a syntax error) has a non-zero `exit_code` and writes
nothing, except `echo`, whose file holds the messages that say why. A
mesh output that cannot be written has `exit_code` 1 and an
`output-not-writable` error, and a missing input an `input-not-found`
one (as for every model method).

- Mesh and 2D formats (`stl`, `binstl`, `off`, `obj`, `3mf`, `wrl`,
  `pov`, `svg`, `dxf`, `pdf`) go through the command line's encoder,
  byte for byte.
- `echo`: every message, as the `.echo` export holds them; `csg`: the
  node tree; `ast`: the parsed program printed back.
- `png`: OpenSCAD's image at its default camera (fitted to the model,
  512x512, Cornfield): the OpenCSG preview, or with `"mode": "render"`
  the rendered geometry, lit as OpenSCAD lights it. `snapshot` is the
  agent's view; this is the command line's `-o x.png` without its camera
  flags.

### `snapshot`

A contact sheet (`neoscad snapshot`). Params: the common ones, `output`
(default: the model's stem with `-snapshot.png`, relative to `cwd`),
`views` ([string]), `size` (`"WxH"`), `dims`, `preview`, `diff` (another
model), `lighting` (`headlight`, the default, or `openscad`),
`highlight` ([string]: parts to show in colour, the rest ghosted) and
`issues` (`true` to run `check` with its defaults, or with the check
parameters given alongside; or an object of check parameters: the
findings are marked on the sheet). Result: the
snapshot summary of `docs/cli-json.md` plus `exit_code`. A request the
server cannot draw (no GPU, a bad view name) is error -32001 with the
message.

### `check`

Printability checks (`neoscad check`). Params: the common ones, `bed`
(`"WxDxH"` or `[w, d, h]`, mm), `nozzle`, `min_wall`, `max_overhang`
(degrees from vertical) and `max_findings`; out of range is error
-32602. Result: the check object of `docs/cli-json.md` ("`neoscad
check`"), whose `exit_code` is 1 when a finding is an error. Publishes
the document's diagnostics.

### `measure`

Measurements (`neoscad measure`). Params: the common ones, `part`
(string), `between` ([A, B]), `section` (`"z=5"`, `"x=-2"`, `"y=0"`),
`axis` (`"x"`, `"y"` or `"z"`), `center` (`[a, b]` or `"a,b"`),
`profile` (`[from, to, step]` or `"from:to:step"`) and `svg`: a file name (the server writes the section's outline there,
relative to `cwd`, and `section.svg` names it) or `true` (the SVG text
in `section.svg_text`). Result: the measure object of
`docs/cli-json.md`; an unknown part gives `failed` and `error` with
`exit_code` 1.

### `format`

Formatting (`neoscad fmt`; `docs/cli-json.md`). Params: `path` (the
file: an open document's buffer when it is one, else the file; relative
to `cwd`) and/or `text` (format this instead of the file's text; the
path, if any, still selects the configuration), `cwd`, `indent`,
`width` (override `.neoscad-fmt.toml`) and `diff` (bool). Nothing is
written: the result carries the text. Result: `{"path", "changed",
"error", "config", "text", "exit_code"}` plus `diff` when asked for;
`text` is the formatted text (`null` on an error), `error` as in the
command's JSON (`exit_code` 1: a syntax error, the file left as it is).
Neither `path` nor `text` is error -32602.

### `docs`

Reference text (`neoscad docs`; `docs/cli-json.md`). Params: `name`
(none: the index), `file` (also `in`: search this file, its includes and
the libraries it `use`s), `cwd`, `full`, `brief` (bool: a short index
for an agent's context: `_private` names left out, and when the
included and used files define more than 100 names, each file with its
count instead of its names; `neoscad mcp` sends it), `file_arg` (how
the client names its file argument in the hint for an unknown name;
default `` `file` ``; `neoscad mcp` sends its `path`). Result: the command's JSON plus
`text`, the text the command prints. An unknown name is a result with
`exit_code` 1 and `did_you_mean`.

### `test`

Model tests (`neoscad test`; `docs/model-tests.md`). Params: `paths`
([string]) and/or `path` (test files or directories; default `cwd`),
`cwd`, `filter`, `enable`/`parts` (`part()` for every test) and `jobs`
(threads; default one per CPU). Open documents' buffers are what the
tests run. Result: the JSON of `docs/model-tests.md`. Each test is a run
of its file (not superseding others), so `cancel` on a test file's path
stops the request with -32800.

### `cli.export`, `cli.snapshot`, `cli.check`, `cli.measure`

The command line's server mode: a command-line run for a client of the
**same build** (`binary`, which identifies the executable) in the **same
environment** (`environment`: `OPENSCADPATH`, `OPENSCAD_FONT_PATH`,
`NEOSCAD_FONT_DIR`, `HOME`), with `cwd`. The other parameters are the
command line's, as `crates/cli/src/delegate.rs`,
`crates/cli/src/snapshot.rs`, `crates/cli/src/check.rs` and
`crates/cli/src/measure.rs` build them (`cli.export` carries `parts`
for `--enable part`, and `enable` carries every `--enable` name,
including the experimental features). The result is what the command
would have printed: `{"exit_code", "stderr", "stdout"}` (strings, or
arrays of bytes when not UTF-8). A server of another build or environment
answers -32001, and the client runs the command itself.

## Notifications

From the server:

- `progress`: `{"id": <request id>, "stage": "parse" | "evaluate" |
  "geometry" | "draw"}`, as each stage starts.
- `diagnostics`: `{"path": <normalised path>, "diagnostics": [...]}`
  after each `evaluate`, `render`, `export`, `check` or `measure` of a
  document: its current
  diagnostics (possibly none), as LSP publishes them.

From the client:

- `exit`: the server exits at once.
- `$/cancelRequest` `{"id": <request id>}`: cancels that request's
  document (LSP's name).
- `cancel` `{"path": ...}`: as the `cancel` method, with no answer.

## Resource limits

Every model request (`evaluate`, `render`, `export`, `snapshot`,
`check`, `measure`, and the models `test` runs) runs under resource
limits, because a server runs models that agents and editors write and
one runaway `$fn` must not exhaust the machine: time 60 s, estimated
memory 4 GiB, 10,000 fragments per primitive, 10,000 slices per
extrusion, 10 million list elements, 64 MiB strings, 10 million
`rands()` numbers and 10 million triangles per result
(`docs/cli-json.md`, "Resource limits", has the details). A request that
would pass one is an ordinary result: `exit_code` 1 and a
`resource-limit` error saying which limit, where, and how to raise it.
`--limit NAME=VALUE` at start changes the server's limits and a
request's `limits` parameter its own. The command line's `cli.*`
requests run unlimited, as the command line does in its own process.

## Errors

| Code | Meaning |
|---|---|
| -32700 | Not JSON. |
| -32600 | Not a request. |
| -32601 | Unknown method. |
| -32602 | Bad parameters (the message says which). |
| -32800 | Cancelled by a newer request on the document, or `cancel`. |
| -32001 | The operation could not run: no GPU, a snapshot with a bad view, a `cli.*` request from another build or environment. |
| -32603 | The request panicked (a bug): the message is `internal error: the request panicked: ...`. The server keeps serving; its documents and caches stay (the release build unwinds, and the session's locks survive a request that panicked holding one). |

A model's own failures (a syntax error, an empty result) are not errors:
they are results with a non-zero `exit_code` and the diagnostics.

## The command line as a client

`neoscad IN -o OUT` (geometry and PNG exports), `neoscad snapshot`,
`neoscad check` and `neoscad measure`
send their work to the server on the default socket when one answers,
unless `--no-server` is given or `NEOSCAD_NO_SERVER` is set (the
conformance harness sets it). Before it sends anything (its working
directory, environment and command line) the client checks the socket:
it must be a socket owned by the user, in a directory that is the
user's and not writable by group or others (or a sticky one such as
`/tmp`). On Windows it opens the pipe (at identification level only, so
the pipe's server cannot act as the client) and checks that the pipe's
owner is the user: pipe names are global, and another user could create
the pipe first. Otherwise the command runs in-process. The output is the command's own: the same
files and the same stderr, except the render summary's geometry
cache count and size and its times, which are the (warm) server's. Runs the server
cannot take (dependency files, `-m`, parameter sets, `--animate`,
`--summary-file`, `--hardwarnings`, `--limit`, the evaluation flags, echo, AST, CSG
and param exports, and a run mixing PNG with other formats) and every
failure to reach a server (none, another build or environment, a dropped
connection) run in the process, silently.

## Example

```
→ {"jsonrpc":"2.0","id":1,"method":"open","params":{"path":"/w/a.scad","text":"cube(10);"}}
← {"jsonrpc":"2.0","id":1,"result":{"length":9,"path":"/w/a.scad","version":1}}
→ {"jsonrpc":"2.0","id":2,"method":"render","params":{"path":"/w/a.scad"}}
← {"jsonrpc":"2.0","method":"progress","params":{"id":2,"stage":"parse"}}
← {"jsonrpc":"2.0","method":"progress","params":{"id":2,"stage":"evaluate"}}
← {"jsonrpc":"2.0","method":"progress","params":{"id":2,"stage":"geometry"}}
← {"jsonrpc":"2.0","method":"diagnostics","params":{"diagnostics":[],"path":"/w/a.scad"}}
← {"jsonrpc":"2.0","id":2,"result":{"cache_entries":1,"counts":{"echoes":0,"errors":0,"warnings":0},"diagnostics":[],"echo":[],"exit_code":0,"geometry":{"area":600.0,"bbox":{...},"components":1,"dimensions":3,"manifold":true,"triangles":12,"vertices":8,"volume":1000.0},"preview_bbox":null,"timings_ms":{...}}}
```
