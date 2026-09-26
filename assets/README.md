# Bundled assets

Files OpenSCAD ships in its resources directory, which neoscad compiles
into its binaries (the `neoscad-assets` crate, `crates/assets`), so that
the default font and `include <MCAD/...>` work with nothing installed and
in the WASM build, which has no file system.

| Directory | What | Source | Licence |
|---|---|---|---|
| `fonts/Liberation-2.00.1/` | Liberation Sans, Serif and Mono 2.00.1 (12 TrueType files). Liberation Sans Regular is OpenSCAD's default font. | OpenSCAD's `fonts/Liberation-2.00.1`, commit `28fe66bc` of `openscad/openscad` | SIL Open Font License 1.1, `fonts/Liberation-2.00.1/LICENSE` |
| `libraries/MCAD/` | The MCAD library | OpenSCAD's `libraries/MCAD` submodule, `openscad/MCAD` commit `1ea40220` | GNU LGPL 2.1 (some files also allow more permissive terms, as their comments say), `libraries/MCAD/lgpl-2.1.txt` |

Both are copied unchanged, except that MCAD's `.git` submodule pointer and
`.gitignore` are left out. The fonts are byte-identical to the ones the
conformance suite's expected outputs were made with; a test in
`crates/assets` checks them (and MCAD) against the reference checkout.

## How they are used

- **Fonts**: the command line adds them first, where OpenSCAD's fontconfig
  setup adds `<resources>/fonts`; `NEOSCAD_FONT_DIR` replaces them with a
  directory.
- **MCAD**: mounted in memory (`lang::vfs::Overlay`) at `libraries/` next to
  the executable and appended to the library path after `OPENSCADPATH` and
  the user library directory, the order `parser_init()` in OpenSCAD's
  `parsersettings.cc` uses for `<resources>/libraries`. Nothing is written
  to disk; a real `libraries/` directory at that place still shows through
  for files the overlay lacks.

The `bundled-assets` feature of `neoscad-cli` (on by default) controls
the embedding. The assets add about 4.3 MB to the binary.

## Updating

Copy the directories again from the reference checkout (see the root
`CLAUDE.md`), drop MCAD's `.git` and `.gitignore`, update the commits
above, and run `cargo test -p neoscad-assets`.
