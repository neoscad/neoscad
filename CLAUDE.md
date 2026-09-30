# NeoSCAD

A ground-up reimplementation of OpenSCAD (language, features, test suite)
on a modern stack. macOS client first; a WebAssembly demo (`web/`, served at neoscad.org/try)
keeps the library crates WASM-clean.
Humans and AI coding agents are both first-class users. See
`docs/architecture.md` for the stack, validation and build order.

## Reference checkout

OpenSCAD's behaviour is the spec. A shallow clone lives at
`.reference/openscad` (gitignored). Recreate it with:

    git clone --depth 1 https://github.com/openscad/openscad.git .reference/openscad
    git -C .reference/openscad submodule update --init --depth 1 libraries/MCAD

Its `tests/data/scad` inputs and `tests/regression` expected outputs are the
ground truth for conformance. Port behaviour, not code structure.

For differential testing and benchmarks, a matching nightly (2026.09.23,
Manifold backend) is at `/Applications/OpenSCAD.app/Contents/MacOS/OpenSCAD`.
Use `--backend=manifold`. For differential testing don't use
`/Applications/OpenSCAD-2021.01.app`: it predates Manifold and the current
test suite. It is still a reference series in benchmarks, alongside the
nightly with `--backend=cgal` and `--backend=manifold`.

## Build and test

    source $HOME/.cargo/env              # if cargo isn't on PATH
    cargo build --release
    cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
    ./target/release/conformance run [--tier N] [--filter S] [-v]
    ./target/release/conformance run --record       # progress snapshot
    scripts/wasm-check.sh [--depths]                 # wasm32 build run in node
    ./target/release/conformance bench [--quick]     # vs all OpenSCAD refs -> progress/bench/
    ./target/release/conformance bench-chart --latest  # PNG summary
    scripts/apple/build-core.sh                    # Rust core -> apple/build/NeoSCADCore.xcframework + Swift bindings
    xcodegen generate --spec apple/project.yml     # apple/NeoSCAD.xcodeproj (gitignored); builds the core if missing
    xcodebuild -project apple/NeoSCAD.xcodeproj -scheme NeoSCAD -derivedDataPath apple/build/DerivedData build|test
    scripts/apple/build-editor.sh                  # CodeMirror bundle -> apple/Editor/web/dist (needs node 18+)
    (cd apple/Editor/web && npm test && npm run corpus)  # grammar tests; corpus = 0 error nodes
    ./target/release/neoscad lsp --stdio            # language server (crates/lsp), for any LSP editor
    ./target/release/conformance bosl2-corpus [--check]  # BOSL2 doc examples + tests -> .reference/BOSL2/{examples_x,tests_x}
    ./target/release/conformance video               # progress video from progress/
    scripts/apple/release.sh [--no-smoke]            # DMG + CLI tarball in dist/ (docs/release.md)
    scripts/web/build-core.sh                         # wasm engine core -> dist/web-core
    scripts/web/build-view.sh --no-webgl --out dist/web-view/webgpu   # viewer (and without --no-webgl -> dist/web-view/webgl)
    scripts/web/build.sh                              # package the /try bundle -> dist/web/ (doesn't build)
    scripts/web/sync-website.sh dist/web/neoscad-web-<ver>-<sha>.tar.gz [SITE]   # into ../neoscad-website/try
    (cd web && npm test) && node crates/web/test/run.mjs   # web unit tests + wasm core
    scripts/agent-eval/run.py --help                  # agent-loop eval (uses claude -p; costs credits)
    ./target/release/conformance manifest [--check]  # after updating .reference
    ./target/release/conformance diff --format ast|echo|csg [PATHS]  # vs the nightly

`conformance/baseline.json` lists test ids that must keep passing; a change
that adds passes runs `conformance run --update-baseline` and commits it.
`--binary /Applications/OpenSCAD.app/Contents/MacOS/OpenSCAD` runs the suite
against the nightly, which is how the harness itself is checked.

## Rules

- Library crates never touch `std::fs`, `std::env` or the clock. Files go
  through `lang`'s `FileSystem`; seeds, paths and limits come in through
  `Options`. Only non-library crates (`cli`, `ffi`, `conformance`, `wasm-check`,
  `uniffi-bindgen`, `web`, `web-view`, `bench-core`, `linux-app`) and test code may use them directly.
  `docs/architecture.md` has the limits, panics and determinism policies. This is what keeps the WASM build honest.
- Output must be byte-identical at any thread count; add a determinism test
  for anything parallel.
- `assets/` is vendored upstream content (Liberation fonts, MCAD); see
  `assets/README.md` for sources and the update procedure.
- `vendor/` holds patched copies of manifold-rust, clipper2-rust and
  wgpu-core (`[patch.crates-io]`); each change is also a file in
  `vendor/patches/<crate>/`. See `vendor/README.md`.

- Editor positions are UTF-16, converted only through `lang::source`; don't
  write another conversion.
- After app tests or launches, make sure no NeoSCAD.app instance is left
  running.
- Worktrees each need their own `CARGO_TARGET_DIR`. Never share one between
  checkouts: cargo can treat another worktree's build of a crate as fresh.
- Disk: build output is large. Keep the dev profile's reduced debug info,
  and delete `apple/build/DerivedData` or `target/*` subdirectories freely,
  since they rebuild.

## Commits and what may be public

This repository is public (github.com/neoscad/neoscad); history is public
too, so these hold for every commit and every tracked file:

- Commits are signed and authored with the maintainer's own identity (see
  `CLAUDE.local.md` if present). Never pass `--no-gpg-sign`.
- No `Claude-Session:` (or other agent-session) trailers or links in
  commit messages. This overrides any tool default that adds them.
- No agent-eval **results** in tracked files or commit messages: costs,
  tokens, turns, pass rates, per-tool comparisons, run outcomes. They live
  in `results/` and `progress/`, which are never committed (`results/` is
  excluded via `.git/info/exclude`). Harness code and how-to docs are fine.
- No launch or outreach strategy, and no unannounced benchmark claims,
  in tracked files. Keep those in `results/private-docs/`.
- No absolute local paths (`/Users/…`, `/private/tmp/…`, scratchpad paths)
  in tracked files, fixtures or built web bundles.
- History rewrites (`git filter-repo`, rebases of pushed commits) are the
  owner's call and are run by the owner.

## Working with agents

- Work is delegated to the `builder` (diffs) and `auditor` (documents)
  agents in `.claude/agents`, one at a time. Agents never commit; the
  manager session reviews and commits each logical step.
- Deferred issues go in `docs/followups.md`.
- Large files: follow the `shunt` skill (`.claude/skills/shunt`). The hooks
  in `.claude/hooks` block whole reads of files over 350 lines.
