//! Messages and error unwinding.
//!
//! Every message the evaluator prints (echo output, warnings, errors and
//! stack traces) is a [`Message`]: a `lang::diag::Diagnostic` for tools
//! (stable code, span, severity) plus the exact bytes OpenSCAD would print,
//! since echoed strings need not be UTF-8.
//!
//! OpenSCAD reports an evaluation error by throwing a C++ exception that
//! each enclosing call site catches, annotates with a `TRACE:` line and
//! rethrows. `Unwind` is that exception: evaluation functions return
//! `Err(Box<Unwind>)`, and the same call sites add the same trace lines.
//! The exception's trace budget is reproduced too: the first `trace_depth`
//! lines print immediately, later ones are kept in a ring of the same size
//! and printed, after an `*** Excluding N frames ***` line, when the error
//! reaches the top (`EvaluationException`'s destructor).

use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};

use lang::diag::{DiagCode, Diagnostic, PathBase, Severity, relative_path};
use lang::source::{SourceMap, Span};

/// One message, in the order OpenSCAD prints it.
#[derive(Debug)]
pub struct Message<'a> {
    /// The tool view. `diag.message` is `text` decoded lossily.
    pub diag: Diagnostic,
    /// The message text exactly as OpenSCAD prints it, without the
    /// severity prefix and location suffix.
    pub text: &'a [u8],
    /// Resolves `diag.span`; `None` when the message has no location.
    pub sources: Option<&'a SourceMap>,
}

impl Message<'_> {
    /// The line OpenSCAD prints: `WARNING: text in file x.scad, line 3`.
    /// Paths are relative to `main_dir` (the evaluation messages' base).
    pub fn render_openscad(&self, main_dir: &Path) -> Vec<u8> {
        let mut out = Vec::with_capacity(self.text.len() + 48);
        out.extend_from_slice(self.diag.severity.openscad_label().as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(self.text);
        if let (Some(span), Some(sources)) = (self.diag.span, self.sources) {
            let rel = relative_path(sources.path(span.file), main_dir);
            out.extend_from_slice(
                format!(" in file {}, line {}", rel.display(), self.diag.line).as_bytes(),
            );
        }
        out
    }
}

/// Where evaluation messages go.
pub trait Output {
    fn message(&mut self, m: &Message<'_>);
}

/// Collects messages in memory (for tests and embedding).
#[derive(Debug, Default)]
pub struct Collect {
    pub lines: Vec<(Severity, DiagCode, String)>,
}

impl Output for Collect {
    fn message(&mut self, m: &Message<'_>) {
        self.lines.push((
            m.diag.severity,
            m.diag.code,
            String::from_utf8_lossy(m.text).into_owned(),
        ));
    }
}

/// OpenSCAD's console (`PRINT` in utils/printutils.cc): renders messages
/// the way the command line prints them, and applies its two filters.
///
/// - After five identical consecutive warning, error or trace lines,
///   further repeats are dropped (so an error inside a deep recursion does
///   not print thousands of identical trace lines).
/// - `--quiet` drops everything except errors.
#[derive(Debug)]
pub struct Console<W: Write> {
    out: W,
    quiet: bool,
    last: VecDeque<Vec<u8>>,
    main_dir: PathBuf,
    /// Rendered paths per (source map address, file, base directory), as
    /// computing a relative path touches the file system. The base is part
    /// of the key: a file's parser errors print relative to the working
    /// directory and its warnings relative to the main file's directory.
    paths: Vec<((usize, u32, PathBuf), String)>,
}

impl<W: Write> Console<W> {
    pub fn new(out: W, main_dir: PathBuf, quiet: bool) -> Self {
        Console {
            out,
            quiet,
            last: VecDeque::with_capacity(5),
            main_dir,
            paths: Vec::new(),
        }
    }

    pub fn into_inner(self) -> W {
        self.out
    }

    /// Print one line with OpenSCAD's filtering. `severity` is `None` for
    /// plain output (OpenSCAD's `message_group::NONE`).
    pub fn print(&mut self, severity: Option<Severity>, line: &[u8]) {
        let repeatable = matches!(
            severity,
            Some(Severity::Warning | Severity::Error | Severity::Trace)
        );
        if repeatable {
            if self.last.len() == 5 && self.last.iter().all(|l| l == line) {
                return;
            }
            if self.last.len() == 5 {
                self.last.pop_front();
            }
            self.last.push_back(line.to_vec());
        }
        if self.quiet && severity != Some(Severity::Error) {
            return;
        }
        let _ = self.out.write_all(line);
        let _ = self.out.write_all(b"\n");
    }

    fn path_of(&mut self, sources: &SourceMap, span: Span, base: &Path) -> String {
        let (addr, file) = (sources as *const SourceMap as usize, span.file.0);
        if let Some((_, p)) = self
            .paths
            .iter()
            .find(|(k, _)| k.0 == addr && k.1 == file && k.2 == base)
        {
            return p.clone();
        }
        let p = relative_path(sources.path(span.file), base)
            .display()
            .to_string();
        self.paths
            .push(((addr, file, base.to_path_buf()), p.clone()));
        p
    }

    /// Print a front-end diagnostic (lexer, parser, include messages).
    pub fn diagnostic(&mut self, d: &Diagnostic, sources: &SourceMap, cwd: &Path) {
        let mut line = format!("{}: {}", d.severity.openscad_label(), d.message).into_bytes();
        if let Some(span) = d.span {
            let base = match d.base {
                PathBase::WorkingDir => cwd.to_path_buf(),
                PathBase::MainFileDir => self.main_dir.clone(),
            };
            let p = self.path_of(sources, span, &base);
            line.extend_from_slice(format!(" in file {}, line {}", p, d.line).as_bytes());
        }
        self.print(Some(d.severity), &line);
    }
}

impl<W: Write> Output for Console<W> {
    fn message(&mut self, m: &Message<'_>) {
        let mut line = Vec::with_capacity(m.text.len() + 48);
        line.extend_from_slice(m.diag.severity.openscad_label().as_bytes());
        line.extend_from_slice(b": ");
        line.extend_from_slice(m.text);
        if let (Some(span), Some(sources)) = (m.diag.span, m.sources) {
            let base = self.main_dir.clone();
            let p = self.path_of(sources, span, &base);
            line.extend_from_slice(format!(" in file {}, line {}", p, m.diag.line).as_bytes());
        }
        self.print(Some(m.diag.severity), &line);
    }
}

/// A source location: a unit (file set) and a span in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Loc {
    pub unit: u32,
    pub span: Span,
}

/// A message waiting to be printed (a trace line beyond the budget).
#[derive(Debug, Clone)]
pub(crate) struct Pending {
    pub severity: Severity,
    pub code: DiagCode,
    pub text: Vec<u8>,
    pub loc: Option<Loc>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum UnwindKind {
    Recursion,
    Assertion,
    LoopLimit,
    /// A vector too deeply nested to print.
    EchoStack,
    /// The embedder asked evaluation to stop.
    Interrupted,
    /// `--hardwarnings`: a warning was printed (`HardWarningException`).
    HardWarning,
}

/// An evaluation error on its way up (OpenSCAD's `EvaluationException`).
#[derive(Debug)]
pub(crate) struct Unwind {
    pub kind: UnwindKind,
    /// Trace lines still allowed to print immediately; goes negative.
    pub depth: i32,
    tail: VecDeque<Pending>,
    cap: usize,
}

impl Unwind {
    pub fn new(kind: UnwindKind, trace_depth: u32) -> Box<Unwind> {
        Box::new(Unwind {
            kind,
            depth: trace_depth as i32,
            tail: VecDeque::new(),
            cap: trace_depth as usize,
        })
    }

    /// `EvaluationException::LOG`: print now, or keep for the end. Returns
    /// the message when it should print now.
    pub fn log(&mut self, p: Pending) -> Option<Pending> {
        if self.kind == UnwindKind::Interrupted {
            return None;
        }
        if self.depth > 0 {
            return Some(p);
        }
        if self.cap == 0 {
            return None;
        }
        if self.tail.len() == self.cap {
            self.tail.pop_front();
        }
        self.tail.push_back(p);
        None
    }

    /// What the destructor prints: the skipped-frames line and the kept
    /// tail.
    pub fn finish(self) -> Vec<Pending> {
        let mut out = Vec::new();
        if self.kind == UnwindKind::Interrupted {
            return out;
        }
        let skipped = -(i64::from(self.depth) + self.tail.len() as i64);
        let skipped = skipped as i32;
        if skipped > 0 {
            out.push(Pending {
                severity: Severity::Trace,
                code: DiagCode::Trace,
                text: format!("  *** Excluding {skipped} frames ***").into_bytes(),
                loc: None,
            });
        }
        out.extend(self.tail);
        out
    }
}

pub(crate) type R<T> = Result<T, Box<Unwind>>;
