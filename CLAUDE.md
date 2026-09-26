# NeoSCAD

A ground-up reimplementation of OpenSCAD (language, features, test suite)
on a modern stack. macOS client first, WebAssembly web build close behind.
Humans and AI coding agents are both first-class users. See
`docs/architecture.md` for the stack, validation and build order.

## Reference checkout

OpenSCAD's behaviour is the spec. A shallow clone lives at
`.reference/openscad` (gitignored). Recreate it with:

    git clone --depth 1 https://github.com/openscad/openscad.git .reference/openscad

Its `tests/data/scad` inputs and `tests/regression` expected outputs are the
ground truth for conformance. Port behaviour, not code structure.

For differential testing and benchmarks, a matching nightly (2026.09.23,
Manifold backend) is at `/Applications/OpenSCAD.app/Contents/MacOS/OpenSCAD`.
Use `--backend=manifold`. Don't use `/Applications/OpenSCAD-2021.01.app`:
it predates Manifold and the current test suite.

## Working with agents

- Work is delegated to the `builder` (diffs) and `auditor` (documents)
  agents in `.claude/agents`, one at a time. Agents never commit; the
  manager session reviews and commits each logical step.
- Large files: follow the `shunt` skill (`.claude/skills/shunt`). The hooks
  in `.claude/hooks` block whole reads of files over 350 lines.
