# The language server in other editors

`neoscad lsp --stdio` is NeoSCAD's language server for `.scad` files, the
same one the apps' editors use (`crates/lsp`). It speaks the Language
Server Protocol over stdin and stdout, so any editor with an LSP client
can run it. It offers:

- diagnostics: parse and evaluation errors and warnings, with quick fixes
  (geometry-stage warnings, which only a render prints, are not included);
- hover, completion and signature help, for builtins and your own modules,
  functions and variables, through `include` and `use`;
- go to definition and find references;
- rename, of names no included file defines or uses (it sees the
  document and what it includes, not the files that include it);
- formatting of a whole document or a range, following
  `.neoscad-fmt.toml`;
- document symbols (the outline) and folding ranges;
- with `--enable sketch`, constrained sketches (`docs/sketch.md`): the
  sketch vocabulary completed (with snippets), hovered and resolved only
  inside sketch bodies; an under-constrained sketch as an information
  marker; each hint's edit as a quick fix; "Pin drawing" as a code
  action (`refactor.rewrite`) anywhere in a sketch; on hover, an entity
  variable's solved values and a `sketch` call's state from the last
  evaluation of the same text; and go to definition on a handle's member
  (`top.start`) to the point it names;
- with `--enable fillet`, edge fillets and chamfers
  (`docs/fillet-edges.md`): inside the `edges` or `except` string of a
  `fillet_edges()` or `chamfer_edges()` call, completion of the selector
  language (the atoms where an operand goes, `and`, `or` and `exc` after
  one, nothing inside `child(` or `box(`, and the "did you mean" word
  for a slip; `@name` only with `--enable query`) and hover on the word
  under the cursor; `fillet_edges` and `chamfer_edges` offered only with
  the extension, and no selector completion under a program's own
  `module fillet_edges`. Hover on any builtin call's named argument
  (`r`, `edges`, or `scale` in `linear_extrude(scale = 2)`) shows that
  parameter's line of the reference. The rest needs the children's
  geometry, so a render: when a host hands the server a rendered run (the
  apps and the web page do), hover on the call's name adds what that run
  selected, its diagnostics sit on the text to change (the `edges`
  string for a count or an empty selection, `r` or `d` for a size that
  does not fit) with their fixes as quick fixes (the size that fits, the
  nested two-call rewrite), and "Pin count" (`refactor.rewrite`) writes
  the selection's size into `expect`. `neoscad lsp --stdio` evaluates
  without rendering, so it shows the selector's own errors but none of
  these.

It finds libraries as the command line does: beside the file, on
`OPENSCADPATH`, in the user library folder, then the bundled ones (MCAD).
Its evaluations run under the same resource limits as `neoscad serve`;
change them with `--limit NAME=VALUE` (repeatable, `off` for none), and
turn on OpenSCAD's experimental features or NeoSCAD's extensions for
them with `--enable NAME` (repeatable, as on the command line). Hover and
completion label a NeoSCAD extension's builtin ("NeoSCAD extension
(`--enable part`); not in OpenSCAD"). To
see what an editor sends and gets, add `--log FILE`, which appends every
message to FILE.

## The command

The examples below run `neoscad` from `PATH`, which the command-line
packages (Homebrew, Scoop, the `.deb` and `.rpm`, the archives) put it on.
The desktop apps carry their own copy, off `PATH`; to use it, put its
absolute path in place of `neoscad`:

| App | Path |
|---|---|
| macOS | `~/Library/Application Support/NeoSCAD/bin/neoscad` (a link the app makes and keeps pointing at its own copy at each launch) |
| Windows | `C:\Program Files\NeoSCAD\bin\neoscad.exe` |
| Linux Flatpak | the command `flatpak run --command=neoscad org.neoscad.NeoSCAD` |

Editors started from the macOS Dock or Finder may not see a shell's
`PATH` (Homebrew's `/opt/homebrew/bin` in particular); if the server does
not start, give the absolute path. Check the command by hand with
`neoscad lsp --help`.

## VS Code

There is no NeoSCAD extension. VS Code needs two: one that defines a
language for `.scad` files, and a generic LSP client to run the server
for it.

1. Install [OpenSCAD](https://marketplace.visualstudio.com/items?itemName=Antyos.openscad)
   (`Antyos.openscad`), which gives `.scad` files the language id `scad`
   and syntax highlighting.
2. Install [Simple LSP Client](https://marketplace.visualstudio.com/items?itemName=wdomitrz.simple-lsp-client)
   (`wdomitrz.simple-lsp-client`) and add to your `settings.json`:

```json
{
  "simpleLspClient.servers": {
    "neoscad": {
      "cmd": ["neoscad", "lsp", "--stdio"],
      "filetypes": ["scad"]
    }
  }
}
```

Leave out `Leathong.openscad-language-support`
(openscad-LSP's client), or disable it, so that two servers do not
answer the same files.

## Neovim

Neovim 0.11 and later configure servers with `vim.lsp.config` and need
no plugin; Neovim detects `.scad` files as the filetype `openscad`. In
your `init.lua`:

```lua
vim.lsp.config('neoscad', {
  cmd = { 'neoscad', 'lsp', '--stdio' },
  filetypes = { 'openscad' },
  root_markers = { '.git' },
})
vim.lsp.enable('neoscad')
```

nvim-lspconfig's `openscad_lsp` and `openscad_ls` configs start other
servers (`openscad-lsp`); don't enable them alongside this one.

## Helix

Helix already knows `.scad` files as the language `openscad`, with
`openscad-lsp` as its server. Point it at NeoSCAD's in your
`languages.toml` (`~/.config/helix/languages.toml`, or
`%AppData%\helix\languages.toml` on Windows):

```toml
[language-server.neoscad]
command = "neoscad"
args = ["lsp", "--stdio"]

[[language]]
name = "openscad"
language-servers = ["neoscad"]
```

`hx --health openscad` shows whether Helix finds the command.

## Emacs

Eglot is built into Emacs 29 and later. Install `scad-mode` (NonGNU ELPA
or MELPA: `M-x package-install RET scad-mode`), which opens `.scad` files
in `scad-mode`, then tell Eglot which server to run for it:

```elisp
(with-eval-after-load 'eglot
  (add-to-list 'eglot-server-programs
               '(scad-mode . ("neoscad" "lsp" "--stdio"))))
(add-hook 'scad-mode-hook #'eglot-ensure)
```

## Zed

Zed runs only the language servers its extensions provide; its settings
can change a provided server's binary but cannot add a server. Zed's
OpenSCAD extension (`openscad`) gives `.scad` files highlighting and no
language server, so NeoSCAD's cannot be used in Zed today without an
extension of its own.

## Other editors

Any LSP client that can start a server over stdio works: run
`neoscad lsp --stdio` for `.scad` files. The server ignores the
`languageId` a client sends, needs no initialization options or
workspace settings, and uses UTF-16 positions.
