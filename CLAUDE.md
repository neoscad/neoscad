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
Use `--backend=manifold`. Don't use `/Applications/OpenSCAD-2021.01.app`:
it predates Manifold and the current test suite.

## Build and test

    source $HOME/.cargo/env              # if cargo isn't on PATH
    cargo build --release
    cargo fmt --all && cargo clippy --all-targets -- -D warnings && cargo test
    ./target/release/conformance run [--tier N] [--filter S] [-v]
    ./target/release/conformance run --record       # progress snapshot
    ./target/release/conformance manifest [--check]  # after updating .reference
    ./target/release/conformance diff --format ast|echo|csg [PATHS]  # vs the nightly

`conformance/baseline.json` lists test ids that must keep passing; a change
that adds passes runs `conformance run --update-baseline` and commits it.
`--binary /Applications/OpenSCAD.app/Contents/MacOS/OpenSCAD` runs the suite
against the nightly, which is how the harness itself is checked.

## Working with agents

- Work is delegated to the `builder` (diffs) and `auditor` (documents)
  agents in `.claude/agents`, one at a time. Agents never commit; the
  manager session reviews and commits each logical step.
- Deferred issues go in `docs/followups.md`.
- Large files: follow the `shunt` skill (`.claude/skills/shunt`). The hooks
  in `.claude/hooks` block whole reads of files over 350 lines.
