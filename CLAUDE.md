# NeoSCAD

A ground-up reimplementation of OpenSCAD (language, features, test suite)
on a modern stack. macOS client first, WebAssembly web build close behind.
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
  `Options`. Only `crates/cli`, `crates/conformance` and test code may use
  them directly. This is what keeps the WASM build honest.
- Output must be byte-identical at any thread count; add a determinism test
  for anything parallel.
- `assets/` is vendored upstream content (Liberation fonts, MCAD); see
  `assets/README.md` for sources and the update procedure.
- `vendor/manifold-rust` carries a local patch; see `vendor/README.md`.

## Working with agents

- Work is delegated to the `builder` (diffs) and `auditor` (documents)
  agents in `.claude/agents`, one at a time. Agents never commit; the
  manager session reviews and commits each logical step.
- Deferred issues go in `docs/followups.md`.
- Large files: follow the `shunt` skill (`.claude/skills/shunt`). The hooks
  in `.claude/hooks` block whole reads of files over 350 lines.
