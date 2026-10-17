# scad-deps

A dependency manager for OpenSCAD-language projects. A project declares
the libraries it includes (BOSL2, NopSCADlib, threads-scad, a library of
its own) in `scad.toml`; `scad.lock` pins each to an exact commit and a
content hash; a fetch checks every revision out once per machine into a
shared store and links it into `<project>/.scad/libraries`. Then
`include <BOSL2/std.scad>` finds the pinned copy. The `.scad` sources
need no changes, so stock OpenSCAD can still render them.

It is developed for [NeoSCAD](https://github.com/neoscad/neoscad), whose
`neoscad add`, `fetch` and the rest are this crate's command line under
NeoSCAD's name. Nothing in it is specific to NeoSCAD: the files, the
store and the command line work the same for stock OpenSCAD and any
other tool. This README is the format's specification (version 1).
NeoSCAD's user guide, with how its hosts use the search path and the
design record, is
[`docs/dependencies.md`](https://github.com/neoscad/neoscad/blob/main/docs/dependencies.md).

## Quick start

With NeoSCAD:

    neoscad add BOSL2 --git https://github.com/BelfrySCAD/BOSL2 --tag v2.0.716
    neoscad main.scad -o main.stl          # include <BOSL2/std.scad> just works

With stock OpenSCAD, install the standalone command line (it needs `git`
on the `PATH`):

    cargo install scad-deps --features cli
    scad-deps add BOSL2 --git https://github.com/BelfrySCAD/BOSL2 --tag v2.0.716
    OPENSCADPATH=$(scad-deps fetch --print-path) openscad -o main.stl main.scad

or copy the dependencies into a plain folder (default `./libraries`),
for example to commit it or to build without network access:

    export $(scad-deps vendor)             # prints OPENSCADPATH=<dir>
    openscad -o main.stl main.scad

`add` creates `scad.toml` if there is none, resolves, writes `scad.lock`
and links `.scad/libraries/BOSL2`. Commit `scad.toml` and `scad.lock`.
`.scad/` is specific to one machine and is never committed (a fetch
writes `.scad/.gitignore` for it). On a fresh clone, `scad-deps fetch`
(or `neoscad fetch`) recreates the links. If the store already holds
those revisions, it runs no git at all.

## Commands

`scad-deps` and `neoscad` take the same commands, give the same output
and share the same store:

| Command | What it does |
|---|---|
| `add NAME --git URL [--tag T \| --rev R \| --branch B] [--include-root DIR] [--name FOLDER] [--no-fetch]` | Add or replace a git dependency, then fetch. If the fetch fails, `scad.toml` is left as it was. |
| `add NAME --path DIR` | Add a path dependency, then fetch. |
| `remove NAME [--no-fetch]` | Remove a dependency, then fetch (which removes its link). |
| `fetch [--locked] [--offline] [--print-path]` | Resolve what `scad.lock` does not pin yet, write `scad.lock`, check out what the store lacks, verify, link. `--print-path` prints the project's library directories, `OPENSCADPATH`-style, on stdout. |
| `update [NAME...]` | Ask the remotes again (for every dependency, or only the NAMEs): branches and moved tags move. Then fetch. |
| `tree` | The locked graph, from the project down. |
| `vendor [DIR]` | Fetch, then copy every dependency into a plain folder (default `<project>/libraries`) and print `OPENSCADPATH=DIR`. |
| `store list` / `store gc [--dry-run]` / `store path` | The store: what it holds, with sizes and users; remove what no project uses; print where it is. |

Every command except `store` takes `--project DIR` (default: the working
directory; the project is the nearest directory at or above it holding
a `scad.toml`), and every command takes `--format json` (one JSON object on stdout, `"schema": 1`). The
commands that fetch also take `--store DIR`, `--offline` (no git command
that reaches the network) and `--locked` (fail if `scad.lock` is
missing, out of date or would change: for CI). Progress and summaries
go to stderr. The exit status is 0 on success and 1 on failure.

As a library: `scad_deps::fetch`, `add`, `remove`, `tree` and `vendor`
are the operations, and `scad_deps::search::project_for(main_file)`
gives a host the directories to search for one file, plus the
dependencies it declares (to explain an include that does not resolve).
The `cli` feature adds `scad_deps::cli`, the command line itself.

## Specification (version 1)

### The manifest: `scad.toml`

```toml
[package]
name = "sky-view-shed"            # optional; the directory name otherwise

[dependencies]
BOSL2   = { git = "https://github.com/BelfrySCAD/BOSL2", tag = "v2.0.716" }
Framing = { git = "https://github.com/the-ned-workshop/FramingSCAD", rev = "21645b1" }
threads = { git = "https://github.com/rcolyer/threads-scad", branch = "master", name = "threads-scad" }
parts   = { path = "../shared-parts" }
nop     = { git = "https://github.com/nophead/NopSCADlib", tag = "v21.0.0", include_root = "." }

[paths]
libraries = ["lib"]               # project folders searched after .scad/libraries
```

- The top-level tables are `[package]`, `[dependencies]` and `[paths]`.
  Any other top-level key is an error, so future versions can add one.
  Only `name` is read from `[package]`. Its other keys (a version, a
  description) are ignored, so other tools can keep their own data
  there.
- A `[dependencies]` key is the dependency's identity in the graph, and
  one revision of it is selected. Unless `name` says otherwise, the key
  is also the folder name that `include <Key/…>` uses. Folder names must
  be unique, non-empty, not `.` or `..`, must not start with `.` and must
  not contain `/ \ : < > " | ? *`.
- A **git** dependency has `git = URL` and at most one of `tag`, `rev`
  (a commit id: at least 4 hex digits, compared in lower case) and
  `branch`. With none of them, it follows the remote's default branch.
  The URL can be anything git accepts (https, ssh, `git@host:path`,
  `file://`); the user's own git credentials and configuration apply.
- A **path** dependency has `path = DIR`, relative to the directory of
  the manifest that declares it. It is linked as it is, never copied,
  and takes no `tag`, `rev` or `branch`.
- `include_root = "subdir"` names the folder of the source that holds
  the library's files. It is `/`-separated and relative, with no `..`.
  `.` or an empty value means the root.
- `name = "Folder"`: the folder name, when it is not the key.
- `[paths] libraries` lists directories, relative to the project root,
  that the project keeps itself (a git submodule, a vendored folder).
  They need no fetching.

A dependency's own `scad.toml` adds its dependencies to the graph. For a
git dependency, the manifest is read at the resolved commit, from the
repository root or else from its `include_root`. A git dependency may
not declare path dependencies. A library without a manifest has no
dependencies.

`add` and `remove` edit the file line by line, keeping comments, order
and the other tables as they are.

### The lockfile: `scad.lock`

```toml
# scad.lock: the exact revisions of scad.toml's dependencies, written by
# a fetch (scad-deps, NeoSCAD). Commit it with scad.toml; do not edit it.
version = 1
inputs = "sha256:274541f0…"

[[package]]
name = "BOSL2"
source = "git+https://github.com/BelfrySCAD/BOSL2"
tag = "v2.0.716"
commit = "9948313433…"
hash = "sha256:f135f3e4…"
dependencies = []

[[package]]
name = "parts"
source = "path+../shared-parts"
dependencies = []
```

The file is written byte for byte as follows, so the same graph always
gives the same bytes whichever tool wrote it:

1. the two comment lines above, then `version = 1` and `inputs = "…"`;
2. for each selected package, sorted by `name` (byte order): an empty
   line, `[[package]]`, then, one per line and in this order:
   - `name`: the key;
   - `folder`, only when it differs from the name;
   - `source`: `"git+URL"` (the URL as written in the manifest) or
     `"path+DIR"`, where DIR is relative to the project root,
     `/`-separated (`.` for the root itself). It is absolute only when
     no relative path exists, as with another drive;
   - for git: one of `tag`, `rev` or `branch` as requested (none for
     the default branch), then `include_root` if one is given, then
     `commit` (the full commit id) and `hash` (the content hash below);
   - for path: `include_root` if one is given;
   - `dependencies`: the keys this package's own manifest requires, as
     an inline array (`["A", "B"]`, `, `-separated).

Strings are TOML basic strings. `"` and `\` are escaped, as are `\n`,
`\t` and `\r`, and other control characters are written as `\uXXXX`. The
file uses `\n` line endings and ends with a newline. It holds no path
specific to one machine.

A reader rejects a `version` it does not know, a git package without
both `commit` and `hash`, and a package with more than one of `tag`,
`rev` and `branch`.

**`inputs`** is a hash of every requirement the project and its path
dependencies declare (directly or through other path dependencies;
git dependencies' manifests are not included). For each requirement,
form the line

    requirer · NUL · key · NUL · folder · NUL · include_root · NUL · source

where `requirer` is empty for the project itself, or the key of the path
dependency whose manifest declares the requirement. `include_root` is
the normalised value, or empty. `source` is `git+URL ` followed by the
reference as the lockfile writes it (`tag = "v1"`, `rev = "abc1"`,
`branch = "main"`, or nothing for the default branch; the space after
the URL is always there), or `path+DIR`, with DIR relative to the
project root as above. Sort the distinct lines by their bytes and join
them with `\n`. `inputs` is `sha256:` followed by the lowercase hex
SHA-256 of the result. While `inputs` matches the manifests, a fetch
links the locked packages and resolves nothing.

### The content hash

Recorded as `hash` and checked against the checkout every time a project
is linked to it. A mismatch is reported with the checkout's path and both
hashes, and the checkout is never silently replaced. It is written
`sha256:<64 lowercase hex digits>`:

1. list every regular file and every symbolic link under the tree's root,
   recursively, without following links (an empty directory contributes
   nothing, as git does not track it);
2. name each entry by its path relative to the root, with components
   joined by `/`, as UTF-8, and sort the entries by those bytes;
3. SHA-256 the concatenation, over the entries in order, of
   `path bytes · 0x00 · content length as 8 bytes big-endian · content`,
   where a file's content is its bytes and a link's content is its target
   as written (`/`-separated).

File modes, times and owners are not part of the hash. A link hashes
like a file holding its target, so a Windows checkout without symbolic
links hashes the same as a Unix one.

### Where includes are found

For a file inside a project (the nearest `scad.toml` at or above the
file's directory), the search order is:

1. the including file's own directory (OpenSCAD's rule, unchanged);
2. `<project>/.scad/libraries`;
3. the project's `[paths] libraries`, in order;
4. the host's own library path. For OpenSCAD that is `OPENSCADPATH`,
   the user library folder and the bundled libraries.

A file outside any project resolves exactly as it does in OpenSCAD.
`fetch --print-path` prints entries 2 and 3, joined by the platform's
`OPENSCADPATH` separator (`:`, or `;` on Windows). That is how stock
OpenSCAD gets the same order.

### Resolution

Resolution runs breadth first from the project. Every requirement becomes
a node: a git requirement is resolved to a commit in its mirror, and a
path requirement to its directory. Then each key gets exactly one node:

- **Minimal version selection** (as in Go modules): when every
  requirement of a key is a tag that reads as a version, the highest one
  wins. A version is an optional `v` or `V`, one to three dot-separated
  numbers (missing ones count as 0), an optional `-pre-release` and an
  optional `+build`, which is ignored. Numbers compare numerically. A
  pre-release sorts before its release, and two pre-releases compare as
  strings.
- Otherwise all requirements of the key must name the same commit, or
  resolution fails with an error that names two requirers and what each
  one wants.
- Every requirement of a key must also agree on the source (one URL
  after normalising, or one directory), on `include_root` and on the
  folder name.

The result is pruned to what the project reaches through the selected
nodes. Every package is linked flat into `.scad/libraries`: SCAD's
include namespace is flat, so one copy per name is the only workable
policy.

A requirement that matches a lockfile entry exactly (key, URL, revision,
`include_root`) keeps the locked commit. Editing one dependency never
moves another, and a fetch never moves a branch by itself; `update`
resolves the named keys (or all of them) afresh. A tag already in the
mirror is taken as it is (tags are meant to be immutable; `update`
notices a moved one).

### The store

There is one store per machine, shared by every project and every tool
that implements this format. It lives in `--store DIR`, else in
`SCAD_STORE`, else in `NEOSCAD_STORE` (the variable's first name, kept as
an alias), else in the platform's cache directory:
`~/Library/Caches/NeoSCAD/store` on macOS, `$XDG_CACHE_HOME/neoscad/store`
(by default `~/.cache/neoscad/store`) on Linux, and
`%LOCALAPPDATA%\NeoSCAD\store` on Windows. `SCAD_GIT`, else its alias
`NEOSCAD_GIT`, names the git executable (default: `git` on the `PATH`).
A variable set to the empty string counts as unset.

The default folder keeps NeoSCAD's name although the format is
tool-neutral: it is where the first implementation put the store, and
renaming it would leave every existing store behind, to be fetched
again. A tool that implements this format uses the same folder, so one
store serves all of them.

```text
<store>/
├── git/<label>-<url hash>.git                 bare mirror, one per remote
├── checkouts/<label>-<commit>[-<root hash>]/  one read-only tree per commit, no .git
├── projects/<root hash>.toml                  which checkouts and mirrors a project links
├── tmp/                                       checkouts and clones being made
├── lock                                       held while the store changes
└── empty-gitconfig                            the git configuration checkouts see
```

- A URL is **normalised** by trimming whitespace, then trailing `/`s,
  then one `.git` suffix, then lower-casing the scheme and host
  (`scheme://host`, or the `user@host` of an scp-like `user@host:path`).
  `<label>` is the last path component of the normalised URL, with
  every character outside `[A-Za-z0-9._-]` replaced by `_`, cut to 40
  characters and stripped of leading dots (`repo` if nothing is left).
  `<url hash>` is the first 16 hex digits of the SHA-256 of the
  normalised URL.
- A **mirror** is made by `git clone --bare`. Later revisions cost a
  `git fetch` of every branch and tag (moved tags forced, deleted ones
  pruned). Each mirror is fetched at most once per run, and only when it
  is needed.
- A **checkout** is named by the label, the first 12 digits of the
  commit, and (when `include_root` is given) `-` plus the first 8 hex
  digits of the SHA-256 of the normalised `include_root`. It is made
  from the mirror's objects with a throwaway index (`git read-tree` and
  `git checkout-index`, with `GIT_INDEX_FILE` in `tmp/`), so the mirror
  is not changed and the tree has no `.git`. Git's user and system
  configuration is turned off (`GIT_CONFIG_GLOBAL` is the empty file,
  `GIT_CONFIG_NOSYSTEM=1`) and `core.autocrlf=false` and `core.eol=lf`
  are set, so the bytes and the content hash are the same on every
  machine. Submodules and LFS objects are not fetched. The tree is
  written in `tmp/`, hashed, renamed into place and made read-only
  (files, and on Unix directories too).
- Every change to the store holds an OS file lock on `lock`, so
  concurrent fetches take turns, and the second finds the first's work.
  A checkout appears in one rename, so a reader never sees half of one.
- **The index**: each fetch rewrites `projects/<first 16 hex digits of
  the SHA-256 of the project root's absolute path>.toml`, which holds
  `root = "…"`, `checkouts = […]` and `mirrors = […]` (directory names).
  `store gc` drops the entries of projects whose `scad.toml` is gone,
  then removes every checkout and mirror that no remaining project
  lists, and anything left in `tmp/`.

### Links

`.scad/libraries/<Folder>` is a link to the package's checkout (or, for
a path dependency, to its directory), joined with its `include_root`:

- on Unix, a symbolic link;
- on Windows, a directory symbolic link where the user may make one
  (Developer Mode); otherwise a directory junction, which needs no
  privilege but only works on local volumes; otherwise a copy.

A fetch changes only links that point somewhere else, and removes
entries that are no longer dependencies. It writes `.scad/.gitignore`
containing `*` if the file is missing. `vendor` copies the same trees as
plain, writable files (no links into the store) into a folder outside
`.scad/`, leaving the folder's other entries alone.

### Security

A fetch never runs code from a dependency: there are no build scripts,
and `checkout-index` runs no git hooks.

## License

MIT (see `LICENSE`). The rest of NeoSCAD is GPL-2.0-or-later; this crate
depends on nothing under the GPL, so it can be used, and moved to its
own repository, under the MIT licence alone.
