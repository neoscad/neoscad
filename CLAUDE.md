# NeoSCAD

A ground-up reimplementation of OpenSCAD (language, features, test suite)
on a modern stack. macOS client first, WebAssembly web build close behind.
The architecture is still being decided; this file grows as it is.

## Reference checkout

OpenSCAD's behaviour is the spec. A shallow clone lives at
`.reference/openscad` (gitignored). Recreate it with:

    git clone --depth 1 https://github.com/openscad/openscad.git .reference/openscad

Its `tests/data/scad` inputs and `tests/regression` expected outputs are the
ground truth for conformance. Port behaviour, not code structure.

## Working with agents

- Work is delegated to the `builder` (diffs) and `auditor` (documents)
  agents in `.claude/agents`, one at a time. Agents never commit; the
  manager session reviews and commits each logical step.
- Large files: follow the `shunt` skill (`.claude/skills/shunt`). The hooks
  in `.claude/hooks` block whole reads of files over 350 lines.
