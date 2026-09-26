//! Diagnostics shared by every NeoSCAD crate.
//!
//! A diagnostic carries two views of the same problem:
//!
//! - the **OpenSCAD view**: message text and a line number exactly as
//!   OpenSCAD prints them (`WARNING: ... in file x.scad, line 3`). The
//!   conformance suite compares that text word for word, so the message is
//!   stored verbatim and [`Diagnostic::render_openscad`] reproduces the
//!   surrounding format. The line is stored rather than derived from the span
//!   because OpenSCAD's notion of "the line" differs by message (a syntax
//!   error reports the line where the offending token *ends*, a lexer warning
//!   the line where the escape sequence sits);
//! - the **tool view**: a stable [`DiagCode`], a precise byte [`Span`] and
//!   optional fix [`Hint`]s, for `--format json`, the LSP and agents.
//!
//! This lives in `lang`, not in a separate crate, because every diagnostic
//! needs `Span` and `SourceMap` to be rendered and every later crate already
//! depends on `lang`; a `diag` crate would only move those two types.

use std::fmt;
use std::path::{Component, Path, PathBuf};

use crate::source::{SourceMap, Span};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    Error,
    Warning,
    Deprecated,
    /// Output of `echo()`: not a problem, but it travels the same channel
    /// and OpenSCAD interleaves it with warnings in one stream.
    Echo,
    /// A call-stack line printed after an evaluation error.
    Trace,
}

impl Severity {
    /// The prefix OpenSCAD prints (`getGroupName` in utils/printutils.cc).
    pub fn openscad_label(self) -> &'static str {
        match self {
            Severity::Error => "ERROR",
            Severity::Warning => "WARNING",
            Severity::Deprecated => "DEPRECATED",
            Severity::Echo => "ECHO",
            Severity::Trace => "TRACE",
        }
    }
}

/// Stable identifiers. The string form is part of the public JSON output, so
/// variants may be added but never renamed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DiagCode {
    /// The parser met a token the grammar does not allow.
    SyntaxError,
    UnterminatedString,
    UnterminatedComment,
    UnterminatedInclude,
    UnterminatedUse,
    /// A non-ASCII identifier; OpenSCAD gates these behind an experimental
    /// feature.
    NonAsciiIdentifier,
    UndefinedEscape,
    /// An integer literal that a double cannot hold exactly.
    ImpreciseInteger,
    HexTooLarge,
    /// `2d = ...`: identifiers starting with a digit are deprecated.
    DigitIdentifier,
    NewlineInInclude,
    NewlineInUse,
    IncludeNotFound,
    LibraryNotFound,
    /// A variable assigned twice in one scope; the later value wins.
    Reassignment,
    /// A customizer parameter value outside its declared range.
    ParameterRange,
    /// A parameter-set file that cannot be read or parsed.
    ParameterFile,
    /// `echo()` output.
    Echo,
    /// A call-stack line after an evaluation error.
    Trace,
    UnknownVariable,
    UnknownFunction,
    UnknownModule,
    /// An operator applied to operands it is not defined for; the result is
    /// `undef`.
    UndefinedOperation,
    /// Arguments that do not fit the parameters (count, names, duplicates).
    ArgumentMismatch,
    /// A builtin received a value of the wrong type or out of range.
    InvalidArgument,
    AssertionFailed,
    /// Recursion or stack exhaustion.
    RecursionLimit,
    /// A loop or range that would run too many iterations.
    IterationLimit,
    /// A variable reassigned in a way OpenSCAD warns about at run time.
    Overwrite,
    ExperimentalFeature,
    /// Any other evaluation-time message.
    Evaluation,
    /// A message from building geometry (render time): mixed dimensions,
    /// invalid transforms, meshes that are not manifold.
    Geometry,
}

impl DiagCode {
    pub fn as_str(self) -> &'static str {
        match self {
            DiagCode::SyntaxError => "syntax-error",
            DiagCode::UnterminatedString => "unterminated-string",
            DiagCode::UnterminatedComment => "unterminated-comment",
            DiagCode::UnterminatedInclude => "unterminated-include",
            DiagCode::UnterminatedUse => "unterminated-use",
            DiagCode::NonAsciiIdentifier => "non-ascii-identifier",
            DiagCode::UndefinedEscape => "undefined-escape",
            DiagCode::ImpreciseInteger => "imprecise-integer",
            DiagCode::HexTooLarge => "hex-too-large",
            DiagCode::DigitIdentifier => "digit-identifier",
            DiagCode::NewlineInInclude => "newline-in-include",
            DiagCode::NewlineInUse => "newline-in-use",
            DiagCode::IncludeNotFound => "include-not-found",
            DiagCode::LibraryNotFound => "library-not-found",
            DiagCode::Reassignment => "reassignment",
            DiagCode::ParameterRange => "parameter-range",
            DiagCode::ParameterFile => "parameter-file",
            DiagCode::Echo => "echo",
            DiagCode::Trace => "trace",
            DiagCode::UnknownVariable => "unknown-variable",
            DiagCode::UnknownFunction => "unknown-function",
            DiagCode::UnknownModule => "unknown-module",
            DiagCode::UndefinedOperation => "undefined-operation",
            DiagCode::ArgumentMismatch => "argument-mismatch",
            DiagCode::InvalidArgument => "invalid-argument",
            DiagCode::AssertionFailed => "assertion-failed",
            DiagCode::RecursionLimit => "recursion-limit",
            DiagCode::IterationLimit => "iteration-limit",
            DiagCode::Overwrite => "overwrite",
            DiagCode::ExperimentalFeature => "experimental-feature",
            DiagCode::Evaluation => "evaluation",
            DiagCode::Geometry => "geometry",
        }
    }
}

impl fmt::Display for DiagCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A suggested fix: what to do, and optionally the exact edit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hint {
    pub message: String,
    pub replacement: Option<(Span, String)>,
}

/// Which directory OpenSCAD makes a message's file path relative to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PathBase {
    /// The working directory (lexer and parser messages).
    #[default]
    WorkingDir,
    /// The main file's directory (evaluation and reassignment messages).
    MainFileDir,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Diagnostic {
    pub code: DiagCode,
    pub severity: Severity,
    /// OpenSCAD's message text, without the severity prefix or location.
    pub message: String,
    /// Precise location, when there is one.
    pub span: Option<Span>,
    /// The 1-based line OpenSCAD reports (in `span.file`); 0 with no span.
    pub line: u32,
    pub base: PathBase,
    pub hints: Vec<Hint>,
    /// Emission order. OpenSCAD reports lexer, parser and reassignment
    /// messages in the order the scanner reaches them and stops at the first
    /// syntax error; sorting by this key reproduces that interleaving.
    pub seq: u64,
}

impl Diagnostic {
    pub fn new(code: DiagCode, severity: Severity, message: impl Into<String>) -> Self {
        Self {
            code,
            severity,
            message: message.into(),
            span: None,
            line: 0,
            base: PathBase::WorkingDir,
            hints: Vec::new(),
            seq: 0,
        }
    }

    pub fn at(mut self, span: Span, line: u32) -> Self {
        self.span = Some(span);
        self.line = line;
        self
    }

    pub fn with_seq(mut self, seq: u64) -> Self {
        self.seq = seq;
        self
    }

    pub fn with_base(mut self, base: PathBase) -> Self {
        self.base = base;
        self
    }

    pub fn with_hint(mut self, message: impl Into<String>) -> Self {
        self.hints.push(Hint { message: message.into(), replacement: None });
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    /// The line OpenSCAD prints: `ERROR: <message> in file <path>, line <n>`.
    /// `cwd` and `main_dir` resolve [`PathBase`].
    pub fn render_openscad(&self, sources: &SourceMap, cwd: &Path, main_dir: &Path) -> String {
        let mut s = format!("{}: {}", self.severity.openscad_label(), self.message);
        if let Some(span) = self.span {
            let base = match self.base {
                PathBase::WorkingDir => cwd,
                PathBase::MainFileDir => main_dir,
            };
            let rel = relative_path(sources.path(span.file), base);
            s.push_str(&format!(" in file {}, line {}", rel.display(), self.line));
        }
        s
    }
}

/// `std::filesystem::relative(path, base)`: both sides are made canonical
/// where they exist (resolving symlinks such as macOS `/tmp`), then the
/// shortest `..`-path from `base` to `path` is returned.
pub fn relative_path(path: &Path, base: &Path) -> PathBuf {
    let p = weakly_canonical(path);
    let b = weakly_canonical(base);
    let pc: Vec<Component> = p.components().collect();
    let bc: Vec<Component> = b.components().collect();
    let common = pc.iter().zip(&bc).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..bc.len() {
        out.push("..");
    }
    for c in &pc[common..] {
        out.push(c.as_os_str());
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// Canonicalise the longest existing prefix and append the rest lexically.
fn weakly_canonical(path: &Path) -> PathBuf {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map(|d| d.join(path)).unwrap_or_else(|_| path.to_path_buf())
    };
    let mut existing = abs.clone();
    let mut rest = Vec::new();
    loop {
        if let Ok(c) = existing.canonicalize() {
            let mut out = c;
            for r in rest.iter().rev() {
                out.push(r);
            }
            return normalize_lexically(&out);
        }
        match (existing.file_name().map(|f| f.to_os_string()), existing.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent.to_path_buf();
            }
            _ => return normalize_lexically(&abs),
        }
    }
}

fn normalize_lexically(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths_like_std_filesystem() {
        assert_eq!(relative_path(Path::new("/nonexist/a/b.scad"), Path::new("/nonexist/a")), Path::new("b.scad"));
        assert_eq!(
            relative_path(Path::new("/nonexist/t/data/x.scad"), Path::new("/nonexist/b/t")),
            Path::new("../../t/data/x.scad")
        );
        assert_eq!(relative_path(Path::new("/nonexist/a"), Path::new("/nonexist/a")), Path::new("."));
    }

    #[test]
    fn renders_openscad_format() {
        let mut sm = SourceMap::new();
        let f = sm.add("/nonexist/d/e.scad".into(), b"x".to_vec());
        let d = Diagnostic::new(DiagCode::SyntaxError, Severity::Error, "Parser error: syntax error")
            .at(Span::new(f, 0, 1), 3);
        assert_eq!(
            d.render_openscad(&sm, Path::new("/nonexist/d"), Path::new("/")),
            "ERROR: Parser error: syntax error in file e.scad, line 3"
        );
        let d = Diagnostic::new(DiagCode::SyntaxError, Severity::Warning, "plain");
        assert_eq!(d.render_openscad(&sm, Path::new("/"), Path::new("/")), "WARNING: plain");
    }
}
