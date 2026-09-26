# `neoscad serve`: the protocol

`neoscad serve` is a long-lived process that holds one
`session::Session` (`crates/session`): open documents, parsed files,
the geometry cache and the last CSG products, all warm. It answers
[JSON-RPC 2.0](https://www.jsonrpc.org/specification) requests. The
command line, the future MCP server and the apps are its clients; the
methods mirror the session's API (`evaluate`, `render`, `export`,
`snapshot`, `cancel` and the document methods).

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
| 1 | First version (phase 7a). |

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
  - `--idle-timeout SECS` (default 1800, 0 for never): the server exits
    when it has had no connection and no request for that long.
  - `neoscad serve --status [--format json]` prints the running server's
    `status`; `neoscad serve --stop` sends it `shutdown`. Both exit 1
    when no server answers.
  - `--cache-mb N` sets the geometry cache budget (MiB, per colour scheme
    and font set; default 200, OpenSCAD's two default cache sizes).

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
`render`, `export`, `snapshot` and the `cli.*` methods run concurrently,
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
| `progress` | bool | Send `progress` notifications (default true). |

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
   "snapshot": true}}
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
                   "lexed_files": 31, "lexed_bytes": 9325744},
   "geometry_cache": {"entries": 100, "bytes": 419112, "budget": 209715200,
                      "hits": 113, "misses": 1105, "evictions": 0}}}
```

Sizes are estimates in bytes. Both caches evict least recently used
entries past their budgets. `requests` counts evaluations, renders,
exports and snapshots (a snapshot with `diff` is two).

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
-32602 and changes nothing.

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
`["export-svg/fill=true"]`). Result: `exit_code`, `output`, `format`,
`bytes` (written), `geometry` (`null` for the non-mesh formats),
`diagnostics`, `echo`, `counts`, `timings_ms`. A model that fails (wrong
dimension, empty, a syntax error) has a non-zero `exit_code` and writes
nothing, except `echo`, whose file holds the messages that say why.

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
model), `lighting` (`headlight`, the default, or `openscad`). Result: the
snapshot summary of `docs/cli-json.md` plus `exit_code`. A request the
server cannot draw (no GPU, a bad view name) is error -32001 with the
message.

### `cli.export`, `cli.snapshot`

The command line's server mode: a command-line run for a client of the
**same build** (`binary`, which identifies the executable) in the **same
environment** (`environment`: `OPENSCADPATH`, `OPENSCAD_FONT_PATH`,
`NEOSCAD_FONT_DIR`, `HOME`), with `cwd`. The other parameters are the
command line's, as `crates/cli/src/delegate.rs` and
`crates/cli/src/snapshot.rs` build them. The result is what the command
would have printed: `{"exit_code", "stderr", "stdout"}` (strings, or
arrays of bytes when not UTF-8). A server of another build or environment
answers -32001, and the client runs the command itself.

## Notifications

From the server:

- `progress`: `{"id": <request id>, "stage": "parse" | "evaluate" |
  "geometry" | "draw"}`, as each stage starts.
- `diagnostics`: `{"path": <normalised path>, "diagnostics": [...]}`
  after each `evaluate`, `render` or `export` of a document: its current
  diagnostics (possibly none), as LSP publishes them.

From the client:

- `exit`: the server exits at once.
- `$/cancelRequest` `{"id": <request id>}`: cancels that request's
  document (LSP's name).
- `cancel` `{"path": ...}`: as the `cancel` method, with no answer.

## Errors

| Code | Meaning |
|---|---|
| -32700 | Not JSON. |
| -32600 | Not a request. |
| -32601 | Unknown method. |
| -32602 | Bad parameters (the message says which). |
| -32800 | Cancelled by a newer request on the document, or `cancel`. |
| -32001 | The operation could not run: no GPU, a snapshot with a bad view, a `cli.*` request from another build or environment. |

A model's own failures (a syntax error, an empty result) are not errors:
they are results with a non-zero `exit_code` and the diagnostics.

## The command line as a client

`neoscad IN -o OUT` (geometry and PNG exports) and `neoscad snapshot`
send their work to the server on the default socket when one answers,
unless `--no-server` is given or `NEOSCAD_NO_SERVER` is set (the
conformance harness sets it). The output is the command's own: the same
files and the same stderr, except the render summary's `Geometries in
cache` count and times, which are the (warm) server's. Runs the server
cannot take (dependency files, `-m`, parameter sets, `--animate`,
`--summary-file`, `--hardwarnings`, the evaluation flags, echo, AST, CSG
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
