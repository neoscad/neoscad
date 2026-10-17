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
manifest's `[paths]` table. The code is the `project` crate (a host
crate) and the commands in `crates/cli/src/pkg.rs`.

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

## The manifest: `scad.toml`

```toml
[package]
name = "sky-view-shed"            # optional; the directory name otherwise

[dependencies]
BOSL2       = { git = "https://github.com/BelfrySCAD/BOSL2", tag = "v2.0.716" }
FramingSCAD = { git = "https://github.com/the-ned-workshop/FramingSCAD", rev = "21645b1" }
threads     = { git = "https://github.com/rcolyer/threads-scad", branch = "master", name = "threads-scad" }
parts       = { path = "../shared-parts" }
nop         = { git = "https://github.com/nophead/NopSCADlib", tag = "v21.0.0", include_root = "." }

[paths]
libraries = ["lib"]               # project folders searched after .scad/libraries
```

A dependency is either

- **git**: `git = URL` with at most one of `tag`, `rev` (a commit id, at
  least 4 hex digits) and `branch`; none means the remote's default
  branch. Any URL git accepts works (https, ssh, `git@host:path`,
  `file://`), with the user's own git credentials and configuration;
- **path**: `path = DIR`, relative to the manifest's directory. It is
  linked as it is, never copied, so edits show through at once.

and may add

- `include_root = "subdir"`: the folder of the repository that holds the
  library's files (`.` or nothing for the root);
- `name = "Folder"`: the folder name `include <Folder/…>` uses, if not the
  key.

The key is the dependency's identity in the graph: one revision per key.
Folder names must be unique. Other tables and keys of `[package]` (a
version, a description) are ignored, so other tools may use the same
file. `neoscad add` and `neoscad remove` edit the file line by line and
keep comments, order and other tables as written.

`[paths] libraries` adds directories (relative to the project root) that
the project keeps itself, such as a git submodule or a vendored folder.
They are searched after `.scad/libraries` and need no fetching.

## The lockfile: `scad.lock`

```toml
# scad.lock: the exact revisions of scad.toml's dependencies, written by
# `neoscad fetch`. Commit it with scad.toml; do not edit it by hand.
version = 1
inputs = "sha256:274541f0…"

[[package]]
name = "BOSL2"
source = "git+https://github.com/BelfrySCAD/BOSL2"
tag = "v2.0.716"
commit = "9948313433…"            # the full commit id
hash = "sha256:f135f3e4…"         # the checkout's content hash
dependencies = []

[[package]]
name = "parts"
source = "path+../shared-parts"
dependencies = []
```

One `[[package]]` per dependency the project uses, its own and its
dependencies' (sorted by name), with what was asked for beside what it
resolved to. `inputs` is a hash of every requirement the project and its
path dependencies declare; while it matches the manifests, `neoscad
fetch` links the locked packages without resolving anything. The file is
written byte for byte by `crates/project/src/lock.rs`, so the same graph
always gives the same file, and it holds no machine-specific path (path
dependencies are relative to the project root).

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
into dependencies); and the macOS, Windows and Linux apps. Hosts find the project (`project::search::project_for`, a few small
reads and no git) and hand the session a `lang::project::ProjectLibs`
through `session::Config::project`; the library crates never look for a
project themselves. The web demo has no disk and is unchanged: its
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
into each project that uses it. The store lives in the platform cache
directory: `~/Library/Caches/NeoSCAD/store` on macOS,
`$XDG_CACHE_HOME/neoscad/store` (default `~/.cache/neoscad/store`) on
Linux, `%LOCALAPPDATA%\NeoSCAD\store` on Windows; `NEOSCAD_STORE` or
`--store` moves it, and `neoscad store path` prints it. Two projects using
BOSL2 at the same tag:

```text
~/Library/Caches/NeoSCAD/store/
├── git/
│   └── BOSL2-7d0ce70e5ba7e0c7.git/     bare mirror of the remote (one per URL)
├── checkouts/
│   └── BOSL2-994831343316/             the tree at that commit, read-only, no .git
├── projects/
│   ├── 3f2a9c1e0b7d4a55.toml           root = ".../sky-view-shed", checkouts, mirrors
│   └── 8c01d2e4f6a7b9c3.toml           root = ".../garden-planter", ...
├── tmp/                                checkouts and clones being made
├── lock
└── empty-gitconfig

sky-view-shed/
├── scad.toml
├── scad.lock
├── main.scad                           include <BOSL2/std.scad>
└── .scad/
    ├── .gitignore                      *
    └── libraries/
        └── BOSL2 -> ~/Library/Caches/NeoSCAD/store/checkouts/BOSL2-994831343316
```

- **Mirrors** (like Cargo's git database): one bare repository per remote,
  named by the URL's last component and a hash of the normalised URL
  (lower-case scheme and host, no trailing `/` or `.git`). The first use
  is a `git clone --bare`; a later revision costs a `git fetch` of the
  delta (every branch and tag, moved tags forced, deleted ones pruned).
- **Checkouts** (like the pnpm or Nix store): one tree per commit (and
  `include_root`, whose hash is added to the name when given), made from
  the mirror's objects with a throwaway index (`git read-tree` and
  `checkout-index` with `GIT_INDEX_FILE` in `tmp/`), so the mirror is not
  changed and the tree has no `.git`: no project can commit, check out or
  dirty it for another. Submodules and LFS objects are not fetched. The
  tree is written in `tmp/`, hashed, renamed into place and made
  read-only (files, and on Unix directories too). Git runs with the
  user's and the system's configuration off for this step
  (`GIT_CONFIG_GLOBAL` an empty file, `GIT_CONFIG_NOSYSTEM`), and with
  `core.autocrlf=false` and `core.eol=lf`, so line-ending settings or
  filters on one machine cannot change the bytes, and the content hash is
  the same everywhere.
- **Concurrency**: every operation that changes the store holds `lock`
  (an OS file lock, `File::lock`), so two projects fetching at once take
  turns; the second finds the first's mirror and checkout and does no
  git work for them. A checkout appears in one rename, so a reader never
  sees half of one.
- **Index and gc**: each fetch rewrites `projects/<hash of the project
  root>.toml` with the checkouts and mirrors that project links.
  `neoscad store gc` drops the entries of projects whose `scad.toml` is
  gone, then removes every checkout and mirror no remaining project lists
  (and anything left in `tmp/`). `neoscad store list` shows each entry's
  size and how many projects use it.
- **Links**: `.scad/libraries/<Name>` is a symbolic link to the checkout,
  or to a path dependency's directory. On Windows it is a directory
  symbolic link where the user may make one (Developer Mode), else a
  directory junction (`mklink /J`, no privilege needed, local volumes
  only), else a copy. `neoscad fetch` relinks only what changed and
  removes entries that are no longer dependencies.

### The content hash

Recorded in `scad.lock` and checked by every `neoscad fetch` against the
checkout it links (a checkout that fails is reported with its path and
both hashes, and is never replaced silently). Written
`sha256:<64 lowercase hex digits>`, version 1 (`crates/project/src/hash.rs`):

1. list every regular file and every symbolic link under the tree's root,
   recursively, without following links (an empty directory contributes
   nothing, as git does not track it);
2. name each by its path relative to the root, components joined by `/`,
   as UTF-8, and sort by those bytes;
3. SHA-256 the concatenation, over the entries in order, of
   `path bytes · 0x00 · content length as 8 bytes big-endian · content`,
   where a file's content is its bytes and a link's is its target as
   written (`/`-separated).

Modes, times and owners are not part of it, and a link hashes like a file
holding its target, so a Windows checkout without symbolic links hashes
the same as a Unix one.

### Resolution

Breadth first from the project: every requirement becomes a node, a git
one resolved to a commit in its mirror and a path one to its directory. A
git dependency's own `scad.toml` (read from its commit with `git
cat-file`, at the repository root, or else at its `include_root`) and a
path dependency's add their requirements; libraries without a manifest
are leaves. A git dependency may not declare path dependencies (they
would point outside its repository).

Then each key gets one node, by minimal version selection as Go modules
do it: when every requirement of a key is a tag that reads as a version
(`v1.2.3`, `1.2`, `v2.0.716-rc1`; `v` optional, a pre-release sorts
before its release), the highest wins. Otherwise all requirements must
name the same commit, or the error names two requirers and what each
wants:

    conflicting requirements for `B`: `app` wants rev 8c3f1a92e4 (commit 8c3f1a9), `A` wants tag v1.0.0 (commit 21d0b57); exact revisions must agree (only version tags like v1.2.3 are reconciled), so make them name the same commit

Sources must agree too: one URL (as normalised), one `include_root`, one
folder name. The result is pruned to what the project reaches through
the selected nodes, and every package is linked flat into
`.scad/libraries` (SCAD's include namespace is flat, so one copy per
name is the only workable policy).

A requirement that matches a lockfile entry exactly (key, URL, revision,
`include_root`) keeps the locked commit, so editing one dependency never
moves another and `neoscad fetch` never moves a branch by itself.
`neoscad update` fetches the mirrors and resolves the named keys (or all)
afresh. A mirror is fetched at most once per run, and only when needed:
it is missing a ref or commit, a branch is not locked yet, or the key is
being updated. A tag already in the mirror is taken as it is (tags are
meant to be immutable; `update` notices a moved one).

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
