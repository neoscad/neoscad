//! The NeoSCAD core for the macOS app: a [`session::Session`] behind a
//! small UniFFI API (`docs/audits/macos-prep.md`, steps 8b, 8c and 8e).
//! Swift sees three objects: `Core`, whose methods mirror the session's
//! operations (documents: `open`, `update`, `edit`, `close`; `evaluate`,
//! `render`, `render_into`, `snapshot`, `export`, `cancel` and
//! `set_limits`), [`Viewport`], a document window's 3D view, and
//! [`LanguageServer`], its editor's language server (`language.rs`).
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

mod host;
mod language;
mod layer;
mod types;
mod viewport;

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

use session::{Run, Session};

pub use language::*;
pub use types::*;
pub use viewport::*;

uniffi::setup_scaffolding!();

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
/// caches, and the limits every request runs under. The app keeps one for
/// the whole process, so every window shares the caches.
#[derive(uniffi::Object)]
pub struct Core {
    session: Session,
    limits: Mutex<session::Limits>,
    test_hooks: bool,
    /// Analysed library files, shared by every window's language server.
    lsp_cache: Arc<lsp::Cache>,
}

impl std::fmt::Debug for Core {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Core")
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

impl Core {
    /// A request on `path`: named by its absolute path, with its directory
    /// as the working directory (so messages name the file as `neoscad`
    /// run in that directory does), superseding older requests on the
    /// same document (an editor's requests go stale with every edit), and
    /// under the core's current limits.
    fn run(&self, path: &str) -> Result<Run, CoreError> {
        let doc = self.doc_path(path)?;
        let mut run = Run::new(doc.to_string_lossy());
        run.cwd = doc.parent().map(Path::to_path_buf);
        run.supersede = true;
        run.limits = Some(*self.limits.lock().unwrap_or_else(PoisonError::into_inner));
        Ok(run)
    }

    /// `path` made absolute. A relative path would resolve against the
    /// app's working directory (`/` for a launched app), which is never
    /// what the caller meant.
    fn doc_path(&self, path: &str) -> Result<PathBuf, CoreError> {
        let p = PathBuf::from(path);
        if !p.is_absolute() {
            return Err(CoreError::InvalidArgument {
                message: format!("document paths must be absolute (got '{path}')"),
            });
        }
        Ok(session::normal(&p))
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
            let limits = cfg.limits;
            Ok(Arc::new(Core {
                session: Session::new(cfg),
                limits: Mutex::new(limits),
                test_hooks: config.test_hooks,
                lsp_cache: Arc::new(lsp::Cache::new()),
            }))
        })
    }

    // --- Documents ----------------------------------------------------------

    /// Open a document: with `text`, as an unsaved buffer that every read
    /// of its path sees (includes of it too); without, tracked but read
    /// from disk.
    pub fn open(&self, path: String, text: Option<String>) -> Result<DocInfo, CoreError> {
        guarded(|| {
            let doc = self.doc_path(&path)?;
            Ok(self.session.open(&doc, text.map(String::into_bytes)).into())
        })
    }

    /// Replace a document's whole text (opening it if needed). Requests
    /// still running on the old text are cancelled.
    pub fn update(&self, path: String, text: String) -> Result<DocInfo, CoreError> {
        guarded(|| {
            let doc = self.doc_path(&path)?;
            Ok(self.session.update(&doc, text.into_bytes()).into())
        })
    }

    /// Apply edits (UTF-8 byte offsets) to a document's current text.
    pub fn edit(&self, path: String, edits: Vec<TextEdit>) -> Result<DocInfo, CoreError> {
        guarded(|| {
            let doc = self.doc_path(&path)?;
            let edits: Vec<session::TextEdit> = edits
                .into_iter()
                .map(|e| session::TextEdit {
                    start: usize::try_from(e.start).unwrap_or(usize::MAX),
                    end: usize::try_from(e.end).unwrap_or(usize::MAX),
                    text: e.text,
                })
                .collect();
            self.session
                .edit(&doc, &edits)
                .map(Into::into)
                .map_err(|message| CoreError::InvalidArgument { message })
        })
    }

    /// Forget a document: its buffer, its last products and any request
    /// running on it. Whether it was open.
    pub fn close(&self, path: String) -> Result<bool, CoreError> {
        guarded(|| {
            let doc = self.doc_path(&path)?;
            Ok(self.session.close(&doc))
        })
    }

    /// Stop every request running on a document. How many there were.
    pub fn cancel(&self, path: String) -> Result<u32, CoreError> {
        guarded(|| {
            let doc = self.doc_path(&path)?;
            Ok(u32::try_from(self.session.cancel(&doc)).unwrap_or(u32::MAX))
        })
    }

    // --- Limits -------------------------------------------------------------

    /// Replace the limits of every later request (running ones keep
    /// theirs).
    pub fn set_limits(&self, limits: ResourceLimits) -> Result<(), CoreError> {
        guarded(|| {
            let l = limits.to_session()?;
            *self.limits.lock().unwrap_or_else(PoisonError::into_inner) = l;
            Ok(())
        })
    }

    pub fn limits(&self) -> Result<ResourceLimits, CoreError> {
        guarded(|| Ok((*self.limits.lock().unwrap_or_else(PoisonError::into_inner)).into()))
    }

    // --- Operations ---------------------------------------------------------

    /// Parse and evaluate: diagnostics and `echo()` output.
    pub fn evaluate(&self, path: String) -> Result<Evaluation, CoreError> {
        guarded(|| {
            let run = self.run(&path)?;
            let r = self.session.evaluate(&run, false)?;
            Ok(Evaluation {
                exit_code: r.exit_code,
                aborted: r.aborted,
                diagnostics: types::diagnostics(&r.log),
                echo: r.log.echo(),
                console: types::console(&r.log),
                timings: r.timings.into(),
            })
        })
    }

    /// Evaluate and build: geometry statistics and diagnostics.
    pub fn render(&self, path: String, mode: RenderMode) -> Result<RenderResult, CoreError> {
        guarded(|| {
            let run = self.run(&path)?;
            let scheme = render::ColorScheme::cornfield();
            let r = self.session.render(&run, mode.into(), &scheme)?;
            Ok(render_result(&r, &scheme))
        })
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
            let s = self.session.snapshot(&req).map_err(|e| match e {
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
            let id = format.unwrap_or_else(|| {
                target
                    .extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default()
            });
            let fmt = session::export::Format::from_id(&id).ok_or_else(|| {
                CoreError::InvalidArgument {
                    message: format!(
                        "unknown export format '{id}' (stl, binstl, off, obj, 3mf, wrl, pov, svg, dxf or pdf)"
                    ),
                }
            })?;
            let scheme = render::ColorScheme::cornfield();
            let settings = export_settings(&run, scheme.geometry_scheme());
            let req = session::ExportRequest {
                run,
                outputs: vec![(output.clone(), fmt)],
                force: false,
                scheme,
                settings,
            };
            let mut sink = Files { bytes: 0 };
            let r = self.session.export(&req, &mut sink)?;
            Ok(ExportResult {
                exit_code: r.exit_code,
                format: fmt.id().to_string(),
                bytes: sink.bytes,
                geometry: r
                    .geometry
                    .as_ref()
                    .map(|g| types::geometry_stats(g, &req.scheme.geometry_scheme())),
                diagnostics: types::diagnostics(&r.log),
                console: types::console(&r.log),
                timings: r.timings.into(),
            })
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
    RenderResult {
        exit_code: r.exit_code,
        diagnostics: types::diagnostics(&r.log),
        echo: r.log.echo(),
        console: types::console(&r.log),
        geometry: r
            .geometry
            .as_ref()
            .map(|g| types::geometry_stats(g, &scheme.geometry_scheme())),
        cache_entries: r.cache_entries as u64,
        timings: r.timings.into(),
    }
}

/// The encoder settings at OpenSCAD's defaults (no `-O` options): the
/// command line's `encode_settings` with an empty option list.
fn export_settings(run: &Run, scheme: geom::color::Scheme) -> session::export::Settings {
    session::export::Settings {
        scheme,
        svg: io::svg::SvgStyle::default(),
        pdf: io::pdf::PdfOptions::default(),
        pdf_warnings: Vec::new(),
        threemf: io::threemf::Options::default(),
        threemf_warning: None,
        title: Path::new(&run.input)
            .file_name()
            .map(|f| f.to_string_lossy().into_owned())
            .unwrap_or_default(),
        source_path: run.input.clone(),
        creation_date: iso8601_now(),
        pov_camera: None,
    }
}

/// `YYYY-MM-DDTHH:MM:SSZ` now, for the dates PDF and 3MF files record
/// (Howard Hinnant's `civil_from_days`, as the command line computes it).
fn iso8601_now() -> String {
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
