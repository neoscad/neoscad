//! The language server for an editor (`crates/lsp`), over the core's
//! session: the editor's `@codemirror/lsp-client` sends JSON-RPC through
//! the web view's bridge to Swift, and Swift passes each message here.
//!
//! Every window's editor has a server of its own (each client sends its
//! own `initialize`), and they share the core's session and its cache of
//! analysed library files, so BOSL2 is indexed once per process.
//!
//! The app keeps the session's copy of a document itself (`Core.edit`),
//! so the server does not write its documents into the session
//! (`sync_session` off); it evaluates the exact version its client sent
//! for diagnostics instead.

use std::sync::Arc;

use crate::{Core, CoreError, guarded};

/// A language server for one editor. See the module documentation.
#[derive(Debug, uniffi::Object)]
pub struct LanguageServer {
    core: Arc<Core>,
    server: lsp::Server,
}

#[uniffi::export]
impl Core {
    /// A language server for one editor, sharing this core's session.
    pub fn language_server(self: Arc<Self>) -> Result<Arc<LanguageServer>, CoreError> {
        guarded(|| {
            let server = lsp::Server::with_cache(lsp::Options::default(), self.lsp_cache.clone());
            Ok(Arc::new(LanguageServer { core: self, server }))
        })
    }

    /// A file's text as the core reads it: an open document's buffer, a
    /// file on disk, or a bundled library (MCAD is mounted in memory and
    /// exists nowhere on disk), for showing a library file the editor
    /// jumped to. Not UTF-8 text is decoded lossily; the view is
    /// read-only.
    pub fn read_file(&self, path: String) -> Result<String, CoreError> {
        guarded(|| {
            let p = self.doc_path(&path)?;
            let bytes = self.session.fs().read(&p).map_err(|e| CoreError::Failed {
                message: format!("cannot read '{}': {e}", p.display()),
            })?;
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        })
    }

    /// The library directories, in search order (`OPENSCADPATH`, the
    /// user's library folder, the bundled libraries): a file under one is
    /// a library the editor shows read-only.
    pub fn library_dirs(&self) -> Result<Vec<String>, CoreError> {
        guarded(|| {
            Ok(self
                .session
                .config()
                .libs
                .0
                .iter()
                .map(|p| session::normal(p).to_string_lossy().into_owned())
                .collect())
        })
    }
}

#[uniffi::export]
impl LanguageServer {
    /// Handle one JSON-RPC message from the editor; the messages to send
    /// back, in order. Parsing recurses over the text, so it runs on a
    /// thread with the evaluator's stack (a dispatch queue's is 512 KiB).
    pub fn handle(&self, message: String) -> Result<Vec<String>, CoreError> {
        guarded(|| {
            Ok(eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
                self.server.handle(&self.core.session, &message)
            }))
        })
    }

    /// Whether a document changed since its diagnostics were published:
    /// the app then calls `publish_diagnostics` once typing pauses.
    pub fn diagnostics_pending(&self) -> Result<bool, CoreError> {
        guarded(|| Ok(self.server.diagnostics_pending()))
    }

    /// Evaluate the changed documents (under the core's current limits)
    /// and return their `publishDiagnostics` notifications. Blocks for
    /// the evaluations; a change meanwhile stops a stale one.
    pub fn publish_diagnostics(&self) -> Result<Vec<String>, CoreError> {
        guarded(|| {
            let limits = *self
                .core
                .limits
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.server.set_limits(Some(limits));
            Ok(self.server.publish_diagnostics(&self.core.session))
        })
    }
}
