//! What every NeoSCAD front end does with a [`session::Session`], without
//! the host: document runs and their console lines in editor positions,
//! the customizer, check, measure (with sections, distances and picking)
//! and export, with results shaped as serde records.
//!
//! Two hosts wrap it. The macOS app's core (`crates/ffi`) declares these
//! records to UniFFI as remote types and adds what needs the machine:
//! the disk, the clock, the GPU and the viewport. The web worker
//! (`crates/web`) sends them as JSON (`docs/web-protocol.md`). Keeping the
//! shaping here means the two cannot drift: a console line points at the
//! same place, and a check reports the same numbers, in both.
//!
//! Like every library crate it never touches `std::fs`, `std::env` or the
//! clock (`CLAUDE.md`, "Rules"): files come through the session's
//! `FileSystem`, and what only a host knows (whether a file is on disk, a
//! creation date, where an export's bytes go) is passed in.

mod document;
mod document_loop;
mod examples;
mod inspect;
mod overlay;
mod present;
mod preview;
mod text;
mod types;
pub mod update;

use std::path::PathBuf;
use std::sync::{Mutex, PoisonError};

use session::{Run, Session};

pub use document::*;
pub use document_loop::*;
pub use examples::*;
pub use inspect::*;
pub use overlay::*;
pub use present::*;
pub use preview::*;
pub use text::*;
pub use types::*;

/// One session and the limits every request runs under: the state a
/// front end keeps for its whole life.
pub struct Client {
    pub session: Session,
    limits: Mutex<session::Limits>,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Client")
            .field("session", &self.session)
            .finish_non_exhaustive()
    }
}

impl Client {
    /// A client over a session made from `cfg`, starting with its limits.
    pub fn new(cfg: session::Config) -> Client {
        let limits = cfg.limits;
        Client {
            session: Session::new(cfg),
            limits: Mutex::new(limits),
        }
    }

    /// The limits later requests run under.
    pub fn current_limits(&self) -> session::Limits {
        *self.limits.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn limits(&self) -> ResourceLimits {
        self.current_limits().into()
    }

    /// Replace the limits of every later request (running ones keep
    /// theirs).
    pub fn set_limits(&self, limits: ResourceLimits) -> Result<(), CoreError> {
        let l = limits.to_session()?;
        *self.limits.lock().unwrap_or_else(PoisonError::into_inner) = l;
        Ok(())
    }

    /// `path` made absolute. A relative path would resolve against the
    /// host's working directory (`/` for a launched app), which is never
    /// what the caller meant.
    ///
    /// "Absolute" is `has_root`, a leading `/`: on Unix that is exactly
    /// `is_absolute`, but on wasm32-unknown-unknown `is_absolute` also
    /// asks for a Windows-style prefix, so it refused every path the web
    /// worker uses (`/doc/main.scad`).
    pub fn doc_path(&self, path: &str) -> Result<PathBuf, CoreError> {
        let p = PathBuf::from(path);
        if !p.has_root() {
            return Err(CoreError::InvalidArgument {
                message: format!("document paths must be absolute (got '{path}')"),
            });
        }
        Ok(session::normal(&p))
    }

    /// A request on `path`: named by its absolute path, with its directory
    /// as the working directory (so messages name the file as `neoscad`
    /// run in that directory does), superseding older requests on the
    /// same document (an editor's requests go stale with every edit), and
    /// under the current limits.
    pub fn run(&self, path: &str) -> Result<Run, CoreError> {
        let doc = self.doc_path(path)?;
        let mut run = Run::new(doc.to_string_lossy());
        run.cwd = doc.parent().map(std::path::Path::to_path_buf);
        run.supersede = true;
        run.limits = Some(self.current_limits());
        Ok(run)
    }

    // --- Documents ----------------------------------------------------------

    /// Open a document: with `text`, as an unsaved buffer that every read
    /// of its path sees (includes of it too); without, tracked but read
    /// from the file system.
    pub fn open(&self, path: &str, text: Option<String>) -> Result<DocInfo, CoreError> {
        let doc = self.doc_path(path)?;
        Ok(self.session.open(&doc, text.map(String::into_bytes)).into())
    }

    /// Replace a document's whole text (opening it if needed). Requests
    /// still running on the old text are cancelled.
    pub fn update(&self, path: &str, text: String) -> Result<DocInfo, CoreError> {
        let doc = self.doc_path(path)?;
        Ok(self.session.update(&doc, text.into_bytes()).into())
    }

    /// Apply edits (UTF-8 byte offsets) to a document's current text.
    pub fn edit(&self, path: &str, edits: Vec<TextEdit>) -> Result<DocInfo, CoreError> {
        let doc = self.doc_path(path)?;
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
    }

    /// Forget a document: its buffer, its last products and any request
    /// running on it. Whether it was open.
    pub fn close(&self, path: &str) -> Result<bool, CoreError> {
        let doc = self.doc_path(path)?;
        Ok(self.session.close(&doc))
    }

    /// Stop every request running on a document. How many there were.
    pub fn cancel(&self, path: &str) -> Result<u32, CoreError> {
        let doc = self.doc_path(path)?;
        Ok(u32::try_from(self.session.cancel(&doc)).unwrap_or(u32::MAX))
    }

    /// How many requests are running on a document.
    pub fn running(&self, path: &str) -> Result<u32, CoreError> {
        let doc = self.doc_path(path)?;
        Ok(u32::try_from(self.session.running(&doc)).unwrap_or(u32::MAX))
    }

    // --- Operations ---------------------------------------------------------

    /// Parse and evaluate: diagnostics and `echo()` output.
    pub fn evaluate(&self, path: &str) -> Result<Evaluation, CoreError> {
        let run = self.run(path)?;
        let r = self.session.evaluate(&run, false)?;
        Ok(Evaluation {
            exit_code: r.exit_code,
            aborted: r.aborted,
            diagnostics: diagnostics(&r.log),
            echo: r.log.echo(),
            console: console(&r.log),
            timings: r.timings.into(),
        })
    }

    /// Evaluate and build: geometry statistics and diagnostics.
    pub fn render(&self, path: &str, mode: RenderMode) -> Result<RenderResult, CoreError> {
        let run = self.run(path)?;
        let scheme = render::ColorScheme::cornfield();
        let r = self.session.render(&run, mode.into(), &scheme)?;
        Ok(render_result(&r, &scheme))
    }

    /// A file's text as the session reads it: an open document's buffer,
    /// a file of the host's file system, or a bundled library (MCAD is
    /// mounted in memory), for showing a library file the editor jumped
    /// to. Not UTF-8 text is decoded lossily; the view is read-only.
    pub fn read_file(&self, path: &str) -> Result<String, CoreError> {
        let p = self.doc_path(path)?;
        let bytes = self.session.fs().read(&p).map_err(|e| CoreError::Failed {
            message: format!("cannot read '{}': {e}", p.display()),
        })?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// The library directories, in search order: a file under one is a
    /// library the editor shows read-only.
    pub fn library_dirs(&self) -> Vec<String> {
        self.session
            .config()
            .libs
            .0
            .iter()
            .map(|p| session::normal(p).to_string_lossy().into_owned())
            .collect()
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod shared_tests;

#[cfg(test)]
mod scene_tests;
