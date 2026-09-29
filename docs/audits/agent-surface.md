# Audit: the agent surface (phase 7)

Scope: `crates/session`, `neoscad serve`, `neoscad mcp`, and `check`,
`measure`, `test`, `fmt`, `docs` and `snapshot`. Audited at `8669474`
(clean tree), release build, on macOS 27.0 (arm64, 48 GB). The reference
was the nightly `/Applications/OpenSCAD.app` (2026.09.23, `--backend=manifold`).
Repros use a small MCP driver (`scripts` were not changed): it spawns
`neoscad mcp` in a scratch directory, performs the legacy `initialize`, and
sends `tools/call` lines. A shell equivalent is given where it matters.

## On firm ground

- **The MCP read fence holds.** `include`, `use`, `import()`, `surface()`,
  `dxf_dim()`, `use <font.ttf>`, `..`, absolute paths, a symlinked
  directory, `base_dir`, and the `path` of `docs`, `test` and `format` all
  failed to read outside the roots, and so did a `neoscad://docs/../..`
  resource. An existing and a missing outside file give the same message,
  so there is no existence oracle. Details are in "Checked and found fine".
- **`measure` is accurate.** Volume, centroid, area and bbox agree with an
  independent computation on the nightly's exported mesh for six models
  (BOSL2 and OpenSCAD examples) to about 3e-6 relative. `--section` and
  `--between` match analytic values to 1e-6.
- **Most `check` findings on real models are correct.** On 14 models, the
  floating, thin-plate and feather-edge findings are real. One class is
  noise: the "0 mm thin walls" on `spring_handle` (finding 4).
- **No command execution** is reachable through serve or MCP (`-m` exists
  only on the in-process command-line path). The serve socket is private
  on macOS.
- **The documented examples run as documented**, with the exceptions
  listed in finding 11.

## Findings, by importance

### 1. One request can exhaust the machine's memory, and nothing stops it (high)

**Our code.** `sphere(10, $fn=100000);` through MCP `render` reached a
**43 GB footprint and filled 30 GB of swap in about 3.5 minutes** on a
48 GB Mac. The render thread was inside `geom::primitives::sphere`
(`crates/geom/src/primitives.rs:76`, from `sample`), and I killed it by
hand. Other vectors, each killed by my 3 GB guard within about a second:

| Source (one `render` or `evaluate` call) | RSS at kill | Time |
|---|---|---|
| `x = rands(0,1,1e9);` | 6.4 GB | 1.5 s |
| `function f(s,n) = n==0 ? s : f(str(s,s), n-1); echo(len(f("a",40)));` | 3.7 GB | 0.2 s |
| `function g(v,n) = n==0 ? v : g(concat(v,v), n-1); echo(len(g([1],40)));` | 4.7 GB | 0.4 s |
| `linear_extrude(height=10, slices=100000000) square(1);` | 3.1 GB | 0.4 s |
| `circle(r=1, $fn=1e9);` | 3.3 GB | 1.1 s |
| `cylinder(h=1, r=1, $fn=3e8);` | 3.7 GB | 0.9 s |

Aggravating behaviour:

- **No request timeout and no memory budget** exist in `serve` or `mcp`.
- **Cancellation cannot reach the work.** `notifications/cancelled` and
  `-32800` stop the evaluator "at the next call or loop iteration", but a
  primitive or a kernel operation runs to its end (`docs/followups.md`,
  "Serve and session").
- **A dead client does not stop the server.** When my driver was killed,
  `neoscad mcp` saw end of input, and as documented it "answers the calls
  still running, then exits" (`docs/mcp.md`). It kept allocating as an
  orphan until I killed it with `kill -9`.

**OpenSCAD** has the same lack of caps: `$fn` has no upper bound
(`src/core/CurveDiscretizer.cc:109-113`) and `rands` reserves whatever
count it is given (`src/core/builtin_functions.cc:178-193`). The one-shot
command line matching this is fine. A long-lived server that agents feed
generated code is a different exposure: one bad guess by an agent takes
the user's machine down, and in the macOS app it takes down unsaved work.

**Who it affects.** Every MCP and serve user, and the phase 8 app, which
embeds the same session in-process.

**Suggested change.** Add `Options` limits that `serve`, `mcp` and the app
turn on and the conformance command line leaves off:

- a vertex budget checked before primitives, extrusions and `rands`
  allocate;
- a cap on list and string length;
- a per-request wall-clock deadline wired to the existing cancel token.

On end of input, cancel running calls and exit after a short grace
period. **Owner decision:** these limits diverge from OpenSCAD (a model
OpenSCAD would eventually render is refused), so they should apply only
on agent and app surfaces, with a stable diagnostic code.

### 2. The MCP write fence follows dangling symlinks out of the root (medium; security)

**Our code.** `Roots::can_write` resolves the deepest *existing* ancestor
and appends the rest lexically (`crates/cli/src/mcp/roots.rs:86-105`). A
dangling symlink does not canonicalize, so it is judged by its parent,
which is inside the root. The write then goes through `std::fs::write`
(snapshot, `crates/cli/src/mcp/tools.rs:480`) or the export encoder, and
the kernel follows the link.

Repro (`R` is the MCP working directory):

```sh
mkdir -p R outside
ln -s "$PWD/outside/newfile.stl" R/dangling.stl
ln -s "$PWD/outside/dangle.png"  R/dangle.png
# in R: neoscad mcp, then
#   render   {"source":"cube(1);","export":"dangling.stl"}  -> "wrote .../R/dangling.stl"
#   snapshot {"source":"cube(1);","output":"dangle.png"}    -> "saved .../R/dangle.png"
ls outside   # newfile.stl (1443 bytes) and dangle.png now exist outside the root
```

The unit test covers a symlink to an existing directory
(`roots.rs:195-205`), which is refused correctly, but not a dangling link.
`docs/mcp.md` ("Safety") says symlinks are resolved "so neither `..` nor
a link leads out", which is not true for this case.

**Size of the gap.** It needs a pre-planted link: a cloned repository
that ships `out.stl -> ~/Library/LaunchAgents/x.plist`, followed by an
agent asked to "export to out.stl". The attacker does not control the
content (an STL, PNG, OFF, `.ast` or `.csg`), so this is file creation or
clobbering outside the fence, not code execution.

**Suggested change.** Before writing, `lstat` the final component and
refuse a symlink. Better, open the output with `O_NOFOLLOW` (on unix,
`OpenOptions::custom_flags(libc::O_NOFOLLOW)`) through one helper that
both snapshot and export use. Add the dangling case to `roots_refuse_escapes`.

### 3. `snapshot` `output` overwrites any file in the root, source files included (medium)

**Our code.** `snapshot`'s `output` is written with `std::fs::write`
whatever its extension (`tools.rs:478-482`). Repro, with a
`model.scad` in the root:

```
snapshot {"source":"cube(1);","output":"model.scad","size":"64x64"}
-> "ok; saved .../model.scad"
$ file model.scad
model.scad: PNG image data, 64 x 64
```

`render`'s `export` is safer by accident: a format comes from the
extension, so `.txt` and extension-less names are refused ("cannot export
'txt'"). But `writable()` has already created the output's directories by
then (`tools.rs:322-325`).

**Who it affects.** An agent that confuses `output` with `path` destroys
the user's model. Nothing warns, and the file may not be under version
control.

**Suggested change.** Require a `.png` extension for `output`. Refuse to
overwrite an existing file whose extension is not the output's format.
Create directories only after the call has validated its arguments.

### 4. `check` reports contact seams as 0 mm "thin walls", with a wrong fix (medium)

**The brief's claim is confirmed, but the findings are noise.**
`neoscad check .reference/BOSL2/examples/spring_handle.scad` (with
`OPENSCADPATH=.reference`) gives four `thin-wall` **errors**, "the
thinnest 0 mm thick", at [46.39, -5.64, 2.04], [-47.63, -5.48, 2.45],
[45.54, 5.94, 0.85] and [-46.79, 3.68, -4.74], each with the fix "thicken
it to at least 0.8 mm".

**The geometry.** I checked the nightly's own mesh
(`OpenSCAD --backend=manifold ... -o spring.off`, which is identical to
ours: 69,460 triangles, the same volume) with a scratch ray caster:

- All four points lie at radius 6.00 from the rod axis, inside the tight
  loops (|x| from 44 to 50). There the pitch is 2 mm and the wire is 2 mm,
  so adjacent coils touch (`tight_loops=3`, `lpart = wire_d * 3 / 100`).
- At each point, the flagged triangle (area 0.058 mm², not a sliver, with
  its normal along ±x toward the neighbouring coil) casts its inward ray
  and meets the neighbouring coil's face within 0.0000–0.0002 mm. The
  coils overlap by Manifold's tolerance and keep both surfaces.
- **Every adjacent triangle measures 1.9955–1.996 mm**, the true wire
  diameter.

So `check` has found a real feature, coincident opposing surfaces where
two coils fuse. That is not a wall, and the fix is wrong. A slicer would
print these coils as touching.

**Who it affects.** Any model with parts that just touch: coils, stacked
pieces, and `union`s of tangent solids. It raises error-level findings
that an agent will try to "fix".

**Suggested change.** In the wall ray, ignore an exit through a face that
is antiparallel (within a few degrees) and closer than a small ε (for
example 0.01 mm, or 1e-4 of the model's diagonal). Report such places,
if at all, as an `info` finding named `touching-surfaces`.

Related, and worded wrongly rather than computed wrongly: `floating`
says "with nothing under it" whenever a component's lowest point is
above the model's lowest point, even when it rests on another component.
An agent-eval run hit this with a lid on a box.
Either test for support by another component within `bed_tolerance`, or
say "is a separate piece starting N mm above the bed".

### 5. A missing input file fails silently through MCP and serve (medium)

**Our code.** For a path that does not exist:

- `render {"path":"nope.scad"}` returns `failed (exit 1)` / `empty: no
  geometry`, and the structured content has `"diagnostics": []`. The
  server's own result (`verbose: true`) has no message either.
- `check` on the same path returns `"counts": null, "exit_code": null`.
- The command line says `Can't open input file 'nope.scad'!`, and so does
  the nightly.

The message is lost in the session or serve path. `docs/followups.md`
records the same class for serve's `export` ("says nothing when the
output cannot be written").

**Who it affects.** Every agent that mistypes a path: exit 1 with no
reason is the least actionable result possible. The app will also need
these errors for its export UI.

**Suggested change.** Make "cannot open input" and "cannot write output"
structured diagnostics (a code such as `input-not-found` or
`output-not-writable`) in the session, so serve, MCP and the app all get
them. `check` should then return an `exit_code` rather than `null`.

### 6. Syntax errors reach the agent without a column (medium; ergonomics)

**Our code.** MCP's terse diagnostics drop `span` (`docs/mcp.md`, "The
structured content has the diagnostics without spans"), and the syntax
error hint reads "look just before this point for a missing ';' ...".
The "point" is only a line number. For `rotate(45 cube(3);` the agent
sees:

```
error inline.scad:1: Parser error: syntax error (look just before this point ...)
```

The command line's JSON has the column (`span.start.column`), and so
does the terminal excerpt. For one-line inline sources, which is how
agents iterate, the line number carries no information. An agent's
HTML-escaped `&lt;` syntax errors in an agent-eval run were this case.

**Suggested change.** Keep `column` (the span start) in the terse
diagnostic, and in the text say `inline.scad:1:10`. Include the
offending token when it is short ("unexpected `cube`").

### 7. `neoscad serve --socket PATH` deletes whatever file is at PATH (low–medium)

**Our code.** If `PATH` exists and nothing answers on it, `listen`
removes it as a "stale socket" without checking that it is a socket
(`crates/cli/src/serve.rs:1050-1057`). Repro:

```sh
echo "precious notes" > notes.txt
neoscad serve --socket notes.txt --idle-timeout 5 &
ls -l notes.txt   # srw------- notes.txt: the file is gone
```

The same happens through `NEOSCAD_SOCKET`. It is a user-supplied path,
so this is a foot-gun (for example `--socket model.scad`), not an attack.

**Suggested change.** Remove only when `symlink_metadata` reports a
socket (`FileTypeExt::is_socket`). Otherwise refuse with a message.

### 8. The command line trusts any listener at the default socket (low; Linux only)

**Our code.** The client connects to `default_socket()` with no owner
check (`crates/cli/src/client.rs:31-41`), then sends `cwd`, `environment`
(`HOME`, `OPENSCADPATH`, font paths) and the command line, and prints
the `stdout` and `stderr` it gets back. The *server* refuses a default
directory owned by another user (`serve.rs:1038-1043`). The *client* does
not check.

- **macOS: not exploitable.** The default is under the per-user
  `$TMPDIR` (`/var/folders/.../T/neoscad-501`, verified mode 0700).
- **Linux without `XDG_RUNTIME_DIR`:** the path is
  `/tmp/neoscad-<uid>/serve.sock`. Another local user can create that
  directory first, listen there, and receive the victim's paths and
  environment, and forge the victim's output.

An explicit `--socket` in a shared directory also has a bind-then-chmod
window (`serve.rs:1059-1062`). It is harmless under the default umask
022, because connecting requires write permission.

**Suggested change.** In the client, check that the socket's directory
belongs to the user and is not group- or other-writable before
connecting, and fall back to in-process otherwise. Bind inside a private
directory, or set the umask around `bind`. **Platform scope decision:**
this only matters if Linux builds are distributed.

### 9. Token cost: `check` and `docs` are the large defaults (low–medium)

Sizes on mid-size models (spring_handle, 69k triangles; BOSL2logo; CSG),
default arguments. Tokens are **estimated as bytes ÷ 4, not measured with
a tokenizer**. Claude Code shows the model the `structuredContent`
(`docs/mcp.md`), so that column is what counts there.

| Call | Text bytes | Structured bytes | ≈ tokens (structured) |
|---|---|---|---|
| `tools/list` (once per session; with keys and annotations) | | 5,979 | 1,500 |
| `evaluate` spring | 2 | 100 | 25 |
| `render` spring | 165 | 438 | 110 |
| `snapshot` spring, 768² | 165 | 436, plus an 83 KB base64 PNG | 110, plus image tokens (unverified; see below) |
| `check` spring / BOSL2logo / CSG | 3,682 / 4,952 / 4,147 | 5,419 / 6,930 / 5,899 | 1,350–1,750 |
| `check` BOSL2logo, `verbose` | 14,825 | 10,893 | 2,700 (the text is also sent) |
| `measure` spring (with `section`) | 150 (259) | 336 (559) | 85 (140) |
| `test` (2 tests) | 191 | 304 | 75 |
| `format` `check` (diff) | 1,720 | none | 430 |
| `docs` index / `cube` | 750 / 267 | none | 190 / 70 |
| `docs` `path_sweep` (BOSL2) | 13,164 | none | 3,300 |
| `docs` `path: spring.scad` (file index) | 21,246 | none | 5,300 |

- **`check`** is 1.4–1.7k tokens even when findings are truncated at 10
  per code, because each finding repeats the same `fix` sentence (10
  overhang findings, 10 identical fixes). Send each code's `fix` once,
  in a `fixes: {code: text}` map, or send the top 3 per code by default.
- **`docs` on a BOSL2 file** is 21 KB. It lists underscore-prefixed
  private helpers (`_quant_anch()`, `_find_anchor()`, ...), and it names
  every library file as
  `../../../../../../../<home>/…/BOSL2/affine.scad` (a path
  relative to `base_dir`), which costs 60+ bytes per file. Print library
  files relative to their library root (`BOSL2/affine.scad`) and hide
  `_names` unless `full` is set. The same applies to the `path_sweep`
  entry's header line.
- Geometry numbers carry full double precision in `render` and `snapshot`
  (`"area": 9834.381290988327`). Rounding to 1e-6, as `measure` does,
  saves about 10% of those results (see also finding 11).
- **Snapshot image tokens**: an image is billed by pixels, not base64
  bytes. I did not retrieve Anthropic's current formula, so no number is
  given. The 8192×8192 cap returns a 683 KB base64 PNG in 0.46 s. That
  costs bandwidth, not necessarily tokens, if the client downscales.

### 10. MCP accepts wrongly-typed arguments silently, and names CLI flags in errors (low)

- `check {"nozzle":"big"}`, `snapshot {"views":"iso"}` and
  `render {"parts":"yes"}` are all treated as absent: the defaults run
  and nothing says the argument was ignored. An agent that sends
  `"parts": "true"` gets "no part 'a'" and no reason. Refuse a wrong type
  with `isError` and the expected type.
- Error texts use command-line flag names that an MCP client has never
  seen, and serve's `-32602` messages do the same:

  | Where | Text |
  |---|---|
  | `snapshot` | `--size must be WxH, 64 to 8192 each` |
  | `measure` | `--between takes two part names` |
  | `measure` | `--section must be z=MM ...` |
  | `check` | `--max-overhang must be 0 to 90 degrees` |
  | `check` | `--bed must be WxDxH` |
  | serve `check` | `--nozzle must be a positive number` |
  | `measure` | `they need --enable part` |

  Name the argument as the caller spelled it; the warning on `part` does
  already mention "the `parts` request option".

### 11. Consistency of field names, units and documentation (low)

Units are consistent: mm, mm², mm³, and degrees from vertical everywhere.
The names and shapes are not:

- **Diagnostics**: the CLI and serve use `hints: [{"message"}]`, while
  MCP structured content uses `hint: "..."` (a string, singular).
- **`counts` has four meanings**: `{errors, warnings, echoes}` for runs,
  `{errors, warnings, info}` for `check` (where `errors` are *findings*,
  not diagnostics), `{tests, passed, failed, files}` for `test`, and
  `{files, changed, errors}` for `fmt`.
- **Precision**: `check` rounds to 1e-4 (`9834.3813`), `measure` to 1e-6
  (`9834.381291`), and `render` and `snapshot` not at all
  (`9834.381290988327`). An agent comparing one area across tools sees
  three different numbers.
- **`test`**: the CLI JSON has `tests[].id` and `FAILURE` objects, while
  MCP has `failed[].test` and failures as strings. `docs/mcp.md` does not
  describe the MCP shape.
- **Snapshot diagnostics** are `{errors, warnings, echoes, messages,
  echo, items}` rather than the run's `counts` plus `diagnostics`. This
  is documented and acknowledged in `tools.rs:502`.

Documentation that disagrees with behaviour:

- `docs/serve-protocol.md` says an out-of-range `update` "changes
  nothing", but it bumps the version: `open` → 1, a rejected `update` →
  error -32602, then a good `update` → **3**. An LSP-style client that
  checks versions will see a gap.
- `docs/mcp.md` gives the tool list as 5,404 bytes. That figure is
  measured as keyless `[name, description, schema]` arrays
  (`crates/cli/tests/mcp.rs:172-177`). What a client receives is 5,688
  bytes with keys and 5,979 with `annotations` (compact JSON).
- `docs/mcp.md` "Safety": the symlink claim (finding 2).
- `docs/architecture.md` lists `lsp`, `mcp` and `wasm` crates. None
  exists: MCP is `crates/cli/src/mcp/`, and there are no LSP or wasm
  crates yet, only `wasm-check`.
- `docs/cli-json.md`'s snapshot example omits `lighting`, which the
  output has. It is documented in the bullets; update the example.

Spot-checks that matched their documents:

- `cli-json.md`: the summary example byte for byte, the snapshot JSON
  keys, the run object's keys, the diagnostic shape, `check` settings
  and model keys, `docs` unknown-name, and `fmt --stdin`.
- `serve-protocol.md`: the `initialize` capabilities, the example
  session with its progress and diagnostics notifications, -32602,
  -32601, -32700, `status` and `stats` keys, and `docs`.
- `mcp.md`: the open-box `render` example (identical text and
  structured content), `resources/list`, `server/discover`, -32022 with
  the supported versions, and -32602 without `clientCapabilities`.
- `model-tests.md`: `examples/tests` (7 pass), `volum` → "bad @expect",
  `±2%`, `[[min],[max]]` bbox, `check clean`, a file that does not parse,
  and the JSON keys.
- `agent-eval.md`: `--check-graders` (4 of 4 pass) and `--dry-run`.

## `check` on real models: verdicts

`neoscad check --no-server --format json`, default settings, with
`OPENSCADPATH=.reference`.

| Model | Findings | Verdict |
|---|---|---|
| BOSL2 `spring_handle` | 4 thin-wall errors (0 mm), 36 overhangs, off-bed | Thin walls are **noise** (finding 4). Overhangs are real: the underside of the horizontal rod and wire. Correct otherwise |
| BOSL2 `fractal_tree` | 1,008 floating errors, 1,568 overhangs | **Correct.** The nightly's mesh also has 1,009 shells (genus -1008); leaves sit about 2.5 mm from the branch tips (a scratch point-to-triangle distance over 60 leaves) |
| BOSL2 `boolean_geometry` | 3 thin-wall errors, 0.1 mm | **Correct**: `linear_extrude(height=0.1)` plates |
| BOSL2 `BOSL2logo` | 4 floating, 5 thin-wall (0.5 mm), 270 overhangs | Floating is correct (five showcase objects laid out at different heights). The 0.5 mm walls are plausible (truss struts); not verified further |
| BOSL2 `attachments` | 9 overhangs | Plausible |
| BOSL2 `jigsaw_puzzle` | not-3d | Correct (a 2D model) |
| OpenSCAD `CSG` | 2 floating, 10 thin-wall, 4 overhangs | Correct: three objects centred on z = 0 with different lowest points. Thin walls are the real feather edges where a sphere breaks through a cube face |
| OpenSCAD `example004` | thin-wall ×10 (0.11 mm) | Correct: `cube(30)` minus `sphere(20)` leaves knife edges (the followup's "correctly thin, but many") |
| OpenSCAD `rotate_extrude` | 3 floating (0.14 mm) | Correct by definition: the "J" glyph's descender goes 0.1356 below z = 0. Borderline for printing |
| OpenSCAD `GEB` | 1 floating, thin wall 0.008 mm | Plausible (a disconnected piece inside the letter intersection); not verified further |
| OpenSCAD `logo`, `example002`, `text_on_cube` | overhangs, off-bed | Correct |
| OpenSCAD `example024` | none | Correct |

Overhang counts are large on curved models (270 on BOSL2logo, 1,568 on
fractal_tree), but they are truncated at 10 per code. Bridges count as
overhangs, as already recorded in the followups.

`measure` against the nightly's mesh (scratch script: signed-tetrahedron
volume and centroid, triangle area):

| Model | `measure` volume | Mesh volume | Centroid (both) |
|---|---|---|---|
| spring_handle | 11709.3242 | 11709.3231 | [0, 0, 0.0068] |
| rotate_extrude | 7903.1719 | 7903.1843 | [17.5646, 21.1819, 5.6824] |
| example024 | 203221.0537 | 203220.9911 | [0, 0, 25.7066] |
| BOSL2logo | 57465.0844 | 57464.9349 | [-6.347, 0.3247, 0.4613] |
| GEB | 4729.9840 | 4729.9840 | [-6.0857, 6.0720, -6.0347] |

The differences, at most 2.6e-6 relative, come from the OFF text
precision and slightly different triangulations (for example 55,984
against 55,736 triangles on BOSL2logo).

## Checked and found fine

- **Read fence** (`RootedFs`, `crates/cli/src/mcp/roots.rs:145-176`).
  Refused, with the same message as for a missing file:
  - `include <../outside/x.scad>`, an absolute `include`, and `include`
    through a symlinked directory;
  - `import()` of an existing outside STL, a missing outside STL, and
    `/etc/hosts`;
  - `surface()` and `dxf_dim(file=)` of outside files;
  - `use <outside/f.ttf>` (the font is not loaded).

  `path` with `..` or through a link, `base_dir: "/etc"`, and the `path`
  of `docs`, `test` and `format` were refused with `isError` and a
  message naming the roots and `--root`. `resources/read
  neoscad://docs/../../etc/passwd` returned -32002. The read-only roots
  are only the library path, `~/.fonts`, `NEOSCAD_FONT_DIR` and
  `OPENSCAD_FONT_PATH` (`mcp/mod.rs:93-103`).
- **Write fence, apart from finding 2**: `/tmp/x.stl` and `lnk/x.stl`
  (a link to an outside directory) were refused.
- **Command execution**: `-m` runs only from `deps::add_node_files`,
  which only the in-process `run.rs` calls. The server's `cli.export`
  goes to `delegate::execute`, which has no make parameter, and
  `delegate.rs:17-21` keeps `-m` runs local. `run_make` quotes the file
  name correctly (`deps.rs:80`, `'\''`). `neoscad mcp` spawns no
  processes.
- **Socket permissions** (macOS): the default directory is
  `$TMPDIR/neoscad-501`, mode 0700 and owner-checked
  (`serve.rs:1037-1047`); the socket is 0600, and an explicit `--socket`
  is 0600 too. Another local user cannot connect.
- **Protocol robustness**: 15 malformed calls produced errors or
  defaults, never a crash: missing `path` or `source`, `path: 5`, size
  `0x0`, a one-name `between`, `section: "w=3"`, `max_overhang: 400`, an
  unknown tool, an unknown view, and a bad `format` source. The server
  exited cleanly at end of input. Tool panics are caught
  (`tools.rs:236`, `serve.rs:294`).
- **Recursion and iteration limits**: recursion to depth 1e8 in a
  function or 1e6 in a module stops with `recursion-limit` in 20 ms. `for`
  ranges over 1e6 elements warn `iteration-limit`, as OpenSCAD does.
- **Snapshot size** is capped at 64–8192 pixels each way
  (`session/src/snapshot.rs:315-339`). The largest took 0.46 s and about
  750 MB peak RSS.
- **stdout discipline**: every line on MCP stdout parsed as JSON-RPC
  across about 80 calls, including `echo` output.
- **Error hints for common mistakes are good**:
  - "did you mean 'cube'" for `cub`;
  - "module cube() does not support child modules" for a missing `;`;
  - an unknown variable (`tru`, `x`) with a lexical-scope hint;
  - reassignment "OpenSCAD keeps the last value at the first position";
  - 3D-in-2D;
  - `not-3d` with the fix "extrude it".
- **Graders**: `scripts/agent-eval/run.py --check-graders` passes 4 of 4.

## Followups triage (the sections asked about)

Most of these can wait. The ones to settle **before the macOS app** are
those whose effects a GUI user sees directly, or that the app inherits
by embedding the session.

**Before phase 8, or inside it:**

- *Serve and session.* "Cancellation ... one long kernel operation runs
  to its end." Together with finding 1, this decides whether typing
  during a heavy render keeps the app responsive. Needs at least
  primitive and extrusion budgets and a deadline.
- *Serve and session.* "The session keeps up to four renderers ... four
  budgets." An app with several open documents needs one process-wide
  geometry budget and a memory-pressure hook (the app can listen for
  `DispatchSource.makeMemoryPressureSource`).
- *Serve and session.* Panic unwinding: keep it. The app requires it.
  But `catch_unwind` lives in `cli` (`serve.rs:294`, `tools.rs:236`), not
  `session`, so the app's FFI layer must add its own (see
  `macos-prep.md`).
- *MCP.* "`neoscad serve`'s `export` result says nothing when the output
  cannot be written" — fix with finding 5. The app's export UI needs the
  reason.
- *Parts, check and measure.* Thin-wall sampling: fix the seam noise
  (finding 4) before `check` appears in the app's issue panel, where
  users will see "error" badges.
- *Tooling.* `docs --in` output: the `../../` paths and private names
  (finding 9) will be the app's hover and completion text.

**Can wait:**

- *Serve and session*: incremental parsing of includes (33 ms re-render
  is fine at editor latencies); the CLI export path not using the session
  (tests pin agreement); served summary cache counts; Unix-socket-only
  (Windows is out of scope); stage-only `progress` (a spinner suffices);
  scoped "did you mean"; headlight image tests (add them when the
  viewport lands); replay recompute.
- *Parts, check and measure*: uncut part solids; hull and minkowski
  dropping parts; bridges; serial checks (150 ms is acceptable on
  demand); `snapshot --issues` occlusion (the app draws its own
  overlay); per-part sections.
- *Tooling*:
  - Replace the hand-copied "did you mean" pools with `eval::builtins()`.
    Cheap; do it with finding 6.
  - Formatter layout limits, the customizer-header rule, and `fmt`
    without `--enable`.
  - `fmt` rewriting in place without temp-and-rename. Command line only:
    the app formats buffers. Cheap to fix anyway.
  - `test` not served; `@expect` gaps.
- *MCP and the agent eval*: no progress notifications or `roots`; text
  plus structured doubling (Claude Code shows only one); inline source
  as one document per `base_dir`; the snapshot header overlap; tokenizer
  counts; `&lt;` hint (fold into finding 6); a real eval with n > 1 and
  graders' `@expect empty`.
