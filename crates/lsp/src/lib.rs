//! The OpenSCAD language server: the Language Server Protocol over a
//! [`session::Session`], with no transport of its own. A host passes each
//! JSON-RPC message in ([`Server::handle`]) and sends what comes back;
//! the app does it through its FFI bridge and the editor's
//! `@codemirror/lsp-client`, `neoscad lsp --stdio` over stdio with the
//! protocol's `Content-Length` framing for other editors.
//!
//! Features: diagnostics (the session's own, pushed, with fixes),
//! hover, completion, signature help, go to definition, find references,
//! formatting (whole document and range, by `crates/fmt`), document
//! symbols, folding ranges, rename (of names only this document uses)
//! and quick fixes. Positions are UTF-16 (`proto`).
//!
//! # Diagnostics without a clock
//!
//! Library crates never read the clock (`CLAUDE.md`), so the server does
//! not debounce by itself. A change marks the document; the host asks
//! [`Server::diagnostics_pending`] after each message, waits until the
//! messages pause for its debounce interval, and then calls
//! [`Server::publish_diagnostics`] (on another thread if it likes: the
//! server is `Sync`, and a newer change stops an evaluation that went
//! stale). The evaluation is the session's (parse and evaluate, not
//! geometry), of the exact text version the client sent.
//!
//! # Documents and the session
//!
//! The server keeps its documents' text itself. With
//! [`Options::sync_session`] it also opens them in the session, so that
//! renders and other documents' includes see the unsaved text; the app
//! turns that off because its own edit path already keeps the session's
//! copy.
//!
//! # Caches
//!
//! A document's parse and index are made once per version. Included and
//! library files are parsed and indexed once per content ([`Cache`],
//! shared by every server of a host), and a document's program (its
//! includes and libraries, [`World`]) is kept while its version and the
//! files' metadata are unchanged.

mod complete;
mod context;
mod describe;
mod diagnose;
pub mod index;
mod layout;
mod navigate;
pub mod proto;
pub mod uri;
mod value;
pub mod world;

use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

use lang::loader::Metadata;
use lang::source::SourceFile;
use serde_json::{Value, json};
use session::{Limits, Run, Session};

pub use world::{Analyzed, Cache, World};

use proto::code;

/// How the server works with its host's session.
#[derive(Debug, Clone, Default)]
pub struct Options {
    /// Open the client's documents in the session (with their unsaved
    /// text) and close them when the client does. `neoscad lsp` does; the
    /// app does not, because it keeps the session's copy itself.
    pub sync_session: bool,
    /// Limits of the diagnostics' evaluations; the session's own when
    /// `None`.
    pub limits: Option<Limits>,
}

/// What a document's program read: each file with its metadata then.
type Stamps = Vec<(PathBuf, Option<Metadata>)>;

/// An open document at one version.
#[derive(Debug)]
struct Doc {
    path: PathBuf,
    version: i64,
    text: Arc<[u8]>,
    analyzed: OnceLock<Arc<Analyzed>>,
    /// The document's program, while nothing it read has changed.
    world: Mutex<Option<(Arc<World>, Stamps)>>,
}

impl Doc {
    fn new(path: PathBuf, version: i64, text: Arc<[u8]>) -> Doc {
        Doc {
            path,
            version,
            text,
            analyzed: OnceLock::new(),
            world: Mutex::new(None),
        }
    }

    fn analyzed(&self) -> Arc<Analyzed> {
        self.analyzed
            .get_or_init(|| Arc::new(Analyzed::new(self.path.clone(), self.text.to_vec())))
            .clone()
    }
}

#[derive(Debug, Default)]
struct State {
    initialized: bool,
    shutdown: bool,
    exited: bool,
    docs: HashMap<String, Arc<Doc>>,
    /// Documents whose diagnostics are due, in the order they changed.
    dirty: Vec<String>,
    /// The flag of each document's running evaluation.
    running: HashMap<String, Arc<AtomicBool>>,
    /// The other URIs each document last published to (includes).
    published: HashMap<String, HashSet<String>>,
    /// The last diagnostics per URI, for code actions.
    diagnostics: HashMap<String, Vec<Value>>,
}

/// A language server. See the crate documentation.
#[derive(Debug)]
pub struct Server {
    opts: Mutex<Options>,
    cache: Arc<Cache>,
    state: Mutex<State>,
}

/// What a request works on: the session, the document's program and its
/// URI.
pub(crate) struct Ctx<'a> {
    pub session: &'a Session,
    pub world: Arc<World>,
    pub uri: String,
    /// The library directories, for showing library files' paths.
    pub libs: Vec<PathBuf>,
    /// The URIs the client opened files under, by path.
    pub uris: HashMap<PathBuf, String>,
}

impl Ctx<'_> {
    pub fn file(&self) -> &Arc<Analyzed> {
        self.world.main()
    }

    /// The URI of a file: the client's own spelling for a file it has
    /// open (a client matches URIs as strings, and its percent-encoding
    /// may differ from ours), otherwise made from the path.
    pub fn uri_for(&self, path: &Path) -> String {
        self.uris
            .get(path)
            .cloned()
            .unwrap_or_else(|| uri::from_path(path))
    }
}

impl Server {
    /// A server with a cache of its own.
    pub fn new(options: Options) -> Server {
        Server::with_cache(options, Arc::new(Cache::new()))
    }

    /// A server sharing `cache` with others (the app's windows).
    pub fn with_cache(options: Options, cache: Arc<Cache>) -> Server {
        Server {
            opts: Mutex::new(options),
            cache,
            state: Mutex::new(State::default()),
        }
    }

    pub fn set_limits(&self, limits: Option<Limits>) {
        self.opts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .limits = limits;
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Whether the client sent `exit`; the host stops then.
    pub fn exited(&self) -> bool {
        self.state().exited
    }

    /// The exit code `exit` asks for: 0 after `shutdown`, 1 without.
    pub fn exit_code(&self) -> i32 {
        i32::from(!self.state().shutdown)
    }

    /// Whether a document changed since its diagnostics were published.
    pub fn diagnostics_pending(&self) -> bool {
        !self.state().dirty.is_empty()
    }

    /// Handle one message; the messages to send back (a response for a
    /// request, nothing for most notifications). A request that panics
    /// is answered with an internal error, and the server goes on.
    pub fn handle(&self, session: &Session, message: &str) -> Vec<String> {
        let msg: Value = match serde_json::from_str(message) {
            Ok(v) => v,
            Err(e) => return vec![proto::error(&Value::Null, code::PARSE_ERROR, e.to_string())],
        };
        let id = msg.get("id").cloned();
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            // A response to a request of ours (there are none), or junk.
            return match id {
                Some(id) if msg.get("result").is_none() && msg.get("error").is_none() => {
                    vec![proto::error(&id, code::INVALID_REQUEST, "no method")]
                }
                _ => Vec::new(),
            };
        };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        match id {
            Some(id) => {
                let r = catch_unwind(AssertUnwindSafe(|| self.request(session, method, &params)));
                vec![match r {
                    Ok(Ok(result)) => proto::response(&id, result),
                    Ok(Err((c, m))) => proto::error(&id, c, m),
                    Err(p) => {
                        let what = p
                            .downcast_ref::<&str>()
                            .map(|s| s.to_string())
                            .or_else(|| p.downcast_ref::<String>().cloned())
                            .unwrap_or_default();
                        proto::error(
                            &id,
                            code::INTERNAL_ERROR,
                            format!("the server panicked: {what}"),
                        )
                    }
                }]
            }
            None => {
                let _ = catch_unwind(AssertUnwindSafe(|| {
                    self.notification(session, method, &params)
                }));
                Vec::new()
            }
        }
    }

    fn request(
        &self,
        session: &Session,
        method: &str,
        params: &Value,
    ) -> Result<Value, (i64, String)> {
        {
            let mut st = self.state();
            if method == "initialize" {
                st.initialized = true;
                return Ok(initialize_result());
            }
            if !st.initialized {
                return Err((code::SERVER_NOT_INITIALIZED, "initialize first".into()));
            }
            if st.shutdown {
                return Err((code::INVALID_REQUEST, "the server is shutting down".into()));
            }
            if method == "shutdown" {
                st.shutdown = true;
                return Ok(Value::Null);
            }
        }
        let needs_doc = method.starts_with("textDocument/");
        if !needs_doc {
            return Err((code::METHOD_NOT_FOUND, format!("unknown method '{method}'")));
        }
        let uri = params
            .pointer("/textDocument/uri")
            .and_then(Value::as_str)
            .ok_or((code::INVALID_PARAMS, "no textDocument.uri".to_string()))?;
        let ctx = self.context(session, uri)?;
        Ok(match method {
            "textDocument/hover" => navigate::hover(&ctx, params),
            "textDocument/completion" => complete::completion(&ctx, params),
            "textDocument/signatureHelp" => navigate::signature_help(&ctx, params),
            "textDocument/definition" => navigate::definition(&ctx, params),
            "textDocument/references" => navigate::references(&ctx, params),
            "textDocument/formatting" => layout::format(&ctx, None)?,
            "textDocument/rangeFormatting" => {
                let range = layout::range_of(&ctx, params)
                    .ok_or((code::INVALID_PARAMS, "no range".to_string()))?;
                layout::format(&ctx, Some(range))?
            }
            "textDocument/documentSymbol" => layout::symbols(&ctx),
            "textDocument/foldingRange" => layout::folding(&ctx),
            "textDocument/prepareRename" => navigate::prepare_rename(&ctx, params)?,
            "textDocument/rename" => navigate::rename(&ctx, params)?,
            "textDocument/codeAction" => {
                let published = self
                    .state()
                    .diagnostics
                    .get(uri)
                    .cloned()
                    .unwrap_or_default();
                diagnose::code_actions(uri, ctx.file().source(), params, &published)
            }
            _ => return Err((code::METHOD_NOT_FOUND, format!("unknown method '{method}'"))),
        })
    }

    fn notification(&self, session: &Session, method: &str, params: &Value) {
        let mut st = self.state();
        if method == "exit" {
            st.exited = true;
            return;
        }
        if !st.initialized {
            return;
        }
        let uri = params
            .pointer("/textDocument/uri")
            .and_then(Value::as_str)
            .map(str::to_string);
        match method {
            "textDocument/didOpen" => {
                let (Some(uri), Some(text)) = (
                    uri,
                    params.pointer("/textDocument/text").and_then(Value::as_str),
                ) else {
                    return;
                };
                let Some(path) = uri::to_path(&uri) else {
                    return;
                };
                let version = params
                    .pointer("/textDocument/version")
                    .and_then(Value::as_i64)
                    .unwrap_or(0);
                let text: Arc<[u8]> = text.as_bytes().into();
                self.changed(&mut st, session, &uri, Doc::new(path, version, text));
            }
            "textDocument/didChange" => {
                let Some(uri) = uri else {
                    return;
                };
                let Some(old) = st.docs.get(&uri).cloned() else {
                    return;
                };
                let version = params
                    .pointer("/textDocument/version")
                    .and_then(Value::as_i64)
                    .unwrap_or(old.version + 1);
                let mut text = old.text.to_vec();
                for c in params
                    .get("contentChanges")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let Some(new) = c.get("text").and_then(Value::as_str) else {
                        continue;
                    };
                    match c.get("range") {
                        None => text = new.as_bytes().to_vec(),
                        Some(r) => {
                            // Each change is relative to the text the
                            // previous one left.
                            let src = SourceFile::new(old.path.clone(), text.clone());
                            let Some((a, b)) = proto::offsets(&src, r) else {
                                continue;
                            };
                            text.splice(a as usize..b as usize, new.bytes());
                        }
                    }
                }
                let doc = Doc::new(old.path.clone(), version, text.into());
                self.changed(&mut st, session, &uri, doc);
            }
            "textDocument/didClose" => {
                let Some(uri) = uri else {
                    return;
                };
                if let Some(doc) = st.docs.remove(&uri) {
                    if let Some(f) = st.running.remove(&uri) {
                        f.store(true, Ordering::Relaxed);
                    }
                    st.dirty.retain(|u| *u != uri);
                    if self.sync_session() {
                        session.close(&doc.path);
                    }
                }
            }
            _ => {}
        }
    }

    fn sync_session(&self) -> bool {
        self.opts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .sync_session
    }

    /// A document opened or changed: keep it, stop its stale evaluation,
    /// and mark its diagnostics due.
    fn changed(&self, st: &mut State, session: &Session, uri: &str, doc: Doc) {
        if self.sync_session() {
            session.update(&doc.path, doc.text.to_vec());
        }
        st.docs.insert(uri.to_string(), Arc::new(doc));
        if let Some(f) = st.running.get(uri) {
            f.store(true, Ordering::Relaxed);
        }
        if !st.dirty.iter().any(|u| u == uri) {
            st.dirty.push(uri.to_string());
        }
    }

    /// The document `uri` (open, or read through the session when the
    /// client asks about a file it has not opened) and its program.
    fn context<'s>(&self, session: &'s Session, uri: &str) -> Result<Ctx<'s>, (i64, String)> {
        let path =
            uri::to_path(uri).ok_or((code::INVALID_PARAMS, format!("not a file URI: {uri}")))?;
        let (doc, open, uris) = {
            let st = self.state();
            let open: HashMap<PathBuf, Arc<Doc>> = st
                .docs
                .values()
                .map(|d| (d.path.clone(), d.clone()))
                .collect();
            let uris: HashMap<PathBuf, String> = st
                .docs
                .iter()
                .map(|(u, d)| (d.path.clone(), u.clone()))
                .collect();
            (st.docs.get(uri).cloned(), open, uris)
        };
        let doc = match doc {
            Some(d) => d,
            None => {
                let text = session.fs().read(&path).map_err(|e| {
                    (
                        code::INVALID_PARAMS,
                        format!("cannot read '{}': {e}", path.display()),
                    )
                })?;
                Arc::new(Doc::new(path.clone(), 0, text.into()))
            }
        };
        let world = self.world(session, &doc, &open);
        Ok(Ctx {
            session,
            world,
            uri: uri.to_string(),
            libs: session.config().libs.0.clone(),
            uris,
        })
    }

    /// The program of `doc`, rebuilt only when a file it read changed.
    fn world(&self, session: &Session, doc: &Doc, open: &HashMap<PathBuf, Arc<Doc>>) -> Arc<World> {
        let fs = session.fs();
        let stamp = |p: &Path| -> Option<Metadata> {
            match open.get(p) {
                // An open document's version stands in for its metadata.
                Some(d) => Some(Metadata {
                    modified: Some(i128::from(d.version)),
                    len: d.text.len() as u64,
                }),
                None => fs.metadata(p),
            }
        };
        let mut slot = doc.world.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((w, stamps)) = &*slot
            && stamps.iter().all(|(p, m)| m.is_some() && stamp(p) == *m)
        {
            return w.clone();
        }
        let loader = world::Loader {
            fs: &*fs,
            libs: &session.config().libs,
            cache: &self.cache,
            open: &|p: &Path| {
                open.get(p)
                    .filter(|d| d.path != doc.path)
                    .map(|d| d.analyzed())
            },
        };
        let w = Arc::new(World::new(doc.analyzed(), &loader));
        let stamps = w
            .files
            .iter()
            .skip(1)
            .chain(w.libs.iter().flatten())
            .map(|f| (f.path.clone(), stamp(&f.path)))
            .collect();
        *slot = Some((w.clone(), stamps));
        w
    }

    /// Evaluate every document whose diagnostics are due and return the
    /// `publishDiagnostics` notifications. Blocks for the evaluations; a
    /// document changed meanwhile is skipped (it is due again).
    pub fn publish_diagnostics(&self, session: &Session) -> Vec<String> {
        let (jobs, open, uris) = {
            let mut st = self.state();
            let uris = std::mem::take(&mut st.dirty);
            let mut jobs = Vec::new();
            for uri in uris {
                let Some(doc) = st.docs.get(&uri).cloned() else {
                    continue;
                };
                let flag = Arc::new(AtomicBool::new(false));
                st.running.insert(uri.clone(), flag.clone());
                jobs.push((uri, doc, flag));
            }
            let open: HashMap<PathBuf, Arc<Doc>> = st
                .docs
                .values()
                .map(|d| (d.path.clone(), d.clone()))
                .collect();
            let uris: HashMap<PathBuf, String> = st
                .docs
                .iter()
                .map(|(u, d)| (d.path.clone(), u.clone()))
                .collect();
            (jobs, open, uris)
        };
        let limits = self
            .opts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .limits;
        let mut out = Vec::new();
        for (uri, doc, flag) in jobs {
            let mut run = Run::new(doc.path.to_string_lossy());
            run.cwd = doc.path.parent().map(Path::to_path_buf);
            // Not superseding: the app renders the same document, and
            // neither may cancel the other. Stale runs stop by `flag`.
            run.supersede = false;
            run.text = Some(doc.text.clone());
            run.interrupt = Some(flag.clone());
            run.limits = limits;
            let Ok(ev) = session.evaluate(&run, false) else {
                // Cancelled: by a newer change (already due again), or by
                // the host cancelling the document's requests, after which
                // this version still wants its diagnostics.
                let mut st = self.state();
                if st.docs.get(&uri).is_some_and(|d| Arc::ptr_eq(d, &doc))
                    && !st.dirty.contains(&uri)
                {
                    st.dirty.push(uri);
                }
                continue;
            };
            let world = self.world(session, &doc, &open);
            let fs = session.fs();
            let text_of = |p: &Path| -> Option<Arc<SourceFile>> {
                let t = match open.get(p) {
                    Some(d) => d.text.to_vec(),
                    None => fs.read(p).ok()?,
                };
                Some(Arc::new(SourceFile::new(p.to_path_buf(), t)))
            };
            let uri_for = |p: &Path| uris.get(p).cloned().unwrap_or_else(|| uri::from_path(p));
            let per_uri = diagnose::convert(
                &world,
                &session.config().libs.0,
                &ev.log.diagnostics_json(),
                &text_of,
                &uri_for,
            );
            let mut st = self.state();
            let current = st.docs.get(&uri).is_some_and(|d| Arc::ptr_eq(d, &doc));
            if !current || flag.load(Ordering::Relaxed) {
                continue;
            }
            st.running.remove(&uri);
            let now: HashSet<String> = per_uri.iter().skip(1).map(|(u, _)| u.clone()).collect();
            let before = st
                .published
                .insert(uri.clone(), now.clone())
                .unwrap_or_default();
            for gone in before.difference(&now) {
                st.diagnostics.remove(gone);
                out.push(proto::notification(
                    "textDocument/publishDiagnostics",
                    json!({"uri": gone, "diagnostics": []}),
                ));
            }
            for (i, (u, list)) in per_uri.into_iter().enumerate() {
                let mut params = json!({"uri": u, "diagnostics": list});
                if i == 0 {
                    params["version"] = json!(doc.version);
                }
                st.diagnostics.insert(u, list);
                out.push(proto::notification(
                    "textDocument/publishDiagnostics",
                    params,
                ));
            }
        }
        out
    }

    /// The analysed files the shared cache holds (for tests and stats).
    pub fn cached_files(&self) -> usize {
        self.cache.len()
    }
}

/// The server's capabilities.
fn initialize_result() -> Value {
    json!({
        "capabilities": {
            "positionEncoding": "utf-16",
            "textDocumentSync": {"openClose": true, "change": 2, "save": false},
            "hoverProvider": true,
            "completionProvider": {"triggerCharacters": ["$"], "resolveProvider": false},
            "signatureHelpProvider": {"triggerCharacters": ["(", ","], "retriggerCharacters": [","]},
            "definitionProvider": true,
            "referencesProvider": true,
            "documentFormattingProvider": true,
            "documentRangeFormattingProvider": true,
            "documentSymbolProvider": true,
            "foldingRangeProvider": true,
            "renameProvider": {"prepareProvider": true},
            "codeActionProvider": {"codeActionKinds": ["quickfix"]},
        },
        "serverInfo": {"name": "neoscad", "version": env!("CARGO_PKG_VERSION")},
    })
}
