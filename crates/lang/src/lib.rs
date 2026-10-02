//! The OpenSCAD language front end.
//!
//! Layers, bottom up:
//!
//! - [`syntax`]: a byte-based lexer, a recursive-descent parser with error
//!   recovery and a lossless concrete syntax tree (every byte, comments
//!   included), for the formatter and the LSP;
//! - [`loader`]: `include` splicing and `use` resolution over a pluggable
//!   [`loader::FileSystem`] ([`vfs`] has in-memory ones);
//! - [`fragment`]: included files parsed once and put into each program
//!   that includes them as their parse, for hosts that parse again after
//!   every edit;
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
pub mod fragment;
pub mod loader;
pub mod number;
pub mod paths;
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
    parse_program_cached(path, text, fs, libs, None)
}

/// [`parse_program`], reading and lexing included files through `cache`
/// ([`loader::LexCache`]). The program is the same.
pub fn parse_program_cached(
    path: PathBuf,
    text: Vec<u8>,
    fs: &dyn FileSystem,
    libs: &LibraryPath,
    cache: Option<&dyn loader::LexCache>,
) -> Program {
    parse_program_with(
        path,
        text,
        fs,
        libs,
        Caches {
            lex: cache,
            ..Caches::default()
        },
    )
}

/// What a parse may reuse from earlier ones. With none it reads, lexes
/// and parses everything; the program is the same either way.
#[derive(Clone, Copy, Default)]
pub struct Caches<'a> {
    /// Included files read and lexed.
    pub lex: Option<&'a dyn loader::LexCache>,
    /// Included files parsed and lowered ([`fragment`]).
    pub fragments: Option<&'a dyn fragment::FragmentCache>,
    /// Where to add how includes were put in.
    pub stats: Option<&'a std::cell::Cell<fragment::SpliceStats>>,
}

impl std::fmt::Debug for Caches<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Caches")
            .field("lex", &self.lex.is_some())
            .field("fragments", &self.fragments.is_some())
            .field("stats", &self.stats)
            .finish()
    }
}

/// [`parse_program`] through `caches`.
pub fn parse_program_with(
    path: PathBuf,
    text: Vec<u8>,
    fs: &dyn FileSystem,
    libs: &LibraryPath,
    caches: Caches<'_>,
) -> Program {
    let main = path.clone();
    finish(parse_with(path, text, &main, fs, libs, caches), &main, true)
}

fn parse_with(
    path: PathBuf,
    text: Vec<u8>,
    main: &Path,
    fs: &dyn FileSystem,
    libs: &LibraryPath,
    caches: Caches<'_>,
) -> fragment::Parsed {
    let stats = std::cell::Cell::default();
    let ctx = fragment::Ctx {
        fs,
        libs,
        lex: caches.lex,
        frags: caches.fragments,
        main,
        stats: caches.stats.unwrap_or(&stats),
    };
    fragment::parse(&ctx, path, text, None, Vec::new())
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
    parse_library_cached(path, text, main, fs, libs, None)
}

/// [`parse_library`] with a [`loader::LexCache`].
pub fn parse_library_cached(
    path: PathBuf,
    text: Vec<u8>,
    main: &Path,
    fs: &dyn FileSystem,
    libs: &LibraryPath,
    cache: Option<&dyn loader::LexCache>,
) -> Program {
    parse_library_with(
        path,
        text,
        main,
        fs,
        libs,
        Caches {
            lex: cache,
            ..Caches::default()
        },
    )
}

/// [`parse_library`] through `caches`.
pub fn parse_library_with(
    path: PathBuf,
    text: Vec<u8>,
    main: &Path,
    fs: &dyn FileSystem,
    libs: &LibraryPath,
    caches: Caches<'_>,
) -> Program {
    finish(parse_with(path, text, main, fs, libs, caches), main, false)
}

/// Parse one file without following includes (for editors and formatters).
pub fn parse_file(path: PathBuf, text: Vec<u8>) -> Program {
    let main = path.clone();
    finish(
        fragment::Parsed::plain(loader::load_single(path, text)),
        &main,
        false,
    )
}

/// Parse one file without following includes, with its customizer
/// annotations (`customizer::Parameters::from_ast` reads them). Only the
/// main file's top-level assignments are parameters, so an editor's
/// customizer panel need not read or parse what the file includes.
pub fn parse_file_annotated(path: PathBuf, text: Vec<u8>) -> Program {
    let main = path.clone();
    finish(
        fragment::Parsed::plain(loader::load_single(path, text)),
        &main,
        true,
    )
}

fn finish(parsed: fragment::Parsed, main_path: &Path, annotate: bool) -> Program {
    let (mut ast, lower_diags) = {
        let placed = parsed.placed();
        let (ast, diags, _) = ast::lower_with(
            &parsed.cst,
            &parsed.sources,
            main_path,
            &parsed.uses,
            &placed,
            false,
        );
        (ast, diags)
    };
    let fragment::Parsed {
        sources,
        cst,
        errors,
        mut diags,
        uses,
        ..
    } = parsed;
    let main = FileId(0);
    let toks = cst.tokens();
    for e in &errors {
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
        // Past the nesting limit OpenSCAD's Bison stack is full, and it
        // says "memory exhausted" rather than "syntax error".
        let (message, hint) = if e.too_deep {
            ("Parser error: memory exhausted", nesting_hint())
        } else {
            (
                "Parser error: syntax error",
                // The `\x03` OpenSCAD appends (and so do NeoSCAD's
                // callers) is where an unfinished program fails; read as a
                // token, the hint said ``unexpected `\u0003` at line 2``.
                syntax_hint(
                    &sources,
                    span,
                    toks.get(e.token as usize)
                        .is_none_or(|t| t.kind == syntax::SyntaxKind::Eot),
                ),
            )
        };
        diags.push(
            Diagnostic::new(DiagCode::SyntaxError, Severity::Error, message)
                .at(span, line)
                .with_seq(seq_for_token(e.token) + 1)
                .with_hint(hint),
        );
    }
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
        cst,
        ast,
        uses,
        diags,
    }
}

/// The fix hint of the nesting limit's error, which OpenSCAD's message
/// ("memory exhausted") does not explain.
fn nesting_hint() -> String {
    format!(
        "the program nests more than {} levels deep here (statements, brackets or a chain of operators): build deep structures with a recursive module or function, or a list, instead of writing out every level",
        syntax::parser::NESTING_LIMIT
    )
}

/// The fix hint of a syntax error: the offending token and where it is
/// (OpenSCAD's message names only a line, and in a one-line model that
/// says nothing), what usually causes it, and HTML-escaped brackets when
/// the line has them (an agent that sent `use &lt;x.scad&gt;` saw only
/// "syntax error").
fn syntax_hint(sources: &source::SourceMap, span: Span, at_end: bool) -> String {
    let f = sources.get(span.file);
    // At the end, the place to point at is just after the last thing
    // written, not the start of the line OpenSCAD's appended `\n\x03\n`
    // puts the end marker on: "line 2, column 1" of a one-line model sent
    // an agent looking for a second line.
    let at = if at_end {
        f.text[..span.start as usize]
            .iter()
            .rposition(|b| !b.is_ascii_whitespace())
            .map_or(0, |i| i as u32 + 1)
    } else {
        span.start
    };
    let (line, col) = f.line_col(at);
    let what = if at_end {
        "end of input".to_string()
    } else {
        let tok = &f.text[span.start as usize..(span.end as usize).min(f.text.len())];
        match std::str::from_utf8(tok) {
            Ok(t) if !t.is_empty() && t.len() <= 24 && !t.contains(['\n', '`']) => format!("`{t}`"),
            _ => "this token".to_string(),
        }
    };
    let mut hint = format!(
        "unexpected {what} at line {line}, column {col}: look just before it for a missing ';', ')', ']' or '}}', or an unbalanced bracket"
    );
    let start = f.text[..span.start as usize]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |i| i + 1);
    let end = f.text[span.start as usize..]
        .iter()
        .position(|&b| b == b'\n')
        .map_or(f.text.len(), |i| span.start as usize + i);
    let text = &f.text[start..end];
    let has = |pat: &[u8]| text.windows(pat.len()).any(|w| w == pat);
    if has(b"&lt;") || has(b"&gt;") || has(b"&amp;") {
        hint.push_str("; the line has HTML-escaped characters (&lt; &gt; &amp;): write <, > and & as they are");
    }
    hint
}
