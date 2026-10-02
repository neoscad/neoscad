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
    pub(crate) core: Arc<Core>,
    pub(crate) server: crate::FreedDeep<lsp::Server>,
}

#[uniffi::export]
impl Core {
    /// A language server for one editor, sharing this core's session.
    /// With `host_diagnostics` its diagnostics are those of the runs the
    /// host makes anyway (`run_document` hands them over) and it never
    /// evaluates by itself: a document window's editor, whose document is
    /// previewed after every pause in typing. Without, it evaluates each
    /// changed document for its diagnostics (`publish_diagnostics`).
    pub fn language_server(
        self: Arc<Self>,
        host_diagnostics: bool,
    ) -> Result<Arc<LanguageServer>, CoreError> {
        guarded(|| {
            let options = lsp::Options {
                host_diagnostics,
                ..lsp::Options::default()
            };
            let server = lsp::Server::with_cache(options, self.lsp_cache.clone());
            Ok(Arc::new(LanguageServer {
                core: self,
                server: crate::FreedDeep::new(server),
            }))
        })
    }

    /// A file's text as the core reads it: an open document's buffer, a
    /// file on disk, or a bundled library (MCAD is mounted in memory and
    /// exists nowhere on disk), for showing a library file the editor
    /// jumped to. Not UTF-8 text is decoded lossily; the view is
    /// read-only.
    pub fn read_file(&self, path: String) -> Result<String, CoreError> {
        guarded(|| self.client.read_file(&path))
    }

    /// The library directories, in search order (`OPENSCADPATH`, the
    /// user's library folder, the bundled libraries): a file under one is
    /// a library the editor shows read-only.
    pub fn library_dirs(&self) -> Result<Vec<String>, CoreError> {
        guarded(|| Ok(self.client.library_dirs()))
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
                self.server.handle(self.core.session(), &message)
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
            let limits = self.core.client.current_limits();
            self.server.set_limits(Some(limits));
            // The evaluation has its own thread, but turning diagnostics
            // into markers parses the document and its includes when
            // `handle` has not yet, which needs the evaluator's stack as
            // much as `handle` does.
            Ok(eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
                self.server.publish_diagnostics(self.core.session())
            }))
        })
    }
}
