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

use crate::loader::FileSystem;
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
    /// NeoSCAD's own: a fact worth knowing that is not a problem (an
    /// under-constrained sketch, as FreeCAD reports it). OpenSCAD has no
    /// such group; only NeoSCAD's extensions print one, so OpenSCAD's own
    /// programs never see the label.
    Info,
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
            Severity::Info => "INFO",
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
    /// `use <x.ttf>` (or `.otf`) naming a file that is not there:
    /// OpenSCAD's `Can't read font with path '...'`, an error printed
    /// without a location, after the `Can't open library` warning.
    FontNotFound,
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
    /// Two `part()`s with the same (dotted) name: neoscad's `part()`
    /// extension, on only with `--enable part`.
    DuplicatePart,
    /// A request passed one of a host's resource limits (`eval::limits`):
    /// too many fragments, slices, list elements, time or memory.
    ResourceLimit,
    /// The input file cannot be read (OpenSCAD's `Can't open input file`).
    InputNotFound,
    /// An output file cannot be written (`Can't write to ...`).
    OutputNotWritable,
    /// NeoSCAD's own (never printed on the console): a `polyhedron()` or
    /// imported mesh whose faces all point inward.
    PolyhedronInsideOut,
    /// NeoSCAD's own: some faces of a mesh wound against the rest.
    PolyhedronFlippedFaces,
    /// NeoSCAD's own: a mesh with edges used by only one face.
    PolyhedronOpen,
    /// NeoSCAD's own: a mesh with edges used by more than two faces, or
    /// that cannot be wound consistently.
    PolyhedronNotManifold,
    /// NeoSCAD's own: a `use`d file sets `$fn`, `$fa` or `$fs` at its top,
    /// which its modules never see (special variables come from the
    /// caller).
    UseSpecialVariables,
    /// NeoSCAD's `sketch()` extension (`--enable sketch`): constraints
    /// that contradict each other.
    SketchConflict,
    /// A sketch constraint implied by the others.
    SketchRedundant,
    /// A sketch with free degrees of freedom (info; an error with
    /// `strict = true`).
    SketchUnderconstrained,
    /// A sketch the solver could not bring to a solution.
    SketchNoConvergence,
    /// A sketch solved on another branch than the one drawn.
    SketchFlipped,
    /// A sketch profile curve whose end joins no other curve, or a point
    /// where more than two meet.
    SketchOpenProfile,
    /// A sketch fillet or chamfer longer than a line it trims.
    SketchFilletTooLarge,
    /// A sketch constraint given something that is not an entity.
    SketchUnknownEntity,
    /// An entity used outside the sketch that made it.
    SketchForeignEntity,
    /// Geometry instantiated inside a sketch body.
    SketchGeometryInBody,
    /// Sketch profile loops that cross each other or themselves, which the
    /// even-odd fill turns into a shape the author probably did not mean.
    SketchSelfIntersection,
    /// A sketch point written without a guess, which the solver placed
    /// (info).
    SketchNoGuess,
    /// NeoSCAD's queries (`--enable query`): a query outside any user
    /// module, which has no children to ask about.
    QueryOutsideModule,
    /// A query's child index out of range or not a number.
    QueryIndex,
    /// Two anchors of one name among the children a query asked about.
    QueryDuplicateAnchor,
    /// A geometry query (`child_bounds()`) about children that render to
    /// nothing.
    QueryEmpty,
    /// A geometry query in a host that cannot render, so has no answer.
    QueryUnavailable,
    /// NeoSCAD's `fillet_edges()`/`chamfer_edges()` (`--enable fillet`):
    /// an edge selector that does not parse, or is not a selector.
    FilletSelector,
    /// A fillet or chamfer call whose geometry this version does not
    /// build yet: the children are rendered unchanged (warning).
    FilletNotBuilt,
    /// A fillet call matched a different number of edges than its
    /// `expect` (error).
    FilletCount,
    /// A fillet call whose selector matched no edge (warning).
    FilletNoEdges,
    /// Edges a selector named that are never filleted: polygon seams,
    /// tangent edges, edges of faceted regions (info).
    FilletSkipped,
    /// Selected edges of a kind the blends do not cover (error, or a
    /// warning under the default `edges = "all"`).
    FilletUnsupportedEdge,
    /// A fillet call whose child has no B-rep to select edges on.
    FilletNoBrep,
    /// A fillet call on 2D children.
    Fillet2d,
    /// A blend that does not fit its edge's cross-section or a face
    /// beside it (error, with the largest size that fits).
    FilletTooLarge,
    /// Two blends whose strips overlap on a face (error).
    FilletOverlap,
    /// Selected edges meeting at a vertex the blends cannot join: convex
    /// and concave edges, more than three faces, a curved face (error).
    FilletUnsupportedVertex,
    /// Something else in the child cuts into a blend (info).
    FilletInterrupted,
    /// The blends could not be built, or the result does not hold them
    /// (error).
    FilletFailed,
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
            DiagCode::FontNotFound => "font-not-found",
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
            DiagCode::DuplicatePart => "duplicate-part",
            DiagCode::ResourceLimit => "resource-limit",
            DiagCode::InputNotFound => "input-not-found",
            DiagCode::OutputNotWritable => "output-not-writable",
            DiagCode::PolyhedronInsideOut => "polyhedron-inside-out",
            DiagCode::PolyhedronFlippedFaces => "polyhedron-flipped-faces",
            DiagCode::PolyhedronOpen => "polyhedron-open",
            DiagCode::PolyhedronNotManifold => "polyhedron-not-manifold",
            DiagCode::UseSpecialVariables => "use-special-variables",
            DiagCode::SketchConflict => "sketch-conflict",
            DiagCode::SketchRedundant => "sketch-redundant",
            DiagCode::SketchUnderconstrained => "sketch-underconstrained",
            DiagCode::SketchNoConvergence => "sketch-no-convergence",
            DiagCode::SketchFlipped => "sketch-flipped",
            DiagCode::SketchOpenProfile => "sketch-open-profile",
            DiagCode::SketchFilletTooLarge => "sketch-fillet-too-large",
            DiagCode::SketchUnknownEntity => "sketch-unknown-entity",
            DiagCode::SketchForeignEntity => "sketch-foreign-entity",
            DiagCode::SketchGeometryInBody => "sketch-geometry-in-body",
            DiagCode::SketchSelfIntersection => "sketch-self-intersection",
            DiagCode::SketchNoGuess => "sketch-no-guess",
            DiagCode::QueryOutsideModule => "query-outside-module",
            DiagCode::QueryIndex => "query-index",
            DiagCode::QueryDuplicateAnchor => "query-duplicate-anchor",
            DiagCode::QueryEmpty => "query-empty",
            DiagCode::QueryUnavailable => "query-unavailable",
            DiagCode::FilletSelector => "fillet-selector",
            DiagCode::FilletNotBuilt => "fillet-not-built",
            DiagCode::FilletCount => "fillet-count",
            DiagCode::FilletNoEdges => "fillet-no-edges",
            DiagCode::FilletSkipped => "fillet-skipped",
            DiagCode::FilletUnsupportedEdge => "fillet-unsupported-edge",
            DiagCode::FilletNoBrep => "fillet-no-brep",
            DiagCode::Fillet2d => "fillet-2d",
            DiagCode::FilletTooLarge => "fillet-too-large",
            DiagCode::FilletOverlap => "fillet-overlap",
            DiagCode::FilletUnsupportedVertex => "fillet-unsupported-vertex",
            DiagCode::FilletInterrupted => "fillet-interrupted",
            DiagCode::FilletFailed => "fillet-failed",
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
        self.hints.push(Hint {
            message: message.into(),
            replacement: None,
        });
        self
    }

    pub fn is_error(&self) -> bool {
        self.severity == Severity::Error
    }

    /// The line OpenSCAD prints: `ERROR: <message> in file <path>, line <n>`.
    /// `cwd` and `main_dir` resolve [`PathBase`]; `fs` resolves symlinks
    /// in the path and its base (see [`relative_path`]).
    pub fn render_openscad(
        &self,
        sources: &SourceMap,
        cwd: &Path,
        main_dir: &Path,
        fs: &dyn FileSystem,
    ) -> String {
        let mut s = format!("{}: {}", self.severity.openscad_label(), self.message);
        if let Some(span) = self.span {
            let base = match self.base {
                PathBase::WorkingDir => cwd,
                PathBase::MainFileDir => main_dir,
            };
            let rel = relative_display(sources.path(span.file), base, fs);
            s.push_str(&format!(" in file {rel}, line {}", self.line));
        }
        s
    }
}

/// `std::filesystem::relative(path, base)`: both sides are made canonical
/// where they exist (resolving symlinks such as macOS `/tmp`), then the
/// shortest `..`-path from `base` to `path` is returned.
///
/// The canonical forms come from `fs`, never from the disk directly: a
/// library crate that asked the disk would resolve paths the host's file
/// system does not have (an in-memory document, a root-limited server),
/// and would break the rule that only hosts touch the machine. A relative
/// path or base is taken against `fs`'s answer for `.`, which for the
/// disk is the working directory, so the host decides what that is. Where
/// `fs` resolves nothing (an empty or in-memory file system, which has no
/// symlinks) the result is the lexical one, which is then exact.
pub fn relative_path(path: &Path, base: &Path, fs: &dyn FileSystem) -> PathBuf {
    let p = weakly_canonical(path, fs);
    let b = weakly_canonical(base, fs);
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

/// [`relative_path`] as OpenSCAD prints one in a message: `/`-separated on
/// every host (`fs_uncomplete(..).generic_string()`, `AST.cc`), so output
/// and the tests that compare it are the same on Windows as elsewhere.
pub fn relative_display(path: &Path, base: &Path, fs: &dyn FileSystem) -> String {
    crate::loader::generic(&relative_path(path, base, fs))
}

/// `std::filesystem::weakly_canonical`: canonicalise the longest prefix
/// `fs` resolves and append the rest lexically.
fn weakly_canonical(path: &Path, fs: &dyn FileSystem) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut rest = Vec::new();
    loop {
        // A relative path runs out at the empty path, which stands for the
        // working directory: ask `fs` for `.` there (the disk resolves it,
        // as it resolves every relative path, against the process's).
        let probe = if existing.as_os_str().is_empty() {
            Path::new(".")
        } else {
            existing.as_path()
        };
        // Plain, so a base that exists (verbatim from `canonicalize` on
        // Windows) and a path that does not (lexical, plain) still share
        // their leading components.
        let resolved = fs
            .canonicalize(probe)
            .map(crate::paths::plain)
            .filter(|c| *c != normalize_lexically(probe));
        // An answer that only normalises the path is passed over: it may
        // come from a layer that resolves nothing (a document never saved,
        // the bundled libraries mounted in memory) over a directory that
        // is a symlink on disk, and taking it would print an unsaved
        // `/link/a.scad` as `../link/a.scad` from `/real`. On the
        // disk the ancestors of a canonical path are canonical too, so
        // looking further up changes nothing there.
        if let Some(mut out) = resolved {
            for r in rest.iter().rev() {
                out.push(r);
            }
            return normalize_lexically(&out);
        }
        match (
            existing.file_name().map(|f| f.to_os_string()),
            existing.parent(),
        ) {
            (Some(name), Some(parent)) => {
                rest.push(name);
                existing = parent.to_path_buf();
            }
            _ => return normalize_lexically(path),
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
    use std::io;

    use super::*;
    use crate::vfs::MemFs;

    /// A disk as `relative_path` sees it: `/link` is a symlink to `/real`
    /// (as `/tmp` is on macOS), `/work` is the working directory, a
    /// few directories exist, and `/link/p/doc.scad` is a document never
    /// saved, which (like a session's buffers) resolves to itself.
    struct Linked;

    impl FileSystem for Linked {
        fn read(&self, _: &Path) -> io::Result<Vec<u8>> {
            Err(io::ErrorKind::NotFound.into())
        }
        fn exists(&self, path: &Path) -> bool {
            self.canonicalize(path).is_some()
        }
        fn is_dir(&self, path: &Path) -> bool {
            self.exists(path)
        }
        fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
            let abs = normalize_lexically(&Path::new("/work").join(path));
            if abs == Path::new("/link/p/doc.scad") {
                return Some(abs);
            }
            let abs = match abs.strip_prefix("/link") {
                Ok(r) => Path::new("/real").join(r),
                Err(_) => abs,
            };
            ["/", "/real", "/real/p", "/work"]
                .iter()
                .any(|d| abs == Path::new(d))
                .then_some(abs)
        }
    }

    #[test]
    fn relative_paths_like_std_filesystem() {
        let fs = MemFs::new();
        assert_eq!(
            relative_path(
                Path::new("/nonexist/a/b.scad"),
                Path::new("/nonexist/a"),
                &fs
            ),
            Path::new("b.scad")
        );
        assert_eq!(
            relative_path(
                Path::new("/nonexist/t/data/x.scad"),
                Path::new("/nonexist/b/t"),
                &fs
            ),
            Path::new("../../t/data/x.scad")
        );
        assert_eq!(
            relative_path(Path::new("/nonexist/a"), Path::new("/nonexist/a"), &fs),
            Path::new(".")
        );
    }

    /// Symlinks and the working directory come from the file system: a
    /// path through `/link` meets a base under `/real`, and a
    /// relative path or base is taken from the file system's `.`. Resolved
    /// lexically instead, the first would print `../../link/p/x.scad`.
    #[test]
    fn relative_paths_resolve_through_the_file_system() {
        let fs = Linked;
        assert_eq!(
            relative_path(Path::new("/link/p/x.scad"), Path::new("/real/p"), &fs),
            Path::new("x.scad")
        );
        assert_eq!(
            relative_path(Path::new("/link/p/doc.scad"), Path::new("/real/p"), &fs),
            Path::new("doc.scad")
        );
        assert_eq!(
            relative_path(Path::new("sub/x.scad"), Path::new("/work"), &fs),
            Path::new("sub/x.scad")
        );
        assert_eq!(
            relative_path(Path::new("/work/x.scad"), Path::new(""), &fs),
            Path::new("x.scad")
        );
        assert_eq!(
            relative_path(Path::new("../x.scad"), Path::new("/real"), &fs),
            Path::new("../x.scad")
        );
    }

    #[test]
    fn renders_openscad_format() {
        let fs = MemFs::new();
        let mut sm = SourceMap::new();
        let f = sm.add("/nonexist/d/e.scad".into(), b"x".to_vec());
        let d = Diagnostic::new(
            DiagCode::SyntaxError,
            Severity::Error,
            "Parser error: syntax error",
        )
        .at(Span::new(f, 0, 1), 3);
        assert_eq!(
            d.render_openscad(&sm, Path::new("/nonexist/d"), Path::new("/"), &fs),
            "ERROR: Parser error: syntax error in file e.scad, line 3"
        );
        let d = Diagnostic::new(DiagCode::SyntaxError, Severity::Warning, "plain");
        assert_eq!(
            d.render_openscad(&sm, Path::new("/"), Path::new("/"), &fs),
            "WARNING: plain"
        );
    }
}
