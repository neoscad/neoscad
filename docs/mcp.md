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
change a resource limit; see "Safety").

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
full JSON result.

| Tool | What it answers | Extra arguments |
|---|---|---|
| `evaluate` | errors and warnings with fix hints, `echo()` output; no geometry | |
| `render` | bbox, volume, area, manifold, components; optionally writes the model | `export` (a file; format from its extension), `overwrite` |
| `snapshot` | a PNG contact sheet as MCP image content, plus the geometry summary | `views`, `size` (default `768x768`), `diff_against` (a file) or `diff_source`, `highlight`, `issues`, `dims`, `preview`, `output` (also save the PNG; a `.png` name), `overwrite` |
| `check` | printability findings, each with location and fix | `bed`, `nozzle`, `min_wall`, `max_overhang` |
| `measure` | model and part bbox, volume, centroid; distance between parts; sections | `part`, `between`, `section` |
| `test` | model tests (`docs/model-tests.md`); `path` is a test file or directory, `source` a test file's text | `filter` |
| `format` | `source`: the formatted text; `path`: rewrites the file (only whitespace changes) | `check` (return the diff, write nothing) |
| `docs` | a builtin's reference, or with `path` a file's definitions; no name: the index | `name`, `full`, `verbose` (the whole index) |

The server's `instructions` (sent once, at discovery or `initialize`)
say when to use which: iterate on inline source, `evaluate` for errors,
`render` for numbers, `snapshot` to see, `check` before finishing. The
tool list is 5,498 bytes of compact JSON as `[name, description, input
schema]` arrays, which is what `crates/cli/tests/mcp.rs` measures and
keeps under 5,500 bytes (each description under 300). What a client
receives is larger: 5,778 bytes with the keys (`name`, `description`,
`inputSchema`) and 6,069 with `annotations`, roughly 1,450-1,700 tokens
(estimated at 3.5-4 bytes a token; not measured with a tokenizer).

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
diagnostics and 20 echo lines. **The structured content must stand on
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
  4 GiB estimated memory, 10,000 fragments per primitive, 10,000
  slices, 10 million list elements and `rands()` numbers, 64 MiB
  strings, 10 million triangles per result; `docs/cli-json.md`,
  "Resource limits"). A model that would pass one (`sphere(10,
  $fn=100000)` once reached a 43 GB footprint) fails at once with a
  `resource-limit` error that names the limit and the flag that raises
  it; `neoscad mcp --limit fragments=50000` (or `=off`) does.
- **No network** and **no command execution:** the tools evaluate
  OpenSCAD, which has neither.
- Writes happen only through `render`'s `export`, `snapshot`'s `output`
  and `format` on a `path`.
- Resources: `neoscad://docs` (the builtin index) and the template
  `neoscad://docs/{name}`. No prompts.

## Smoke test

Run once, 2026-09-26, Claude Code 2.1.283, `--model sonnet`:

```sh
claude -p "Use the neoscad snapshot tool on this model: cube(10); Then tell me in one sentence what you see and the volume." \
  --model sonnet --mcp-config mcp.json --strict-mcp-config --tools "" \
  --allowedTools mcp__neoscad__snapshot --output-format stream-json --verbose
```

The server connected, the model called `snapshot` with
`{"source": "cube(10);"}`, received the image and the summary, and
answered "It's a simple 10×10×10 mm cube sitting on the origin corner,
with a volume of 1000 mm³." Two turns, $0.060, 3.9 s.

## Agent-loop eval

`scripts/agent-eval/run.py` runs modeling tasks with this server against
Bash and the OpenSCAD command line; see `docs/agent-eval.md`.
