//! The long-lived NeoSCAD core: the API every long-lived client uses.
//! `neoscad serve` holds one for all its clients, including `neoscad mcp`
//! (which calls serve's `Local` in-process) and the one-shot command line's
//! `cli.*` requests when it delegates to a running server. `neoscad lsp`
//! holds its own, the macOS app drives one through UniFFI (`crates/ffi`,
//! which also runs the `lsp` server over it), and the WASM check
//! (`crates/wasm-check`) builds one. The command line's `snapshot`,
//! `check`, `measure`, `format`, `docs` and `test` subcommands make a
//! short-lived one when they run in-process. The OpenSCAD-compatible
//! one-shot export (`crates/cli/src/run.rs`) does not: it runs its own
//! pipeline and borrows only this crate's encoders and statistics.
//!
//! A session holds:
//!
//! - **documents**: paths with their unsaved text ([`Session::open`],
//!   [`Session::update`], [`Session::edit`]). Every file the pipeline
//!   reads goes through a file system that serves those buffers over the
//!   host's, so includes and imports see unsaved text too;
//! - **warm caches**: parsed files keyed by content and validated by the
//!   metadata of every file they read (`parse`), the geometry cache of
//!   each renderer (subtrees keyed by their Merkle hash, so an edit
//!   recomputes only the subtrees it changed), fonts, and each document's
//!   last CSG products and top-level statements' evaluation (an edit
//!   evaluates only the statements whose inputs it changed; see
//!   `eval::evaluate_incremental` and [`Config::reuse_evaluation`]);
//! - **a budget**: both caches evict least recently used entries past
//!   their budgets ([`Config`]), and [`Session::stats`] reports them.
//!
//! Operations: [`Session::evaluate`] (diagnostics, echo, the tree),
//! [`Session::render`] (preview or full, with geometry statistics),
//! [`Session::export`], [`Session::snapshot`] and [`Session::cancel`].
//!
//! # Threads and cancellation
//!
//! Every operation is a blocking call that takes `&self`, so a native host
//! runs requests on as many threads as it likes and a WASM host calls them
//! in turn. Each request on a document holds an interrupt flag that the
//! evaluator checks at every call and loop iteration and the geometry
//! evaluator before every node. A newer request on the same document (or
//! an edit to it) sets the flags of the requests it supersedes, so a stale
//! render stops within one kernel operation and returns [`Cancelled`]; the
//! geometry it finished stays cached. [`Session::cancel`] sets them
//! explicitly. On wasm32 nothing runs concurrently, so a request is only
//! ever stopped by a host that calls `cancel` from a callback; everything
//! else works the same, synchronously.
//!
//! # No platform access
//!
//! Like every library crate, the session never touches `std::fs`, the
//! environment or the clock: files come through [`Config::fs`], fonts
//! through [`Config::fonts`], timings through [`Config::clock`] and the
//! GPU through [`Config::gpu`].

pub mod check;
pub mod diag;
mod docfs;
pub mod docs;
pub mod export;
pub mod format;
pub mod measure;
pub mod memory;
pub mod mesh;
pub mod modeltest;
pub mod orient;
mod parse;
pub mod parts;
pub mod snapshot;
pub mod stats;
mod usehint;

use std::collections::HashMap;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

pub use eval::limits::{Exceeded, Limit, Limits, MemoryProbe};
use eval::{Console, Logged};
use lang::Program;
use lang::diag::{DiagCode, Diagnostic, Severity};
use lang::loader::{FileSystem, LibraryPath};
use serde_json::{Value, json};

pub use diag::Names;
pub use docfs::normal;
pub use parse::{Lib, PARSE_BUDGET, ParseStats};

/// OpenSCAD's general failure exit code.
pub const EXIT_ERROR: u8 = 1;
/// A feature neoscad does not have yet (see the command line).
pub const EXIT_NOT_IMPLEMENTED: u8 = 3;

/// Builds the fonts `text()` sees, given the `use`d files of the program
/// and its libraries (fonts among them are added).
pub type FontProvider = Arc<dyn Fn(&[String]) -> text::FontDb + Send + Sync>;
/// Milliseconds on any monotonic clock, for timings.
pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;
/// Hands memory the process freed back to the system, so that the next
/// [`MemoryProbe`] reading shows it ([`Config::memory_release`]).
pub type MemoryRelease = Arc<dyn Fn() + Send + Sync>;
/// Opens (once) the GPU snapshots draw on.
#[cfg(feature = "gpu")]
pub type GpuProvider =
    Arc<dyn Fn() -> Result<&'static render::offscreen::Offscreen, String> + Send + Sync>;

/// What a session gets from its host.
#[derive(Clone)]
pub struct Config {
    /// The host's files; open documents' buffers are served over them.
    pub fs: Arc<dyn FileSystem + Send + Sync>,
    pub libs: LibraryPath,
    pub fonts: FontProvider,
    /// Where relative inputs are found and messages are relative to,
    /// unless a request says otherwise ([`Run::cwd`]).
    pub work_dir: PathBuf,
    /// Estimated bytes of cached geometry (per renderer; one per colour
    /// scheme and font set in use).
    pub geometry_budget: usize,
    /// Estimated bytes of cached parses.
    pub parse_budget: usize,
    /// For timings; without one every timing is 0.
    pub clock: Option<Clock>,
    /// The seed of unseeded `rands()`.
    pub rng_seed: u32,
    #[cfg(feature = "gpu")]
    pub gpu: Option<GpuProvider>,
    /// neoscad's `part()` extension for every request (`--enable part`);
    /// a request can also turn it on alone ([`Run::parts`]).
    pub parts: bool,
    /// OpenSCAD's experimental features for every request (`--enable`);
    /// a request can add its own ([`Run::features`]).
    pub features: eval::Features,
    /// Every request's resource limits ([`eval::limits`]), unless it says
    /// otherwise ([`Run::limits`]). [`Limits::NONE`] (the default) is
    /// OpenSCAD's behaviour, for the one-shot command line; a host that
    /// runs models it did not write (`serve`, `mcp`, the app) sets
    /// [`Limits::AGENT`] or its own. The time limit needs [`Config::clock`].
    pub limits: Limits,
    /// Reuse the evaluation of a document's top-level statements whose
    /// inputs an edit did not change (`eval::evaluate_incremental`). On by
    /// default; the result is the same either way, so turning it off is for
    /// comparing against full evaluations.
    pub reuse_evaluation: bool,
    /// The host's measurement of the memory in use, checked against each
    /// request's memory limit besides the estimate
    /// ([`eval::limits::Guard::with_probe`]). It measures the whole
    /// process, so the memory limit becomes a budget shared by every
    /// document and request the host runs; before a reading over the
    /// limit fails a request, the session evicts cached geometry and
    /// measures again (see `memory.rs`). The probe is read at most every
    /// 10 ms on [`Config::clock`]. None (the default) keeps the estimate
    /// alone, which is the same on every machine.
    pub memory_probe: Option<MemoryProbe>,
    /// Called after cached geometry is evicted under memory pressure and
    /// before the probe is read again: a host whose allocator keeps freed
    /// pages for a while (mimalloc purges after a second) returns them
    /// here, or the reading would not show what eviction gave back.
    pub memory_release: Option<MemoryRelease>,
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("libs", &self.libs)
            .field("work_dir", &self.work_dir)
            .field("geometry_budget", &self.geometry_budget)
            .field("parse_budget", &self.parse_budget)
            .finish_non_exhaustive()
    }
}

impl Config {
    /// A configuration with no fonts beyond what `fs` holds at
    /// `use <font.ttf>` paths, no clock and no GPU.
    pub fn new(fs: Arc<dyn FileSystem + Send + Sync>, libs: LibraryPath) -> Config {
        let font_fs = fs.clone();
        Config {
            fs,
            libs,
            fonts: Arc::new(move |used: &[String]| {
                let mut db = text::FontDb::with_fs(font_fs.clone());
                for u in used {
                    if is_font(u) {
                        db.add_file(Path::new(u));
                    }
                }
                db
            }),
            work_dir: PathBuf::from("/"),
            geometry_budget: geom::CACHE_BUDGET,
            parse_budget: PARSE_BUDGET,
            clock: None,
            rng_seed: 0,
            #[cfg(feature = "gpu")]
            gpu: None,
            parts: false,
            features: eval::Features::NONE,
            limits: Limits::NONE,
            reuse_evaluation: true,
            memory_probe: None,
            memory_release: None,
        }
    }
}

/// Whether a `use`d file is a font (`SourceFile::registerUse`).
pub fn is_font(name: &str) -> bool {
    Path::new(name)
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("ttf") || e.eq_ignore_ascii_case("otf"))
}

/// A stage of a request, reported to [`Run::progress`] as it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    Parse,
    Evaluate,
    Geometry,
    /// Drawing and encoding a snapshot.
    Draw,
}

impl Stage {
    pub fn name(self) -> &'static str {
        match self {
            Stage::Parse => "parse",
            Stage::Evaluate => "evaluate",
            Stage::Geometry => "geometry",
            Stage::Draw => "draw",
        }
    }
}

/// Told each [`Stage`] as it starts (a server turns these into progress
/// notifications).
pub type Progress = Arc<dyn Fn(Stage) + Send + Sync>;

/// One request's input and how its messages print.
#[derive(Clone)]
pub struct Run {
    /// The model as named (relative to [`Run::cwd`] or absolute). Messages
    /// name it this way, as the command line does.
    pub input: String,
    /// The working directory: relative names resolve against it and
    /// parser messages print relative to it. Default: [`Config::work_dir`].
    pub cwd: Option<PathBuf>,
    /// `-D` assignments, appended after the text as the command line does.
    pub defines: Vec<String>,
    /// `--quiet`: print only errors.
    pub quiet: bool,
    /// Follow located diagnostics with the source line and a caret (for a
    /// terminal; never in OpenSCAD-compatible output).
    pub rich: bool,
    /// The command line's camera (`--camera`, `--viewall`), which `$vp*`
    /// start from.
    pub camera: eval::Camera,
    /// The seed of unseeded `rands()`; default [`Config::rng_seed`].
    pub rng_seed: Option<u32>,
    /// Cancel older requests on the same document when this one starts.
    /// An editor's or agent's requests do; the command line's one-shot
    /// requests do not, so two exports of one file can run side by side.
    pub supersede: bool,
    pub progress: Option<Progress>,
    /// neoscad's `part("name") { ... }` extension (`--enable part`), on
    /// for this request; see `eval::Options::parts`.
    pub parts: bool,
    /// OpenSCAD's experimental features (`--enable`) for this request, on
    /// top of [`Config::features`]; see `eval::Options::features`.
    pub features: eval::Features,
    /// Run only this module of the main file: its top-level
    /// instantiations are replaced by one call, `entry();`, while its
    /// assignments, definitions, includes and `use`s stay. This is how
    /// `neoscad test` runs each `module test_*()` of a test file as its
    /// own model.
    pub entry: Option<String>,
    /// This request's resource limits instead of [`Config::limits`] (the
    /// command line's requests to a server are unlimited, as it is).
    pub limits: Option<Limits>,
    /// The main file's text for this request, instead of what the file
    /// system or the document's buffer holds. A language server evaluates
    /// the exact version its client sent, which can be a keystroke ahead
    /// of or behind the buffer another client (the app's own edit path)
    /// keeps; its diagnostics would otherwise land on the wrong text.
    /// Other files still read through the session.
    pub text: Option<Arc<[u8]>>,
    /// A flag the host sets to stop this request, besides the session's
    /// own superseding. A language server's evaluation does not supersede
    /// (it must not cancel the app's render of the same document, nor be
    /// cancelled by it), so it stops its own stale runs through this.
    pub interrupt: Option<Arc<AtomicBool>>,
    /// Told the messages so far once evaluation has finished and before
    /// the geometry stage starts ([`Session::render`] only). An editor
    /// shows the evaluation's diagnostics without waiting for the geometry
    /// (which a preview of a large model can take seconds over); the
    /// finished request's log then adds the geometry stage's own.
    pub on_evaluated: Option<EvaluatedHook>,
}

/// The hook of [`Run::on_evaluated`].
pub type EvaluatedHook = Arc<dyn Fn(&Log) + Send + Sync>;

impl std::fmt::Debug for Run {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Run")
            .field("input", &self.input)
            .field("cwd", &self.cwd)
            .field("defines", &self.defines)
            .field("quiet", &self.quiet)
            .field("supersede", &self.supersede)
            .finish_non_exhaustive()
    }
}

impl Run {
    fn stage(&self, s: Stage) {
        if let Some(p) = &self.progress {
            p(s);
        }
    }

    pub fn new(input: impl Into<String>) -> Run {
        Run {
            input: input.into(),
            cwd: None,
            defines: Vec::new(),
            quiet: false,
            rich: false,
            camera: eval::Camera::default(),
            rng_seed: None,
            supersede: true,
            progress: None,
            parts: false,
            features: eval::Features::NONE,
            entry: None,
            limits: None,
            text: None,
            interrupt: None,
            on_evaluated: None,
        }
    }
}

/// A request stopped because a newer one (or [`Session::cancel`]) asked it
/// to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cancelled;

impl std::fmt::Display for Cancelled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("cancelled by a newer request")
    }
}

/// An open document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocInfo {
    pub path: PathBuf,
    /// Bumped by every change; 0 for a document read from disk.
    pub version: u64,
    /// Bytes of text, when the session holds it.
    pub len: Option<usize>,
}

/// A replacement of `start..end` (byte offsets into the current text) by
/// `text`. Edits apply in order, each to the result of the previous one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextEdit {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

/// What a request printed.
#[derive(Debug, Default, Clone)]
pub struct Log {
    /// The bytes the command line would have printed on stderr: OpenSCAD's
    /// lines, word for word.
    pub stderr: Vec<u8>,
    /// The same lines with their tool view.
    pub lines: Vec<Logged>,
    /// Names the program defines, for "did you mean" hints.
    pub names: Arc<Names>,
}

impl Log {
    /// Errors, warnings and deprecations as JSON (`docs/cli-json.md`),
    /// each error with the `TRACE:` lines that followed it.
    pub fn diagnostics_json(&self) -> Vec<Value> {
        diag::list_json(&self.lines, &self.names)
    }

    /// `echo()` output, one line each, as printed.
    pub fn echo(&self) -> Vec<String> {
        self.lines
            .iter()
            .filter(|l| l.severity == Some(Severity::Echo))
            .map(|l| l.text.clone())
            .collect()
    }

    pub fn count(&self, s: Severity) -> usize {
        self.lines.iter().filter(|l| l.severity == Some(s)).count()
    }
}

/// Timings of one request, in milliseconds (0 without a clock).
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Timings {
    /// Reading and parsing (from the cache when unchanged).
    pub parse: f64,
    pub evaluate: f64,
    /// The render or the preview's products.
    pub geometry: f64,
    pub total: f64,
}

impl Timings {
    pub fn json(&self) -> Value {
        let r = |ms: f64| (ms * 10.0).round() / 10.0;
        json!({
            "parse": r(self.parse),
            "evaluate": r(self.evaluate),
            "geometry": r(self.geometry),
            "total": r(self.total),
        })
    }
}

/// The result of [`Session::evaluate`].
#[derive(Debug)]
pub struct Evaluated {
    /// 0, or the command line's exit code for the failure.
    pub exit_code: u8,
    pub log: Log,
    /// The node tree as a `.csg` file, when asked for.
    pub csg: Option<String>,
    /// Whether an evaluation error stopped evaluation early.
    pub aborted: bool,
    pub timings: Timings,
    /// Every file the request found, sorted ([`Rendered::files`]).
    pub files: Vec<PathBuf>,
}

/// What [`Session::render`] builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// The full geometry (`--render`).
    Render,
    /// Full geometry, a mesh result converted to a solid
    /// (`--render=force`).
    Force,
    /// OpenSCAD's preview: the leaves' geometry and the CSG products.
    Preview,
}

/// The result of [`Session::render`].
#[derive(Debug)]
pub struct Rendered {
    pub exit_code: u8,
    pub log: Log,
    /// The rendered geometry (`None` when empty or previewing).
    pub geometry: Option<geom::Geometry>,
    /// The preview's CSG products.
    pub tree: Option<Arc<geom::csg::CsgTree>>,
    /// The request's interrupt flag and limits, for the host to draw
    /// `tree` under (`render::preview::scene_until`): the products'
    /// booleans are the preview's expensive part, and they run after this
    /// returns. The time limit counts from the request's start. A newer
    /// request on the document no longer sets the flag once this returns
    /// (only the host's own `Run::interrupt` and the limits stop it).
    pub stop: geom::csg::Stop,
    /// The file's view after `$vp*`.
    pub camera: eval::Camera,
    /// Which `$vp*` the file assigned itself (a GUI moves its view to
    /// these; the others keep the view's own).
    pub camera_assigned: eval::CameraAssigned,
    /// Entries in the geometry cache after the render.
    pub cache_entries: usize,
    /// The geometry cache's estimated size and its budget after the
    /// render, in bytes, for the render summary.
    pub cache_bytes: usize,
    pub cache_budget: usize,
    pub timings: Timings,
    /// Every file the request found (read, or asked the metadata of),
    /// sorted: the main file, its includes, the libraries it uses and
    /// their includes, imported files and fonts. Open documents' buffers
    /// are among them under their paths. A host that re-runs a document
    /// when its inputs change on disk watches these.
    pub files: Vec<PathBuf>,
    /// Polyhedra and imported meshes that do not bound a solid (also in
    /// the log as NeoSCAD-only warnings): what `check` reports and what a
    /// pinched result is likely to come from.
    pub inputs: Vec<orient::InputIssue>,
}

impl Rendered {
    /// `geometry` as JSON (`null` when empty): see [`stats::geometry`].
    pub fn geometry_json(&self, scheme: &geom::color::Scheme) -> Value {
        self.geometry
            .as_ref()
            .map_or(Value::Null, |g| stats::geometry(g, scheme))
    }
}

/// An export request: the model, its outputs in order, and how to encode
/// them.
#[derive(Debug, Clone)]
pub struct ExportRequest {
    pub run: Run,
    /// Each output: its target as named, and its format.
    pub outputs: Vec<(String, export::Format)>,
    /// `--render=force`.
    pub force: bool,
    /// The render colour scheme (its face colours reach exported meshes).
    pub scheme: render::ColorScheme,
    pub settings: export::Settings,
}

/// Where an export's files go, and how its render summary prints: the
/// host's side of [`Session::export`] (the session itself writes nothing).
pub trait ExportSink {
    /// Write one output; on failure the line to print (the command line
    /// prints `ERROR: Can't write to '...': ...`), which ends the export.
    fn write(&mut self, target: &str, data: &[u8]) -> Result<(), String>;
    /// Print the render summary after the outputs (`RenderStatistic`);
    /// `false` fails the export.
    fn summary(&mut self, facts: &SummaryFacts<'_>, con: &mut Console<Vec<u8>>) -> bool;
}

/// What an export's render summary reports.
#[derive(Debug)]
pub struct SummaryFacts<'a> {
    pub cache_entries: usize,
    /// The geometry cache's estimated size and its budget, in bytes
    /// (OpenSCAD's `Geometry cache size in bytes` and `max_size`).
    pub cache_bytes: usize,
    pub cache_budget: usize,
    /// From the start of geometry evaluation to the summary.
    pub elapsed_ms: f64,
    pub geometry: Option<&'a geom::Geometry>,
    /// The file's view after `$vp*`.
    pub camera: &'a eval::Camera,
}

/// The result of [`Session::export`].
#[derive(Debug)]
pub struct Exported {
    pub exit_code: u8,
    pub log: Log,
    pub geometry: Option<geom::Geometry>,
    pub timings: Timings,
}

/// Session-wide numbers.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Stats {
    pub documents: usize,
    pub parse: ParseStats,
    /// Summed over the renderers.
    pub geometry: geom::CacheStats,
    pub renderers: usize,
    pub requests: u64,
    pub cancelled: u64,
    pub running: usize,
    /// Included files held read and lexed: entries and bytes.
    pub lexed: (usize, usize),
    /// Included files held parsed: entries and estimated bytes.
    pub fragments: (usize, usize),
}

impl Stats {
    pub fn json(&self) -> Value {
        let g = &self.geometry;
        let p = &self.parse;
        json!({
            "documents": self.documents,
            "requests": self.requests,
            "cancelled": self.cancelled,
            "running": self.running,
            "renderers": self.renderers,
            "parse_cache": {"entries": p.entries, "bytes": p.bytes, "budget": p.budget,
                "hits": p.hits, "misses": p.misses, "evictions": p.evictions,
                "lexed_files": self.lexed.0, "lexed_bytes": self.lexed.1,
                "fragment_files": self.fragments.0, "fragment_bytes": self.fragments.1},
            "geometry_cache": {"entries": g.entries, "bytes": g.bytes, "budget": g.budget,
                "hits": g.hits, "misses": g.misses, "evictions": g.evictions},
        })
    }
}

/// A request in flight on a document.
#[derive(Debug)]
struct Job {
    id: u64,
    flag: Arc<AtomicBool>,
    supersede: bool,
}

/// Removes its job from the registry when the request ends.
struct JobGuard<'a> {
    session: &'a Session,
    doc: PathBuf,
    id: u64,
    flag: Arc<AtomicBool>,
    /// The request's resource limits, when it has any. Tripping one sets
    /// `flag`, so every stage stops as for a cancellation, and the limit
    /// is reported instead.
    limits: Option<Arc<eval::limits::Guard>>,
}

impl Drop for JobGuard<'_> {
    fn drop(&mut self) {
        let mut jobs = self
            .session
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(v) = jobs.get_mut(&self.doc) {
            v.retain(|j| j.id != self.id);
            if v.is_empty() {
                jobs.remove(&self.doc);
            }
        }
    }
}

impl JobGuard<'_> {
    fn stopped(&self) -> bool {
        self.flag.load(Ordering::Relaxed)
    }

    /// The limit the request passed, if one stopped it.
    fn exceeded(&self) -> Option<Exceeded> {
        self.limits.as_ref().and_then(|g| g.exceeded())
    }
}

/// A document's last render, reused when nothing it depends on changed.
#[derive(Debug, Clone)]
struct Product {
    /// Root key, epoch, mode, renderer, the preview's term limit and the
    /// request's limits.
    key: (u128, u64, Mode, u64, usize, u64),
    geometry: Option<geom::Geometry>,
    tree: Option<Arc<geom::csg::CsgTree>>,
    messages: Vec<geom::Msg>,
}

/// Where a request's names resolve and print relative to.
#[derive(Debug, Clone)]
struct Paths {
    cwd: PathBuf,
    main_dir: PathBuf,
    /// `cwd` joined with the input: the program's path, as the command
    /// line builds it.
    path: PathBuf,
    /// The document key.
    doc: PathBuf,
    display: String,
}

impl Paths {
    fn of(run: &Run, cfg: &Config) -> Paths {
        let cwd = run.cwd.clone().unwrap_or_else(|| cfg.work_dir.clone());
        let path = cwd.join(&run.input);
        let main_dir = path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| cwd.clone());
        Paths {
            doc: normal(&path),
            cwd,
            main_dir,
            path,
            display: run.input.clone(),
        }
    }
}

/// Replace the top-level instantiations of `p` with one call of the
/// module `entry` ([`Run::entry`]), located at its definition so its
/// messages point there.
fn entry_only(p: &mut Program, entry: &str) {
    let span = p
        .ast
        .root
        .modules
        .iter()
        .find(|m| p.ast.name(m.name) == entry)
        .map(|m| m.span)
        .unwrap_or_default();
    let name = p.ast.names.intern(entry);
    p.ast.root.instantiations = vec![lang::ast::Instantiation {
        name,
        args: Vec::new(),
        children: lang::ast::Scope::default(),
        kind: lang::ast::InstKind::Module,
        tag_root: false,
        tag_highlight: false,
        tag_background: false,
        span,
    }];
}

/// Why a request's pipeline stopped early.
enum Stop {
    /// With this exit code; the messages say why.
    Exit(u8),
    Cancelled,
}

/// A loaded program and its libraries.
struct Loaded {
    program: Arc<Program>,
    libs: Vec<Lib>,
    uses: Vec<String>,
    /// Identifies these sources for geometry message replay
    /// (`geom::RenderOptions::replay`).
    epoch: u64,
}

impl Loaded {
    /// The source map of evaluation unit `unit`: 0 is the main program,
    /// `1 + i` the i-th library.
    fn unit_sources(&self, unit: u32) -> Option<&lang::source::SourceMap> {
        if unit == 0 {
            return Some(&self.program.sources);
        }
        self.libs
            .get(unit as usize - 1)
            .and_then(|lib| lib.program.as_ref())
            .map(|p| &p.sources)
    }

    /// The program of evaluation unit `unit` (see [`Loaded::unit_sources`]).
    fn unit_program(&self, unit: u32) -> Option<&Program> {
        if unit == 0 {
            return Some(&self.program);
        }
        self.libs
            .get(unit as usize - 1)
            .and_then(|lib| lib.program.as_deref())
    }

    /// The `use`d files of the program and its libraries (fonts among
    /// them).
    fn used(&self) -> Vec<String> {
        std::iter::once(&*self.program)
            .chain(self.libs.iter().filter_map(|l| l.program.as_deref()))
            .flat_map(|p| p.ast.uses.iter().cloned())
            .collect()
    }
}

/// One request's console and clock.
struct Pipe {
    con: Console<Vec<u8>>,
    paths: Paths,
    t0: f64,
    timings: Timings,
    /// The programs loaded, for the names of "did you mean" hints, which
    /// are only collected when a diagnostic needs them.
    programs: Vec<Arc<Program>>,
    /// The files as this request sees them, noting what it found.
    fs: Arc<docfs::Recorder>,
    /// Problems with the input meshes (`orient`), as reported.
    inputs: Vec<orient::InputIssue>,
}

/// The core. See the crate documentation.
pub struct Session {
    cfg: Config,
    fs: Arc<docfs::DocFs>,
    parse: Mutex<parse::ParseCache>,
    lexed: parse::LexStore,
    fragments: parse::FragmentStore,
    /// By scheme and font set, most recently used last.
    renderers: memory::Renderers,
    /// The memory probe's last reading and the caches it evicts.
    pressure: Arc<memory::Pressure>,
    fonts: Mutex<Vec<(u64, Arc<text::FontDb>)>>,
    docs: Mutex<HashMap<PathBuf, u64>>,
    jobs: Mutex<HashMap<PathBuf, Vec<Job>>>,
    products: Mutex<HashMap<PathBuf, Product>>,
    /// Each recently evaluated document's statement memo, most recently
    /// used last (see [`Config::reuse_evaluation`]).
    memos: Mutex<Vec<(PathBuf, eval::Memo)>>,
    ids: AtomicU64,
    requests: AtomicU64,
    cancelled: AtomicU64,
    /// Each input mesh's analysis (`orient::analyze`) by its node's key,
    /// so a warm render of a large polyhedron does not look at it again.
    orient: Mutex<OrientMemo>,
}

/// Input mesh analyses by node key, oldest first; at most [`ORIENT_MEMO`].
type OrientMemo = std::collections::VecDeque<(u128, Arc<Option<orient::MeshIssues>>)>;

/// Meshes whose analysis is kept. An entry is small (a flipped face list
/// at most), and a model rarely has more distinct polyhedra than this.
const ORIENT_MEMO: usize = 64;

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("cfg", &self.cfg)
            .finish_non_exhaustive()
    }
}

/// Whether any line is a diagnostic whose "did you mean" hint needs the
/// program's names.
fn wants_names(lines: &[Logged]) -> bool {
    lines.iter().any(|l| {
        matches!(
            l.code,
            Some(DiagCode::UnknownModule | DiagCode::UnknownFunction | DiagCode::UnknownVariable)
        )
    })
}

/// At most this many renderers (colour schemes and font sets) stay warm.
const RENDERERS: usize = 4;

/// At most this many documents keep a statement memo, within
/// [`MEMO_TOTAL`] estimated bytes together (each also has its own budget,
/// `eval::MEMO_BUDGET`). A memo holds a copy of its document's node tree
/// per setting; a host evaluating many files in turn (an agent's checks)
/// does not keep them all.
const MEMOS: usize = 8;
const MEMO_TOTAL: usize = 2 * eval::MEMO_BUDGET;

fn hash_of(x: impl Hash) -> u64 {
    let mut h = DefaultHasher::new();
    x.hash(&mut h);
    h.finish()
}

impl Session {
    pub fn new(cfg: Config) -> Session {
        let renderers = memory::Renderers::default();
        Session {
            pressure: Arc::new(memory::Pressure::new(renderers.clone())),
            renderers,
            fs: Arc::new(docfs::DocFs::new(cfg.fs.clone())),
            parse: Mutex::new(parse::ParseCache::new(cfg.parse_budget)),
            lexed: parse::LexStore::new(cfg.parse_budget / 4),
            fragments: parse::FragmentStore::new(cfg.parse_budget / 2),
            fonts: Mutex::new(Vec::new()),
            docs: Mutex::new(HashMap::new()),
            jobs: Mutex::new(HashMap::new()),
            products: Mutex::new(HashMap::new()),
            memos: Mutex::new(Vec::new()),
            orient: Mutex::new(OrientMemo::new()),
            ids: AtomicU64::new(0),
            requests: AtomicU64::new(0),
            cancelled: AtomicU64::new(0),
            cfg,
        }
    }

    pub fn config(&self) -> &Config {
        &self.cfg
    }

    fn stores(&self) -> parse::Stores<'_> {
        parse::Stores {
            lexed: &self.lexed,
            fragments: &self.fragments,
        }
    }

    /// The file system requests read through: the host's files with the
    /// open documents' buffers over them.
    pub fn fs(&self) -> Arc<dyn FileSystem + Send + Sync> {
        self.fs.clone()
    }

    fn now(&self) -> f64 {
        self.cfg.clock.as_ref().map_or(0.0, |c| c())
    }

    fn doc_path(&self, path: &Path) -> PathBuf {
        normal(&self.cfg.work_dir.join(path))
    }

    // --- Documents ---------------------------------------------------------

    /// Open a document: with `text`, as an unsaved buffer that every read
    /// of its path sees; without, tracked but read from the host's files.
    pub fn open(&self, path: &Path, text: Option<Vec<u8>>) -> DocInfo {
        let doc = self.doc_path(path);
        let len = text.as_ref().map(Vec::len);
        let mut docs = self
            .docs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Each document counts its own changes, one per change: the
        // buffer's file-system version is session-wide (requests draw from
        // the same counter), and reporting it made versions jump (`open`
        // 1, a good `update` 3), which an LSP-style client reads as missed
        // edits.
        let version = match text {
            Some(t) => {
                self.set_buffer(&doc, t.into());
                docs.get(&doc).copied().unwrap_or(0) + 1
            }
            None => {
                self.fs.remove(&doc);
                0
            }
        };
        docs.insert(doc.clone(), version);
        drop(docs);
        self.supersede(&doc);
        DocInfo {
            path: doc,
            version,
            len,
        }
    }

    fn set_buffer(&self, doc: &Path, text: Arc<[u8]>) -> u64 {
        let version = self.ids.fetch_add(1, Ordering::Relaxed) + 1;
        self.fs.set(doc, text, version);
        version
    }

    /// Replace a document's text (opening it if needed). Requests still
    /// running on the old text are cancelled: their results are stale.
    pub fn update(&self, path: &Path, text: Vec<u8>) -> DocInfo {
        self.open(path, Some(text))
    }

    /// Apply edits to a document's current text (its buffer, or the file
    /// when it has none). Fails when an edit's range is out of bounds or
    /// splits a UTF-8 character.
    pub fn edit(&self, path: &Path, edits: &[TextEdit]) -> Result<DocInfo, String> {
        let doc = self.doc_path(path);
        let mut text = self
            .fs
            .read(&doc)
            .map_err(|e| format!("cannot read '{}': {e}", doc.display()))?;
        for e in edits {
            if e.start > e.end || e.end > text.len() {
                return Err(format!(
                    "edit {}..{} is outside the text (0..{})",
                    e.start,
                    e.end,
                    text.len()
                ));
            }
            text.splice(e.start..e.end, e.text.bytes());
        }
        if std::str::from_utf8(&text).is_err() && edits.iter().any(|e| !e.text.is_empty()) {
            // Byte offsets inside a character would corrupt the text.
            return Err("the edits split a UTF-8 character".into());
        }
        Ok(self.open(&doc, Some(text)))
    }

    /// Forget a document: its buffer (reads go to the host's file again),
    /// its last products, and any request running on it.
    pub fn close(&self, path: &Path) -> bool {
        let doc = self.doc_path(path);
        self.cancel(&doc);
        self.fs.remove(&doc);
        self.products
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&doc);
        self.take_memo(&doc);
        self.docs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&doc)
            .is_some()
    }

    /// The open documents.
    pub fn documents(&self) -> Vec<DocInfo> {
        let docs = self
            .docs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut v: Vec<DocInfo> = docs
            .iter()
            .map(|(p, &version)| DocInfo {
                path: p.clone(),
                version,
                len: self.fs.buffer(p).map(|b| b.text.len()),
            })
            .collect();
        v.sort_by(|a, b| a.path.cmp(&b.path));
        v
    }

    /// An open document's unsaved text, shared (no copy); `None` for a
    /// path without a buffer. A host that must know exactly which text a
    /// request evaluates takes it here and passes it as [`Run::text`]:
    /// an edit arriving meanwhile then cannot change what the request
    /// reads.
    pub fn buffer_text(&self, path: &Path) -> Option<Arc<[u8]>> {
        self.fs.buffer(&self.doc_path(path)).map(|b| b.text)
    }

    // --- Cancellation ------------------------------------------------------

    /// Stop every request running on `path`. Returns how many there were.
    pub fn cancel(&self, path: &Path) -> usize {
        let doc = self.doc_path(path);
        let jobs = self
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let v = jobs.get(&doc).map_or(&[][..], Vec::as_slice);
        for j in v {
            j.flag.store(true, Ordering::Relaxed);
        }
        v.len()
    }

    /// How many requests are running on `path`.
    pub fn running(&self, path: &Path) -> usize {
        let doc = self.doc_path(path);
        self.jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&doc)
            .map_or(0, Vec::len)
    }

    /// Stop every request running on any document (a host whose client
    /// went away). Returns how many there were.
    pub fn cancel_all(&self) -> usize {
        let jobs = self
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut n = 0;
        for j in jobs.values().flatten() {
            j.flag.store(true, Ordering::Relaxed);
            n += 1;
        }
        n
    }

    /// Stop the superseding requests running on `doc` (see
    /// [`Run::supersede`]).
    fn supersede(&self, doc: &Path) {
        let jobs = self
            .jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for j in jobs.get(doc).into_iter().flatten() {
            if j.supersede {
                j.flag.store(true, Ordering::Relaxed);
            }
        }
    }

    fn begin(&self, doc: &Path, run: &Run) -> JobGuard<'_> {
        self.requests.fetch_add(1, Ordering::Relaxed);
        let supersede = run.supersede;
        if supersede {
            self.supersede(doc);
        }
        let id = self.ids.fetch_add(1, Ordering::Relaxed) + 1;
        let flag = run
            .interrupt
            .clone()
            .unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
        let limits = run.limits.unwrap_or(self.cfg.limits);
        let limits = (!limits.is_none()).then(|| {
            let probe = limits
                .memory
                .zip(self.cfg.memory_probe.clone())
                .map(|(limit, probe)| {
                    memory::request_probe(
                        self.pressure.clone(),
                        probe,
                        self.cfg.memory_release.clone(),
                        self.cfg.clock.clone(),
                        limit,
                    )
                });
            Arc::new(
                eval::limits::Guard::new(limits, flag.clone(), self.cfg.clock.clone())
                    .with_probe(probe),
            )
        });
        self.jobs
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry(doc.to_path_buf())
            .or_default()
            .push(Job {
                id,
                flag: flag.clone(),
                supersede,
            });
        JobGuard {
            session: self,
            doc: doc.to_path_buf(),
            id,
            flag,
            limits,
        }
    }

    /// Why a stage that was interrupted stopped: a limit (reported here
    /// when the geometry stage found it; the evaluator prints its own), or
    /// a cancellation.
    fn interrupted(&self, pipe: &mut Pipe, loaded: &Loaded, job: &JobGuard<'_>) -> Stop {
        let Some(e) = job.exceeded() else {
            return Stop::Cancelled;
        };
        let mut d = Diagnostic::new(DiagCode::ResourceLimit, Severity::Error, e.message())
            .with_hint(e.hint());
        let mut sources = &loaded.program.sources;
        if let Some(at) = e.at
            && let Some(s) = loaded.unit_sources(at.unit)
        {
            d = d
                .at(at.span, at.line)
                .with_base(lang::diag::PathBase::MainFileDir);
            sources = s;
        }
        pipe.con.diagnostic(&d, sources, &pipe.paths.cwd);
        Stop::Exit(EXIT_ERROR)
    }

    fn cancelled(&self) -> Cancelled {
        self.cancelled.fetch_add(1, Ordering::Relaxed);
        Cancelled
    }

    // --- Caches ------------------------------------------------------------

    pub fn stats(&self) -> Stats {
        let renderers = self
            .renderers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut g = geom::CacheStats::default();
        for (_, r) in renderers.iter() {
            let s = r.stats();
            g.entries += s.entries;
            g.bytes += s.bytes;
            g.budget += s.budget;
            g.hits += s.hits;
            g.misses += s.misses;
            g.evictions += s.evictions;
        }
        // Before the first render there is no renderer, and the sum of
        // their budgets is 0 (`serve --status` said "of 0 MiB"): the budget
        // is then the one the first renderer will get.
        g.budget = g.budget.max(self.cfg.geometry_budget);
        Stats {
            documents: self
                .docs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .len(),
            parse: self
                .parse
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .stats(),
            geometry: g,
            renderers: renderers.len(),
            requests: self.requests.load(Ordering::Relaxed),
            cancelled: self.cancelled.load(Ordering::Relaxed),
            running: self
                .jobs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .values()
                .map(Vec::len)
                .sum(),
            lexed: self.lexed.size(),
            fragments: self.fragments.size(),
        }
    }

    /// Change the budgets (bytes), evicting at once if over.
    pub fn set_budgets(&mut self, geometry: usize, parse: usize) {
        self.cfg.geometry_budget = geometry;
        self.cfg.parse_budget = parse;
        self.parse
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_budget(parse);
        for (_, r) in self
            .renderers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
        {
            r.set_budget(geometry);
        }
    }

    /// Drop every cached parse, geometry and product.
    pub fn clear_caches(&self) {
        self.parse
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.lexed.clear();
        self.fragments.clear();
        self.renderers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.fonts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.products
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
        self.memos
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }

    /// A document's statement memo, taken out while a request uses it: a
    /// concurrent request on the same document starts from an empty one
    /// rather than waiting.
    fn take_memo(&self, doc: &Path) -> eval::Memo {
        let mut memos = self
            .memos
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        match memos.iter().position(|(p, _)| p == doc) {
            Some(i) => memos.remove(i).1,
            None => eval::Memo::new(),
        }
    }

    fn put_memo(&self, doc: PathBuf, memo: eval::Memo) {
        let mut memos = self
            .memos
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        memos.retain(|(p, _)| *p != doc);
        memos.push((doc, memo));
        while memos.len() > 1
            && (memos.len() > MEMOS
                || memos.iter().map(|(_, m)| m.bytes()).sum::<usize>() > MEMO_TOTAL)
        {
            memos.remove(0);
        }
    }

    /// The fonts for a program's `use`d files, shared while they are the
    /// same files (their fonts' outlines stay loaded).
    fn fonts_for(&self, used: &[String], fs: &dyn FileSystem) -> (u64, Arc<text::FontDb>) {
        let fonts: Vec<(&String, Option<lang::loader::Metadata>)> = used
            .iter()
            .filter(|u| is_font(u))
            .map(|u| (u, fs.metadata(Path::new(u))))
            .collect();
        let sig = hash_of(format!("{fonts:?}"));
        let mut cache = self
            .fonts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(i) = cache.iter().position(|(k, _)| *k == sig) {
            let e = cache.remove(i);
            cache.push(e.clone());
            return (sig, e.1);
        }
        let db = Arc::new((self.cfg.fonts)(used));
        cache.push((sig, db.clone()));
        if cache.len() > RENDERERS {
            cache.remove(0);
        }
        (sig, db)
    }

    /// The renderer for a colour scheme and font set: geometry keys do not
    /// include the colours meshes are exported in or the fonts `text()`
    /// used, so each combination has its own cache.
    fn renderer_for(&self, scheme: &geom::color::Scheme, fonts: u64) -> (u64, Arc<geom::Renderer>) {
        let key = hash_of((format!("{scheme:?}"), fonts));
        let mut rs = self
            .renderers
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(i) = rs.iter().position(|(k, _)| *k == key) {
            let e = rs.remove(i);
            rs.push(e.clone());
            return (key, e.1);
        }
        let r = Arc::new(geom::Renderer::with_budget(self.cfg.geometry_budget));
        rs.push((key, r.clone()));
        if rs.len() > RENDERERS {
            rs.remove(0);
        }
        (key, r)
    }

    // --- The pipeline --------------------------------------------------------

    fn pipe(&self, run: &Run) -> Pipe {
        let paths = Paths::of(run, &self.cfg);
        Pipe {
            con: Console::new(Vec::new(), paths.main_dir.clone(), run.quiet)
                .record(true)
                .rich(run.rich),
            paths,
            t0: self.now(),
            timings: Timings::default(),
            programs: Vec::new(),
            fs: Arc::new(docfs::Recorder::new(self.fs.clone())),
            inputs: Vec::new(),
        }
    }

    /// The request's messages so far, as [`Session::finish`] will report
    /// them (without the printed bytes).
    fn log_so_far(&self, pipe: &Pipe) -> Log {
        let lines = pipe.con.records().to_vec();
        let names = if wants_names(&lines) {
            Names::of(pipe.programs.iter().map(|p| &**p))
        } else {
            Names::default()
        };
        Log {
            stderr: Vec::new(),
            lines,
            names: Arc::new(names),
        }
    }

    fn finish(&self, mut pipe: Pipe) -> (Log, Timings) {
        let lines = pipe.con.take_records();
        let names = if wants_names(&lines) {
            Names::of(pipe.programs.iter().map(|p| &**p))
        } else {
            Names::default()
        };
        pipe.timings.total = self.now() - pipe.t0;
        (
            Log {
                stderr: pipe.con.into_inner(),
                lines,
                names: Arc::new(names),
            },
            pipe.timings,
        )
    }

    /// Read and parse the input and its libraries, printing their
    /// messages as the command line does (`run::load`).
    fn load(&self, pipe: &mut Pipe, run: &Run) -> Result<Loaded, Stop> {
        run.stage(Stage::Parse);
        let t = self.now();
        let recorder = pipe.fs.clone();
        let (fs, libs) = (&*recorder, &self.cfg.libs);
        let paths = pipe.paths.clone();
        let read = match &run.text {
            Some(t) => Ok(t.to_vec()),
            None => fs.read(&paths.path),
        };
        let Ok(mut text) = read else {
            pipe.con.print_error_line(
                DiagCode::InputNotFound,
                format!("Can't open input file '{}'!\n", paths.display).as_bytes(),
                false,
            );
            return Err(Stop::Exit(EXIT_ERROR));
        };
        // cmdline(): the text, then an end-of-text marker, then each -D.
        let mut suffix = b"\n\x03\n".to_vec();
        for d in &run.defines {
            suffix.extend_from_slice(d.as_bytes());
            suffix.extend_from_slice(b";\n");
        }
        text.extend_from_slice(&suffix);
        let epoch_text = hash_of(&text);
        let program = match &run.entry {
            None => parse::main_program(&self.parse, self.stores(), &paths.path, text, fs, libs),
            Some(entry) => {
                // Not through the parse cache: the program is changed.
                let mut p = lang::parse_program_with(
                    paths.path.clone(),
                    text,
                    fs,
                    libs,
                    self.stores().caches(),
                );
                entry_only(&mut p, entry);
                Arc::new(p)
            }
        };
        for d in program.openscad_diags() {
            pipe.con.diagnostic(d, &program.sources, &paths.cwd);
        }
        if program.has_syntax_errors() {
            pipe.con.print(
                None,
                format!("Can't parse file '{}'!\n", paths.display).as_bytes(),
            );
            pipe.programs = vec![program.clone()];
            return Err(Stop::Exit(EXIT_ERROR));
        }
        let libraries = parse::libraries(&self.parse, self.stores(), &program, &suffix, fs, libs);
        for lib in &libraries {
            match (&lib.program, lib.open_error()) {
                (Some(p), _) => {
                    for d in p.openscad_diags() {
                        pipe.con.diagnostic(d, &p.sources, &paths.cwd);
                    }
                }
                (None, Some(msg)) => pipe.con.print(Some(Severity::Warning), msg.as_bytes()),
                (None, None) => {}
            }
        }
        let uses = lang::deps::resolve_uses(&program, fs, libs);
        // The sources as the parse cache sees them: the main text, and every
        // other file by its metadata.
        let mut h = DefaultHasher::new();
        epoch_text.hash(&mut h);
        for p in
            std::iter::once(&*program).chain(libraries.iter().filter_map(|l| l.program.as_deref()))
        {
            for (_, f) in p.sources.iter().skip(1) {
                f.path.hash(&mut h);
                format!("{:?}", fs.metadata(&f.path)).hash(&mut h);
            }
            p.sources.path(p.main).hash(&mut h);
        }
        for l in &libraries {
            l.path.hash(&mut h);
            if let Some(p) = &l.program {
                format!("{:?}", fs.metadata(p.sources.path(p.main))).hash(&mut h);
            }
        }
        let loaded = Loaded {
            program,
            libs: libraries,
            uses,
            epoch: h.finish(),
        };
        pipe.programs = std::iter::once(loaded.program.clone())
            .chain(loaded.libs.iter().filter_map(|l| l.program.clone()))
            .collect();
        pipe.timings.parse = self.now() - t;
        Ok(loaded)
    }

    fn evaluate_loaded(
        &self,
        pipe: &mut Pipe,
        loaded: &Loaded,
        run: &Run,
        preview: bool,
        job: &JobGuard<'_>,
    ) -> Result<eval::Evaluation, Stop> {
        run.stage(Stage::Evaluate);
        let t = self.now();
        let libs: Vec<eval::Library<'_>> = loaded
            .libs
            .iter()
            .map(|lib| eval::Library {
                path: &lib.path,
                program: lib.program.as_deref(),
                uses: &lib.uses,
            })
            .collect();
        let features = run.features.union(self.cfg.features);
        // `textmetrics()` measures with the fonts `text()` renders with.
        let fonts = features
            .has(eval::Feature::TextMetrics)
            .then(|| self.fonts_for(&loaded.used(), &*pipe.fs).1);
        let options = eval::Options {
            preview,
            camera: run.camera,
            rng_seed: run.rng_seed.unwrap_or(self.cfg.rng_seed),
            fs: pipe.fs.clone(),
            interrupt: Some(job.flag.clone()),
            guard: job.limits.clone(),
            parts: run.parts || self.cfg.parts,
            features,
            fonts,
            ..eval::Options::default()
        };
        let ev = if self.cfg.reuse_evaluation {
            let doc = pipe.paths.doc.clone();
            let mut memo = self.take_memo(&doc);
            let ev = eval::evaluate_incremental(
                &loaded.program,
                &loaded.uses,
                &libs,
                pipe.paths.main_dir.clone(),
                &options,
                &mut pipe.con,
                &mut memo,
            );
            self.put_memo(doc, memo);
            ev
        } else {
            eval::evaluate(
                &loaded.program,
                &loaded.uses,
                &libs,
                pipe.paths.main_dir.clone(),
                &options,
                &mut pipe.con,
            )
        };
        pipe.timings.evaluate = self.now() - t;
        // The evaluator printed the limit where it was passed.
        if job.exceeded().is_some() {
            return Err(Stop::Exit(EXIT_ERROR));
        }
        if ev.interrupted || job.stopped() {
            return Err(Stop::Cancelled);
        }
        let top = ev.root.find_root_tag().0.unwrap_or(&ev.root);
        usehint::report(
            &mut pipe.con,
            top,
            loaded.libs.len(),
            &|u| loaded.unit_program(u),
            &pipe.paths.cwd,
        );
        Ok(ev)
    }

    /// Geometry messages as the command line prints them
    /// (`run::print_messages`).
    fn print_messages(&self, pipe: &mut Pipe, loaded: &Loaded, messages: &[geom::Msg]) {
        for m in messages {
            let Some(severity) = m.severity else {
                pipe.con.print(None, m.text.as_bytes());
                continue;
            };
            let mut d = Diagnostic::new(DiagCode::Geometry, severity, m.text.clone());
            let mut sources = &loaded.program.sources;
            if let Some(l) = &m.loc
                && let Some(s) = loaded.unit_sources(l.unit)
            {
                d = d.at(l.span, l.line).with_base(l.base);
                sources = s;
            }
            pipe.con.diagnostic(&d, sources, &pipe.paths.cwd);
        }
    }

    /// Look at the model's polyhedra and imported meshes (`orient`) and
    /// log each problem as a NeoSCAD-only warning at the call that made
    /// the mesh, with a fix, and an exact edit when the faces are written
    /// out. `keys` (when the tree has them) lets a mesh's analysis be
    /// reused from an earlier request.
    fn report_inputs(
        &self,
        pipe: &mut Pipe,
        loaded: &Loaded,
        top: &eval::Node,
        import_mesh: &dyn Fn(&eval::Node) -> Option<Arc<geom::polyset::PolySet>>,
        keys: Option<&eval::dump::Keys>,
    ) {
        let mut memo = |n: &eval::Node, f: &dyn Fn() -> Option<orient::MeshIssues>| {
            let Some(keys) = keys else {
                return Arc::new(f());
            };
            let k = keys.get(n);
            let hit = self
                .orient
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, r)| r.clone());
            if let Some(r) = hit {
                return r;
            }
            let r = Arc::new(f());
            let mut m = self
                .orient
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if m.len() == ORIENT_MEMO {
                m.pop_front();
            }
            m.push_back((k, r.clone()));
            r
        };
        pipe.inputs = orient::report(
            &mut pipe.con,
            top,
            import_mesh,
            &mut memo,
            &|u| loaded.unit_program(u),
            &pipe.paths.cwd,
        );
    }

    /// Build the geometry (or the preview's products) of an evaluated
    /// program, reusing the document's last products when the tree, its
    /// sources and the renderer are the same.
    #[allow(clippy::too_many_arguments)]
    fn build(
        &self,
        pipe: &mut Pipe,
        loaded: &Loaded,
        ev: &eval::Evaluation,
        mode: Mode,
        scheme: &geom::color::Scheme,
        job: &JobGuard<'_>,
        csg_limit: usize,
    ) -> Result<(Product, geom::CacheStats), Stop> {
        let t = self.now();
        let top = ev.root.find_root_tag().0.unwrap_or(&ev.root);
        let keys = eval::dump::Keys::new(&ev.root, &*pipe.fs);
        let (font_sig, fonts) = self.fonts_for(&loaded.used(), &*pipe.fs);
        let (rkey, renderer) = self.renderer_for(scheme, font_sig);
        // The limits are part of the key: a product built under looser
        // limits is not reused under tighter ones, so the renderer's
        // cache checks each subtree's demand against them (see
        // `geom::evaluate`'s `Demand`) and a warm request answers as a
        // cold one would.
        let limits = hash_of(format!("{:?}", job.limits.as_ref().map(|g| *g.limits())));
        // The root's key as the renderer's cache has it, not `keys.get`:
        // that one is shared by `group() { group(); X }` and `X`, whose
        // 2D results differ, and the epoch does not tell them apart when
        // one file's entry modules (`Run::entry`) are rendered in turn.
        let root = geom::result_key(top, &keys);
        let key = (root, loaded.epoch, mode, rkey, csg_limit, limits);
        let doc = pipe.paths.doc.clone();
        let reuse = self
            .products
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&doc)
            .filter(|p| p.key == key)
            .cloned();
        let product = match reuse {
            Some(p) => p,
            None => {
                let opts = geom::RenderOptions {
                    scheme: *scheme,
                    force: mode == Mode::Force,
                    fs: pipe.fs.clone(),
                    work_dir: pipe.paths.cwd.clone(),
                    fonts,
                    interrupt: Some(job.flag.clone()),
                    guard: job.limits.clone(),
                    // Every request prints what a fresh command-line run
                    // would, however warm the cache.
                    replay: Some(loaded.epoch),
                };
                let built = if mode == Mode::Preview {
                    geom::csg::CsgTree::build(top, &renderer, &keys, opts, csg_limit).map(|t| {
                        let messages = t.messages.clone();
                        Product {
                            key,
                            geometry: None,
                            tree: Some(Arc::new(t)),
                            messages,
                        }
                    })
                } else {
                    renderer.render(top, &keys, opts).map(|r| Product {
                        key,
                        geometry: r.geometry,
                        tree: None,
                        messages: r.messages,
                    })
                };
                match built {
                    Ok(p) => {
                        self.products
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .insert(doc, p.clone());
                        p
                    }
                    Err(u) if u.is_interrupted() => {
                        return Err(self.interrupted(pipe, loaded, job));
                    }
                    Err(u) => {
                        let mut line = format!("neoscad: {}() is not implemented yet", u.what);
                        if let Some(l) = &u.loc
                            && let Some(sources) = loaded.unit_sources(l.unit)
                        {
                            let rel = lang::diag::relative_display(
                                sources.path(l.span.file),
                                &pipe.paths.main_dir,
                            );
                            line.push_str(&format!(" (in file {rel}, line {})", l.line));
                        }
                        pipe.con.print_unfiltered(line.as_bytes());
                        return Err(Stop::Exit(EXIT_NOT_IMPLEMENTED));
                    }
                }
            }
        };
        if job.stopped() {
            return Err(self.interrupted(pipe, loaded, job));
        }
        self.print_messages(pipe, loaded, &product.messages);
        // Imported meshes are looked at in the form the render read them,
        // from the cache it just filled; an entry already evicted is
        // skipped rather than read again.
        let import_mesh = |n: &eval::Node| match renderer.cached_leaf(n, &keys) {
            Some(geom::Geometry::PolySet(ps)) => Some(ps),
            _ => None,
        };
        self.report_inputs(pipe, loaded, top, &import_mesh, Some(&keys));
        pipe.timings.geometry = self.now() - t;
        Ok((product, renderer.stats()))
    }

    // --- Operations --------------------------------------------------------

    /// Parse only: the program printed back as the `.ast` export writes it
    /// (`None` when it does not parse), with the parser's messages.
    pub fn ast(&self, run: &Run) -> Result<(Option<String>, Log), Cancelled> {
        eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
            let mut pipe = self.pipe(run);
            let _job = self.begin(&pipe.paths.doc, run);
            let text = match self.load(&mut pipe, run) {
                Ok(loaded) => Some(
                    String::from_utf8_lossy(&lang::dump::dump(&loaded.program.ast)).into_owned(),
                ),
                Err(Stop::Cancelled) => return Err(self.cancelled()),
                Err(Stop::Exit(_)) => None,
            };
            Ok((text, self.finish(pipe).0))
        })
    }

    /// Parse and evaluate: diagnostics, `echo()` output and, with `csg`,
    /// the node tree as a `.csg` file.
    pub fn evaluate(&self, run: &Run, csg: bool) -> Result<Evaluated, Cancelled> {
        eval::with_stack(eval::DEFAULT_THREAD_STACK, || self.evaluate_now(run, csg))
    }

    fn evaluate_now(&self, run: &Run, csg: bool) -> Result<Evaluated, Cancelled> {
        let mut pipe = self.pipe(run);
        let job = self.begin(&pipe.paths.doc, run);
        let (exit_code, tree, aborted) = match self.load(&mut pipe, run) {
            Err(Stop::Cancelled) => return Err(self.cancelled()),
            Err(Stop::Exit(c)) => (c, None, false),
            Ok(loaded) => match self.evaluate_loaded(&mut pipe, &loaded, run, true, &job) {
                Err(Stop::Cancelled) => return Err(self.cancelled()),
                Err(Stop::Exit(c)) => (c, None, false),
                Ok(ev) => {
                    let top = ev.root.find_root_tag().0.unwrap_or(&ev.root);
                    // Polyhedra only: imported meshes are read when
                    // geometry is built.
                    self.report_inputs(&mut pipe, &loaded, top, &|_| None, None);
                    let tree = csg.then(|| eval::dump::csg(top, &pipe.paths.main_dir, &*pipe.fs));
                    (0, tree, ev.aborted)
                }
            },
        };
        let files = pipe.fs.files();
        let (log, timings) = self.finish(pipe);
        Ok(Evaluated {
            exit_code,
            log,
            csg: tree,
            aborted,
            timings,
            files,
        })
    }

    /// Evaluate and build: the full geometry, or with [`Mode::Preview`] the
    /// CSG products.
    pub fn render(
        &self,
        run: &Run,
        mode: Mode,
        scheme: &render::ColorScheme,
    ) -> Result<Rendered, Cancelled> {
        eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
            self.render_now(run, mode, scheme, geom::csg::DEFAULT_TERM_LIMIT)
        })
    }

    /// [`Session::render`] with the preview's CSG term limit
    /// (`--csglimit`; `geom::csg::DEFAULT_TERM_LIMIT` otherwise).
    pub fn render_with_limit(
        &self,
        run: &Run,
        mode: Mode,
        scheme: &render::ColorScheme,
        csg_limit: usize,
    ) -> Result<Rendered, Cancelled> {
        eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
            self.render_now(run, mode, scheme, csg_limit)
        })
    }

    fn render_now(
        &self,
        run: &Run,
        mode: Mode,
        scheme: &render::ColorScheme,
        csg_limit: usize,
    ) -> Result<Rendered, Cancelled> {
        self.render_impl(run, mode, scheme, csg_limit, false)
            .map(|(r, _)| r)
    }

    /// [`Session::render`] in [`Mode::Render`], with the solid of each
    /// `part()` in the model (none without `--enable part`, or when the
    /// model has no parts): what `check`, `measure` and part snapshots
    /// work from.
    pub fn render_parts(
        &self,
        run: &Run,
        scheme: &render::ColorScheme,
    ) -> Result<(Rendered, Vec<parts::Part>), Cancelled> {
        eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
            self.render_impl(
                run,
                Mode::Render,
                scheme,
                geom::csg::DEFAULT_TERM_LIMIT,
                true,
            )
        })
    }

    fn render_impl(
        &self,
        run: &Run,
        mode: Mode,
        scheme: &render::ColorScheme,
        csg_limit: usize,
        want_parts: bool,
    ) -> Result<(Rendered, Vec<parts::Part>), Cancelled> {
        let mut parts = Vec::new();
        let mut pipe = self.pipe(run);
        let job = self.begin(&pipe.paths.doc, run);
        let mut out = Rendered {
            exit_code: 0,
            log: Log::default(),
            geometry: None,
            tree: None,
            stop: geom::csg::Stop {
                interrupt: Some(job.flag.clone()),
                guard: job.limits.clone(),
            },
            camera: run.camera,
            camera_assigned: eval::CameraAssigned::default(),
            cache_entries: 0,
            cache_bytes: 0,
            cache_budget: 0,
            timings: Timings::default(),
            files: Vec::new(),
            inputs: Vec::new(),
        };
        let step = (|| {
            let loaded = self.load(&mut pipe, run)?;
            let ev = self.evaluate_loaded(&mut pipe, &loaded, run, mode == Mode::Preview, &job)?;
            if let Some(hook) = &run.on_evaluated {
                hook(&self.log_so_far(&pipe));
            }
            out.camera = ev.camera;
            out.camera_assigned = ev.camera_assigned;
            run.stage(Stage::Geometry);
            let (p, cache) = self.build(
                &mut pipe,
                &loaded,
                &ev,
                mode,
                &scheme.geometry_scheme(),
                &job,
                csg_limit,
            )?;
            out.geometry = p.geometry.filter(|g| !g.is_empty());
            out.tree = p.tree;
            out.cache_entries = cache.entries;
            out.cache_bytes = cache.bytes;
            out.cache_budget = cache.budget;
            if want_parts {
                let top = ev.root.find_root_tag().0.unwrap_or(&ev.root);
                parts =
                    self.part_solids(&mut pipe, &loaded, top, &scheme.geometry_scheme(), &job)?;
            }
            Ok(())
        })();
        match step {
            Err(Stop::Cancelled) => return Err(self.cancelled()),
            Err(Stop::Exit(c)) => out.exit_code = c,
            Ok(()) => {}
        }
        out.files = pipe.fs.files();
        out.inputs = std::mem::take(&mut pipe.inputs);
        (out.log, out.timings) = self.finish(pipe);
        Ok((out, parts))
    }

    /// The solid of every part under `top`, each on its own and placed as
    /// the model places it ([`parts::Part`]). Each part node is rendered
    /// through the same renderer as the model, so its geometry comes from
    /// the cache the render just filled.
    fn part_solids(
        &self,
        pipe: &mut Pipe,
        loaded: &Loaded,
        top: &eval::Node,
        scheme: &geom::color::Scheme,
        job: &JobGuard<'_>,
    ) -> Result<Vec<parts::Part>, Stop> {
        let found = parts::find(top);
        if found.is_empty() {
            return Ok(Vec::new());
        }
        let keys = eval::dump::Keys::new(top, &*pipe.fs);
        let (font_sig, fonts) = self.fonts_for(&loaded.used(), &*pipe.fs);
        let (_, renderer) = self.renderer_for(scheme, font_sig);
        let opts = geom::RenderOptions {
            scheme: *scheme,
            force: false,
            fs: pipe.fs.clone(),
            work_dir: pipe.paths.cwd.clone(),
            fonts,
            interrupt: Some(job.flag.clone()),
            guard: job.limits.clone(),
            // The model's render printed every message already.
            replay: None,
        };
        let nodes: Vec<&eval::Node> = found.iter().map(|f| f.node).collect();
        let built = match renderer.render_many(&nodes, &keys, opts) {
            Ok(b) => b,
            Err(u) if u.is_interrupted() => return Err(self.interrupted(pipe, loaded, job)),
            // The model rendered, so its parts do too; a part that somehow
            // does not is left out rather than failing the request.
            Err(_) => return Ok(Vec::new()),
        };
        if job.stopped() {
            return Err(self.interrupted(pipe, loaded, job));
        }
        Ok(parts::assemble(&found, built))
    }

    /// Evaluate, render and export to each output in turn, printing what
    /// the command line prints (`run::export_mesh` without animation): the
    /// same checks, messages and bytes. The files go to `sink`.
    pub fn export(
        &self,
        req: &ExportRequest,
        sink: &mut (dyn ExportSink + Send),
    ) -> Result<Exported, Cancelled> {
        eval::with_stack(eval::DEFAULT_THREAD_STACK, || self.export_now(req, sink))
    }

    fn export_now(
        &self,
        req: &ExportRequest,
        sink: &mut (dyn ExportSink + Send),
    ) -> Result<Exported, Cancelled> {
        let run = &req.run;
        let mut pipe = self.pipe(run);
        let job = self.begin(&pipe.paths.doc, run);
        let mut geometry = None;
        let step = (|| -> Result<u8, Stop> {
            let loaded = self.load(&mut pipe, run)?;
            let ev = self.evaluate_loaded(&mut pipe, &loaded, run, false, &job)?;
            let started = self.now();
            let scheme = req.scheme.geometry_scheme();
            let mode = if req.force { Mode::Force } else { Mode::Render };
            run.stage(Stage::Geometry);
            let (p, cache) = self.build(
                &mut pipe,
                &loaded,
                &ev,
                mode,
                &scheme,
                &job,
                geom::csg::DEFAULT_TERM_LIMIT,
            )?;
            let root = p.geometry;
            let dim = root.as_ref().map_or(3, geom::Geometry::dimension);
            if req.force && dim == 3 {
                pipe.con
                    .print(None, b"Converted to backend-specific geometry");
            }
            // `predictible-output` comes from the request's features, like
            // every other experimental feature, rather than from the
            // host's encoder settings: otherwise a host that forgot to
            // copy the flag across would export unsorted meshes while
            // reporting the feature as on.
            let mut settings = req.settings.clone();
            settings.predictible_output = run
                .features
                .union(self.cfg.features)
                .has(eval::Feature::PredictibleOutput);
            let mut mesh = None;
            for (target, format) in &req.outputs {
                // `checkAndExport`, per output: the dimension, then
                // emptiness.
                let want = format.dimension();
                if dim != want {
                    pipe.con.print(
                        None,
                        format!("Current top level object is not a {want}D object.").as_bytes(),
                    );
                    return Err(Stop::Exit(EXIT_ERROR));
                }
                let Some(root) = root.as_ref().filter(|g| !g.is_empty()) else {
                    pipe.con.print(None, b"Current top level object is empty.");
                    return Err(Stop::Exit(EXIT_ERROR));
                };
                let enc = export::encode(*format, root, &settings, &mut mesh);
                for (severity, line) in &enc.immediate {
                    pipe.con.print(*severity, line.as_bytes());
                }
                for w in &enc.warnings {
                    pipe.con
                        .print(Some(Severity::Warning), format!("WARNING: {w}").as_bytes());
                }
                if let Err(line) = sink.write(target, &enc.data) {
                    pipe.con
                        .print_error_line(DiagCode::OutputNotWritable, line.as_bytes(), true);
                    return Err(Stop::Exit(EXIT_ERROR));
                }
            }
            let facts = SummaryFacts {
                cache_entries: cache.entries,
                cache_bytes: cache.bytes,
                cache_budget: cache.budget,
                elapsed_ms: self.now() - started,
                geometry: root.as_ref(),
                camera: &ev.camera,
            };
            let ok = sink.summary(&facts, &mut pipe.con);
            geometry = root.clone();
            Ok(if ok { 0 } else { EXIT_ERROR })
        })();
        let exit_code = match step {
            Err(Stop::Cancelled) => return Err(self.cancelled()),
            Err(Stop::Exit(c)) => c,
            Ok(c) => c,
        };
        let (log, timings) = self.finish(pipe);
        Ok(Exported {
            exit_code,
            log,
            geometry: geometry.filter(|g| !g.is_empty()),
            timings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lang::vfs::MemFs;

    fn session(files: &[(&str, &str)]) -> Session {
        let fs = Arc::new(MemFs::new());
        for (p, t) in files {
            fs.insert(p, t.as_bytes().to_vec());
        }
        let mut cfg = Config::new(fs, LibraryPath::default());
        cfg.work_dir = PathBuf::from("/doc");
        Session::new(cfg)
    }

    #[test]
    fn edits_apply_in_order() {
        let s = session(&[("/doc/a.scad", "cube(1);")]);
        let d = s
            .edit(
                Path::new("a.scad"),
                &[
                    TextEdit {
                        start: 5,
                        end: 6,
                        text: "22".into(),
                    },
                    TextEdit {
                        start: 0,
                        end: 0,
                        text: "// x\n".into(),
                    },
                ],
            )
            .unwrap();
        assert_eq!(d.path, PathBuf::from("/doc/a.scad"));
        assert_eq!(
            s.fs.read(Path::new("/doc/a.scad")).unwrap(),
            b"// x\ncube(22);"
        );
        assert!(
            s.edit(
                Path::new("a.scad"),
                &[TextEdit {
                    start: 3,
                    end: 99,
                    text: String::new()
                }]
            )
            .is_err()
        );
    }

    #[test]
    fn diagnostics_carry_spans_codes_and_hints() {
        let s = session(&[("/doc/a.scad", "cube(1);\ncub(2);\n")]);
        let r = s.evaluate(&Run::new("a.scad"), false).unwrap();
        let d = r.log.diagnostics_json();
        assert_eq!(d.len(), 1, "{d:?}");
        assert_eq!(d[0]["code"], "unknown-module");
        assert_eq!(d[0]["line"], 2);
        assert_eq!(d[0]["span"]["start"]["column"], 1);
        assert_eq!(
            d[0]["text"],
            "WARNING: Ignoring unknown module 'cub' in file a.scad, line 2"
        );
        assert_eq!(d[0]["hints"][0]["message"], "did you mean 'cube'?");
    }
}
