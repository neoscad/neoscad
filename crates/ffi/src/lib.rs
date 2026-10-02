//! The NeoSCAD core for the macOS app: a [`session::Session`] behind a
//! small UniFFI API (`docs/audits/macos-prep.md`, steps 8b, 8c and 8e).
//! Swift sees three objects: `Core`, whose methods mirror the session's
//! operations (documents: `open`, `update`, `edit`, `close`; `evaluate`,
//! `render`, `render_into`, `snapshot`, `picture`, `export`, `cancel` and
//! `set_limits`; the panels' `check`, `measure`, `export_file` and
//! `snapshot_file`, in `inspect.rs`), [`Viewport`], a document window's 3D
//! view, and [`LanguageServer`], its editor's language server
//! (`language.rs`). A [`Measurement`] keeps a measured model's solids for
//! sections and distances, and a [`CancelToken`] stops one panel request.
//!
//! # Rules of the bridge
//!
//! - **Every export returns `Result<_, CoreError>`.** UniFFI generates a
//!   non-throwing Swift function as `try! rustCall(...)`, so a panic that
//!   reached it would trap the app and lose unsaved work.
//! - **Every export runs inside [`guarded`]** (`catch_unwind`), which turns
//!   a panic into [`CoreError::Panicked`]. UniFFI catches panics too, but
//!   only as an untyped internal error; catching here gives the app a
//!   message and keeps the session's own state consistent, as
//!   `neoscad serve` does for a request that panics (`serve.rs`,
//!   `guarded`). The session's locks tolerate poisoning, so the next call
//!   runs normally.
//! - **Meshes never cross.** Results carry statistics, diagnostics and at
//!   most a PNG. The viewport ([`Viewport`]) draws from the session inside
//!   Rust: `render_into` puts a render's scene straight onto the GPU, and
//!   Swift only passes the `CAMetalLayer` in and pointer movements.
//! - **One raw pointer.** The layer is the only pointer that crosses, and
//!   `layer.rs` is the only module allowed `unsafe` (see its documentation
//!   for why that use is sound).
//! - **Calls block.** Evaluation and rendering take milliseconds to
//!   minutes; Swift calls them off the main actor (`NeoSCADCore`'s async
//!   wrapper), and `cancel` from any thread stops them.

mod controller;
mod document;
mod host;
mod inspect;
mod language;
mod layer;
/// The process's measured memory, for the memory limit: the command
/// line's module, compiled here too so both hosts measure alike.
#[path = "../../cli/src/memory.rs"]
mod memory;
mod picture;
mod shared;
mod types;
mod update;
mod viewport;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use session::{Run, Session};

pub use controller::*;
pub use document::*;
pub use inspect::*;
pub use language::*;
pub use picture::*;
pub use shared::*;
pub use types::*;
pub use update::*;
pub use viewport::*;

uniffi::setup_scaffolding!();

/// The core allocates through mimalloc, as `neoscad` does (see
/// `crates/cli/src/main.rs`): evaluation and meshing are 7-15% faster than
/// on the system allocator. It is the static library's allocator, so it
/// serves only the core's Rust code; Swift and the system frameworks keep
/// the system malloc. Nothing allocated on one side is freed on the other:
/// UniFFI hands buffers back to Rust to free. It also made the app's
/// `MallocLargeCache=0` launch environment moot (`apple/project.yml`): the
/// core's large buffers no longer pass through the system allocator. The
/// Windows app's DLL is the same: .NET frees nothing the core allocated.
/// Off only for cross-checking the Windows build (Cargo.toml).
#[cfg(feature = "mimalloc")]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// Run `f`, turning a panic into [`CoreError::Panicked`] with the panic's
/// message (see the crate documentation).
fn guarded<T>(f: impl FnOnce() -> Result<T, CoreError>) -> Result<T, CoreError> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(r) => r,
        Err(payload) => {
            let message = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            Err(CoreError::Panicked { message })
        }
    }
}

/// A value that is freed on a thread with the evaluator's stack.
///
/// Parsed documents (the session's caches, a language server's documents)
/// are trees as deep as the source is nested, and freeing one recurses on
/// that depth. The app releases its objects from whatever thread it is on,
/// often a dispatch queue (512 KiB): a release build freeing a language
/// server with a document at the parser's nesting limit (5,000 levels)
/// needed between 256 and 512 KiB, and a test build overflowed 512 KiB at
/// its 2,500.
pub(crate) struct FreedDeep<T: Send>(Option<T>);

impl<T: Send> FreedDeep<T> {
    pub(crate) fn new(value: T) -> FreedDeep<T> {
        FreedDeep(Some(value))
    }
}

impl<T: Send + std::fmt::Debug> std::fmt::Debug for FreedDeep<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl<T: Send> std::ops::Deref for FreedDeep<T> {
    type Target = T;
    fn deref(&self) -> &T {
        self.0.as_ref().expect("freed only on drop")
    }
}

impl<T: Send> Drop for FreedDeep<T> {
    fn drop(&mut self) {
        if let Some(value) = self.0.take() {
            // Not `eval::with_stack`, which panics (here: aborts) when no
            // thread can be made: then the value is freed on this one, as
            // before.
            std::thread::scope(|s| {
                let _ = std::thread::Builder::new()
                    .stack_size(eval::DEFAULT_THREAD_STACK)
                    .spawn_scoped(s, move || drop(value));
            });
        }
    }
}

/// The version of the core (the workspace's), for the About panel and bug
/// reports.
#[uniffi::export]
pub fn core_version() -> Result<String, CoreError> {
    guarded(|| Ok(env!("CARGO_PKG_VERSION").to_string()))
}

/// The limits a new core starts with: [`session::Limits::AGENT`].
#[uniffi::export]
pub fn default_limits() -> Result<ResourceLimits, CoreError> {
    guarded(|| Ok(session::Limits::AGENT.into()))
}

/// One session: documents with their unsaved text, warm parse and geometry
/// caches, and the limits every request runs under (a [`client::Client`],
/// which holds what the web worker shares). The app keeps one for the
/// whole process, so every window shares the caches.
#[derive(uniffi::Object)]
pub struct Core {
    client: FreedDeep<client::Client>,
    test_hooks: bool,
    /// Analysed library files, shared by every window's language server.
    lsp_cache: FreedDeep<Arc<lsp::Cache>>,
}

impl std::fmt::Debug for Core {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Core")
            .field("client", &self.client)
            .finish_non_exhaustive()
    }
}

impl Core {
    fn session(&self) -> &Session {
        &self.client.session
    }

    /// A request on `path` ([`client::Client::run`]): named by its
    /// absolute path, in its directory, superseding older requests on the
    /// same document, under the core's current limits.
    fn run(&self, path: &str) -> Result<Run, CoreError> {
        self.client.run(path)
    }

    /// `path` made absolute ([`client::Client::doc_path`]).
    fn doc_path(&self, path: &str) -> Result<PathBuf, CoreError> {
        self.client.doc_path(path)
    }
}

#[uniffi::export]
impl Core {
    /// A core configured for the app ([`host::config`]), with the agent
    /// limits.
    #[uniffi::constructor]
    pub fn new(config: CoreConfig) -> Result<Arc<Core>, CoreError> {
        guarded(|| {
            let cfg = host::config(config.resource_dir.as_deref());
            Ok(Arc::new(Core {
                client: FreedDeep::new(client::Client::new(cfg)),
                test_hooks: config.test_hooks,
                lsp_cache: FreedDeep::new(Arc::new(lsp::Cache::new())),
            }))
        })
    }

    // --- Documents ----------------------------------------------------------

    /// Open a document: with `text`, as an unsaved buffer that every read
    /// of its path sees (includes of it too); without, tracked but read
    /// from disk.
    pub fn open(&self, path: String, text: Option<String>) -> Result<DocInfo, CoreError> {
        guarded(|| self.client.open(&path, text))
    }

    /// Replace a document's whole text (opening it if needed). Requests
    /// still running on the old text are cancelled.
    pub fn update(&self, path: String, text: String) -> Result<DocInfo, CoreError> {
        guarded(|| self.client.update(&path, text))
    }

    /// Apply edits (UTF-8 byte offsets) to a document's current text.
    pub fn edit(&self, path: String, edits: Vec<TextEdit>) -> Result<DocInfo, CoreError> {
        guarded(|| self.client.edit(&path, edits))
    }

    /// Forget a document: its buffer, its last products and any request
    /// running on it. Whether it was open.
    pub fn close(&self, path: String) -> Result<bool, CoreError> {
        guarded(|| self.client.close(&path))
    }

    /// Stop every request running on a document. How many there were.
    pub fn cancel(&self, path: String) -> Result<u32, CoreError> {
        guarded(|| self.client.cancel(&path))
    }

    // --- Limits -------------------------------------------------------------

    /// Replace the limits of every later request (running ones keep
    /// theirs).
    pub fn set_limits(&self, limits: ResourceLimits) -> Result<(), CoreError> {
        guarded(|| self.client.set_limits(limits))
    }

    pub fn limits(&self) -> Result<ResourceLimits, CoreError> {
        guarded(|| Ok(self.client.limits()))
    }

    // --- Operations ---------------------------------------------------------

    /// Parse and evaluate: diagnostics and `echo()` output.
    pub fn evaluate(&self, path: String) -> Result<Evaluation, CoreError> {
        guarded(|| self.client.evaluate(&path))
    }

    /// Evaluate and build: geometry statistics and diagnostics.
    pub fn render(&self, path: String, mode: RenderMode) -> Result<RenderResult, CoreError> {
        guarded(|| self.client.render(&path, mode))
    }

    /// A contact-sheet PNG of the model (`neoscad snapshot`).
    pub fn snapshot(
        &self,
        path: String,
        options: SnapshotOptions,
    ) -> Result<SnapshotResult, CoreError> {
        guarded(|| {
            let run = self.run(&path)?;
            let name = Path::new(&run.input)
                .with_extension("png")
                .to_string_lossy()
                .into_owned();
            let mut req = session::snapshot::SnapshotRequest::new(run, name);
            req.size = (options.width, options.height);
            req.views = options.views;
            req.dims = options.dims;
            req.preview = options.preview;
            let s = self.session().snapshot(&req).map_err(|e| match e {
                session::snapshot::SnapshotError::Cancelled => CoreError::Cancelled,
                session::snapshot::SnapshotError::Failed(message) => CoreError::Failed { message },
            })?;
            Ok(SnapshotResult {
                exit_code: s.exit_code,
                png: s.png,
                summary_json: s.summary.to_string(),
                diagnostics: types::diagnostics(&s.log),
                console: types::console(&s.log),
            })
        })
    }

    /// Render and write the model to `output` (an absolute path) as
    /// `format` (OpenSCAD's id: `stl`, `binstl`, `off`, `obj`, `3mf`,
    /// `wrl`, `pov`, `svg`, `dxf`, `pdf`), or by `output`'s extension.
    /// The file's bytes are the command line's for the same model.
    pub fn export(
        &self,
        path: String,
        output: String,
        format: Option<String>,
    ) -> Result<ExportResult, CoreError> {
        guarded(|| {
            let run = self.run(&path)?;
            let target = PathBuf::from(&output);
            if !target.is_absolute() {
                return Err(CoreError::InvalidArgument {
                    message: format!("the output path must be absolute (got '{output}')"),
                });
            }
            let fmt = client::export_format(format.as_deref(), &output)?;
            let mut sink = Files { bytes: 0 };
            let mut r = self.client.export(
                run,
                &output,
                fmt,
                &client::ExportOptions::default(),
                iso8601_now(),
                &mut sink,
            )?;
            r.bytes = sink.bytes;
            Ok(r)
        })
    }

    /// Panics, when the core was made with `test_hooks`: how the Swift
    /// tests prove that a panic comes back as [`CoreError::Panicked`] and
    /// leaves the core usable. Without `test_hooks` it is an error, so
    /// the app cannot trip it by accident.
    pub fn debug_panic(&self) -> Result<(), CoreError> {
        guarded(|| {
            if !self.test_hooks {
                return Err(CoreError::InvalidArgument {
                    message: "debug_panic needs a core made with test_hooks".into(),
                });
            }
            panic!("debug_panic requested");
        })
    }
}

/// What `render` (and `render_into`) report about a finished render.
fn render_result(r: &session::Rendered, scheme: &render::ColorScheme) -> RenderResult {
    client::render_result(r, scheme)
}

/// `YYYY-MM-DDTHH:MM:SSZ` now, for the dates PDF and 3MF files record
/// (Howard Hinnant's `civil_from_days`, as the command line computes it).
pub(crate) fn iso8601_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs()) as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// Writes an export's one output to disk.
struct Files {
    bytes: u64,
}

impl session::ExportSink for Files {
    fn write(&mut self, target: &str, data: &[u8]) -> Result<(), String> {
        self.bytes = data.len() as u64;
        std::fs::write(target, data).map_err(|e| format!("ERROR: Can't write to '{target}': {e}"))
    }

    /// The app shows statistics from the result, not the command line's
    /// summary lines, so there is nothing to print.
    fn summary(&mut self, _: &session::SummaryFacts<'_>, _: &mut eval::Console<Vec<u8>>) -> bool {
        true
    }
}

#[cfg(test)]
mod tests;
