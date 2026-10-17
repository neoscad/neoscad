# Project dependencies

A project can declare the libraries it includes (BOSL2, NopSCADlib,
threads-scad, a library of its own) in a `scad.toml`, pin each to an
exact commit and content hash in `scad.lock`, and have them appear under
`<project>/.scad/libraries`, which every NeoSCAD host searches for the
project's files. `include <BOSL2/std.scad>` then finds the pinned copy:
the source does not change, and stock OpenSCAD can still render the
project after `neoscad vendor`.

This is option A of the proposal in issue #1 (manifest, lockfile and a
managed search path), with option B (project search-path entries) as the
manifest's `[paths]` table. The code is the `scad-deps` crate
(`crates/project`, a host crate), which is published on crates.io on its
own so that stock-OpenSCAD users and other tools share the same files
and store: its standalone `scad-deps` binary (`cargo install scad-deps
--features cli`) has the same commands as `neoscad`, which runs
`scad_deps::cli` under its own name (`crates/cli/src/pkg.rs`).

**The formats are specified in the crate's README,
[`crates/project/README.md`](../crates/project/README.md)**: the
manifest's fields, the lockfile's fields, order and bytes, the `inputs`
and content hashes, the search order, the store's layout, the links and
the resolution policy. This page is NeoSCAD's user guide and the design
record.

## Quick start

    neoscad add BOSL2 --git https://github.com/BelfrySCAD/BOSL2 --tag v2.0.716
    neoscad main.scad -o main.stl          # include <BOSL2/std.scad> just works

`neoscad add` creates `scad.toml` if the directory has none, resolves the
new dependency, writes `scad.lock` and links `.scad/libraries/BOSL2`.
Commit `scad.toml` and `scad.lock`. `.scad/` is machine-local (its links
name this machine's store): `neoscad fetch` writes `.scad/.gitignore`
ignoring everything in it, so it is never committed and the project's own
`.gitignore` need not change. On a fresh clone, `neoscad fetch` makes the
links again; on a machine that already has those revisions it runs no git
at all.

## The manifest and the lockfile

```toml
[package]
name = "sky-view-shed"            # optional; the directory name otherwise

[dependencies]
BOSL2   = { git = "https://github.com/BelfrySCAD/BOSL2", tag = "v2.0.716" }
threads = { git = "https://github.com/rcolyer/threads-scad", branch = "master", name = "threads-scad" }
parts   = { path = "../shared-parts" }
nop     = { git = "https://github.com/nophead/NopSCADlib", tag = "v21.0.0", include_root = "." }

[paths]
libraries = ["lib"]               # project folders searched after .scad/libraries
```

A dependency is a git repository (`git = URL` with at most one of `tag`,
`rev` and `branch`; any URL git accepts, with the user's own credentials)
or a local directory (`path = DIR`, linked as it is, so edits show
through at once). `include_root` names the folder holding the library's
files and `name` the folder `include <Name/…>` uses, if not the key.
`neoscad add` and `neoscad remove` edit the file line by line and keep
comments, order and other tables as written. `[paths] libraries` adds
folders the project keeps itself (a submodule, a vendored folder).

`scad.lock` holds one `[[package]]` per dependency the project uses, its
own and its dependencies', with what was asked for beside the commit and
content hash it resolved to, and an `inputs` hash of the manifests: while
that matches, `neoscad fetch` links the locked packages without
resolving anything. It is written byte for byte (the same graph always
gives the same file, whichever tool wrote it) and holds no
machine-specific path. The README has every field.

## Commands

| Command | What it does |
|---|---|
| `neoscad add NAME --git URL [--tag T \| --rev R \| --branch B] [--include-root DIR] [--name FOLDER]` | Add or replace a git dependency, then fetch. A fetch that fails leaves `scad.toml` as it was. |
| `neoscad add NAME --path DIR` | Add a path dependency, then fetch. |
| `neoscad remove NAME` | Remove a dependency, then fetch (the link goes). |
| `neoscad fetch [--locked] [--offline] [--print-path]` | Resolve what `scad.lock` does not pin yet, write `scad.lock`, check out what the store lacks, verify, link. |
| `neoscad update [NAME...]` | Ask the remotes again (all dependencies, or the NAMEs): branches and moved tags move, then fetch. |
| `neoscad tree` | The locked graph from the project down. |
| `neoscad vendor [DIR]` | Fetch, then copy every dependency into a plain folder (default `<project>/libraries`) and print `OPENSCADPATH=DIR`. |
| `neoscad store list` / `gc [--dry-run]` / `path` | The shared store: what it holds, its sizes and users; remove what no project uses; where it is. |

Every command that fetches takes `--project DIR` (default: the working
directory; the nearest `scad.toml` at or above it is the project),
`--store DIR`, `--offline`, `--locked` and `--format json` (one JSON
object on stdout, `schema: 1`, with `cloned`, `fetched`, `checked_out`,
`linked`, `lock_changed` and the packages). Progress and summaries go to
stderr; `--print-path` and `vendor` print their one line on stdout, so
`export $(neoscad vendor)` works in a shell.

The standalone `scad-deps` binary takes the same commands with the same
output (its messages start `scad-deps:` instead).

- `--locked` (for CI) fails if `scad.lock` is missing, out of date with
  `scad.toml`, or would change in any way.
- `--offline` never runs a git command that reaches the network: it
  resolves from the mirrors already in the store and fails, naming the
  dependency, when one is missing.

## Where includes are found

For a file inside a project (the nearest `scad.toml` at or above the
file's directory), the search order is:

1. the including file's own directory (OpenSCAD's rule, unchanged);
2. `<project>/.scad/libraries`;
3. the project's `[paths] libraries`, in order;
4. `OPENSCADPATH`, the user library folder and the bundled libraries, as
   in OpenSCAD.

So the pinned BOSL2 wins over a BOSL2 in `~/Documents/OpenSCAD/libraries`,
and a project with no `scad.toml` resolves exactly as OpenSCAD does.

Every host does this: the command line's exports and `neoscad check`,
`measure`, `snapshot`, `test` and `docs --in`; `neoscad serve` and the
command line through it; the MCP server (`neoscad mcp`, which also makes
the store readable, as it does every library directory); the language
server (`neoscad lsp`, so completion, hover and go-to-definition reach
into dependencies); and the macOS, Windows and Linux apps. Hosts find the project
(`scad_deps::search::project_for`, a few small reads and no git), turn
its plain answer into a `lang::project::ProjectLibs` (`project_libs` in
each host's `host.rs`: scad-deps depends on no NeoSCAD crate) and hand
that to the session through `session::Config::project`; the library
crates never look for a project themselves. The web demo has no disk and is unchanged: its
library path is what the page provides.

The session's parse cache keys a main file's parse by its library path
and a stamp of the project's links, so a warm session (`serve`, the MCP
server, an editor) follows a `neoscad fetch` or `update` at once.

## Diagnostics

An include that does not resolve inside a project gets a hint, in the
JSON diagnostics (`hints`, so in MCP and the LSP too) and on the command
line after OpenSCAD's warning:

    WARNING: Can't open include file 'BOSL2/std.scad'. in file main.scad, line 1
    neoscad: hint: `BOSL2` is declared in scad.toml but not fetched: run `neoscad fetch`

An include whose first folder no dependency declares gets ``Other` is not
a dependency in scad.toml: add it with `neoscad add Other --git URL --tag
TAG` and run `neoscad fetch``. Outside a project, nothing changes.

## OpenSCAD compatibility

The source needs nothing new. Two ways to render a project with stock
OpenSCAD:

    export $(neoscad vendor)                       # copies into ./libraries
    openscad -o main.stl main.scad

or, without copying, point `OPENSCADPATH` at the managed links:

    OPENSCADPATH=$(neoscad fetch --print-path) openscad -o main.stl main.scad

Vendored copies are plain, writable files (no links into the store), so
the folder can be committed or archived for an air-gapped build.

## Design record

### The store

Every revision is fetched and checked out once per machine, then linked
into each project that uses it; the README specifies the layout, the
names and the platform locations (`NEOSCAD_STORE` or `--store` moves it;
`neoscad store path` prints it). The choices behind it:

- **Mirrors** are like Cargo's git database: one bare repository per
  remote, so a later revision costs a `git fetch` of the delta.
- **Checkouts** are like the pnpm or Nix store: one read-only tree per
  commit (and `include_root`), written with a throwaway index so the
  mirror is not changed and the tree has no `.git`, and no project can
  commit, check out or dirty it for another. Git runs with the user's and
  the system's configuration off for this step, so line-ending settings
  or filters on one machine cannot change the bytes or the content hash.
  Unlike pnpm, checkouts are keyed by commit and verified by their
  content hash rather than addressed by it, so one can be found before it
  is hashed. Every `neoscad fetch` checks each checkout it links against
  `scad.lock`'s hash and never replaces one that fails silently.
- **Concurrency**: an OS file lock on the store, and checkouts that
  appear in one rename, so two projects fetching at once take turns and
  the second does no git work for what the first fetched.
- **Index and gc**: each fetch records which checkouts and mirrors the
  project links; `neoscad store gc` removes what no live project lists.
- **Links**: symbolic links, and on Windows a directory junction (no
  privilege needed) where symbolic links are not allowed, else a copy.

### Resolution

Breadth first from the project, then one node per key by minimal version
selection as Go modules do it (the highest of version-like tags wins;
exact revisions must agree), with every package linked flat into
`.scad/libraries`, since SCAD's include namespace is flat and one copy
per name is the only workable policy. The README has the rules. A
conflict names both requirers:

    conflicting requirements for `B`: `app` wants rev 8c3f1a92e4 (commit 8c3f1a9), `A` wants tag v1.0.0 (commit 21d0b57); exact revisions must agree (only version tags like v1.2.3 are reconciled), so make them name the same commit

A requirement that matches a lockfile entry exactly keeps the locked
commit, so editing one dependency never moves another and `neoscad
fetch` never moves a branch by itself; `neoscad update` fetches the
mirrors and resolves the named keys (or all) afresh.

**Fast path.** When `scad.lock` exists, its `inputs` hash matches the
manifests and every locked checkout is in the store, `neoscad fetch`
resolves nothing and runs no git: it checks each checkout's hash, fixes
any link that does not point at it and rewrites the store index.

### Security

Fetching never executes code from a dependency (no build scripts, no git
hooks run by `checkout-index`), and evaluation is pure. The `import()` of
meshes and fonts from dependencies is not limited to the dependency tree
yet (see `docs/followups.md`).

### Against the proposal

Done: the manifest, the lockfile with commits and content hashes, the
managed search path in every native host, `add`/`remove`/`fetch`/
`update`/`tree`/`vendor`, `--locked`, `--offline`, the shared store
(pnpm-style, but keyed by commit and verified by content hash rather
than addressed by it, so a checkout can be found before it is hashed),
minimal version selection for diamonds, libraries without manifests
(`include_root`, `name`), the "declared but not fetched" diagnostic, and
option B as `[paths]`. Not yet: the app's "Fetch missing libraries"
banner and fetching from the MCP server or the LSP (they report the
hint), a `docs` index of every dependency's modules (`docs --in FILE`
does follow a file's includes into them), the web demo (no disk; a fetch
proxy or vendored files would be needed), an index of well-known
libraries (option C), and limiting `import()` to the dependency tree.
