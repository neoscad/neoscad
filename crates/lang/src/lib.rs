//! The OpenSCAD language front end.
//!
//! Layers, bottom up:
//!
//! - [`syntax`]: a byte-based lexer, a recursive-descent parser with error
//!   recovery and a lossless concrete syntax tree (every byte, comments
//!   included), for the formatter and the LSP;
//! - [`loader`]: `include` splicing and `use` resolution over a pluggable
//!   [`loader::FileSystem`] ([`vfs`] has in-memory ones);
//! - [`ast`]: the typed AST the evaluator consumes, lowered from the tree
//!   with OpenSCAD's semantics (scopes, reassignment, literal folding);
//! - [`deps`]: parsing `use`d libraries, as OpenSCAD does before running;
//! - [`customizer`]: parameter annotations and parameter sets;
//! - [`dump`]: the `.ast` export;
//! - [`diag`]: diagnostics shared by all later crates.
//!
//! [`parse_program`] runs the whole pipeline the way OpenSCAD's command line
//! does.

pub mod ast;
pub mod customizer;
pub mod deps;
pub mod diag;
pub mod dump;
pub mod loader;
pub mod number;
pub mod source;
pub mod syntax;
pub mod vfs;

use std::path::{Path, PathBuf};

use crate::ast::Ast;
use crate::diag::{DiagCode, Diagnostic, Severity};
use crate::loader::{FileSystem, LibraryPath, UseRef, seq_for_token};
use crate::source::{FileId, SourceMap, Span};
use crate::syntax::Cst;

/// A parsed program: sources, syntax tree, AST and diagnostics.
#[derive(Debug)]
pub struct Program {
    pub sources: SourceMap,
    pub main: FileId,
    pub cst: Cst,
    pub ast: Ast,
    pub uses: Vec<UseRef>,
    /// All diagnostics, in the order OpenSCAD would emit them.
    pub diags: Vec<Diagnostic>,
}

impl Program {
    /// Whether the program has a syntax (or lexical) error, which makes
    /// OpenSCAD refuse to run it.
    pub fn has_syntax_errors(&self) -> bool {
        self.diags.iter().any(|d| d.code == DiagCode::SyntaxError)
    }

    /// The diagnostics OpenSCAD itself would print: it stops at the first
    /// syntax error, so everything after it is left out.
    pub fn openscad_diags(&self) -> impl Iterator<Item = &Diagnostic> {
        let stop = self
            .diags
            .iter()
            .position(|d| d.code == DiagCode::SyntaxError)
            .map_or(usize::MAX, |i| i + 1);
        self.diags.iter().take(stop)
    }
}

/// Parse `text` as the file `path` (absolute), following includes. The
/// text is used as given: OpenSCAD's command line appends `"\n\x03\n"` and
/// the `-D` assignments before parsing, and callers that want the same
/// behaviour append them too.
pub fn parse_program(
    path: PathBuf,
    text: Vec<u8>,
    fs: &dyn FileSystem,
    libs: &LibraryPath,
) -> Program {
    let main = path.clone();
    finish(loader::load(path, text, fs, libs), &main, true)
}

/// Parse a `use`d library the way `SourceFileCache` does: like a program,
/// but reassignment warnings treat `main` (the using program's file) as the
/// main file, and no customizer annotations are collected.
pub fn parse_library(
    path: PathBuf,
    text: Vec<u8>,
    main: &Path,
    fs: &dyn FileSystem,
    libs: &LibraryPath,
) -> Program {
    finish(loader::load(path, text, fs, libs), main, false)
}

/// Parse one file without following includes (for editors and formatters).
pub fn parse_file(path: PathBuf, text: Vec<u8>) -> Program {
    let main = path.clone();
    finish(loader::load_single(path, text), &main, false)
}

fn finish(loaded: loader::Loaded, main_path: &Path, annotate: bool) -> Program {
    let loader::Loaded {
        sources,
        tokens,
        mut diags,
        uses,
        ..
    } = loaded;
    let main = FileId(0);
    let parse = syntax::parse(tokens);
    let toks = parse.cst.tokens();
    for e in &parse.errors {
        // Bison reports the scanner's line counter, which has already moved
        // past the offending token: the line where that token *ends*.
        let (span, line) = match toks.get(e.token as usize) {
            Some(t) => (
                Span::new(t.file, t.start, t.end()),
                sources.get(t.file).line_of(t.end()),
            ),
            None => {
                let n = sources.get(main).text.len() as u32;
                (Span::new(main, n, n), sources.get(main).line_of(n))
            }
        };
        diags.push(
            Diagnostic::new(
                DiagCode::SyntaxError,
                Severity::Error,
                "Parser error: syntax error",
            )
            .at(span, line)
            .with_seq(seq_for_token(e.token) + 1),
        );
    }
    let (mut ast, lower_diags) = ast::lower(&parse.cst, &sources, main_path, &uses);
    diags.extend(lower_diags);
    diags.sort_by_key(|d| d.seq);
    if annotate {
        let main_path = sources.path(main).to_path_buf();
        customizer::collect_parameters(&mut ast, &sources.get(main).text, |f| {
            sources.path(f) == main_path
        });
    }
    Program {
        sources,
        main,
        cst: parse.cst,
        ast,
        uses,
        diags,
    }
}
