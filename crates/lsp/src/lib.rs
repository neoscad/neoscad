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
//! # Diagnostics from the host's own runs
//!
//! A host that evaluates the document anyway (the app previews it after
//! each pause in typing) sets [`Options::host_diagnostics`]: the server
//! then never evaluates, and the host hands it each run's diagnostics
//! with the exact text the run read ([`Server::supply`]). They are
//! published for the client's version of the document whose text is that
//! text, as soon as both are known, whichever arrives first; a run of
//! another text is kept until a newer one replaces it. So each pause
//! costs one evaluation, not one for the view and one for the markers,
//! and the geometry stage's warnings (a render's) reach the markers too.
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
mod fillet;
pub mod index;
mod layout;
mod navigate;
pub mod proto;
mod sketch;
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
    /// The host supplies each document's diagnostics ([`Server::supply`])
    /// and the server never evaluates (see the crate documentation).
    pub host_diagnostics: bool,
    /// NeoSCAD's extensions the host runs documents with, besides the
    /// session's own (`Config::extensions`): with `sketch`, sketch bodies
    /// bind the sketch vocabulary for completion, hover and navigation,
    /// and the server's own evaluations enable it.
    pub extensions: session::Extensions,
}

/// A run the host supplied: the text it read and its diagnostics.
type Supplied = (Arc<[u8]>, Arc<Vec<Value>>);

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
    /// With [`Options::host_diagnostics`]: the latest run the host
    /// supplied per document path, with the text it read.
    supplied: HashMap<PathBuf, Supplied>,
    /// The latest run's sketches per document path, with the text it
    /// read (hover's solved values, "Pin drawing").
    sketches: HashMap<PathBuf, sketch::Facts>,
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
    /// The sketches of the last run of exactly this text, if any.
    pub sketches: Option<sketch::Facts>,
}

impl Ctx<'_> {
    pub fn file(&self) -> &Arc<Analyzed> {
        self.world.main()
    }

    /// The last run's sketches, when it read the document's current text.
    pub fn sketches(&self) -> &[Value] {
        self.sketches
            .as_ref()
            .and_then(|f| f.for_text(self.file().text()))
            .unwrap_or_default()
    }

    /// The last rendered run's fillet calls, when it read the document's
    /// current text.
    pub fn fillets(&self) -> &[Value] {
        self.sketches
            .as_ref()
            .and_then(|f| f.fillets_for_text(self.file().text()))
            .unwrap_or_default()
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

    /// Change the extensions the host runs documents with
    /// ([`Options::extensions`]), as an app's setting does.
    pub fn set_extensions(&self, extensions: session::Extensions) {
        self.opts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extensions = extensions;
    }

    /// The extensions documents run with: the host's and the session's.
    fn extensions_on(&self, session: &Session) -> session::Extensions {
        let opts = self
            .opts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extensions;
        opts.union(session.config().extensions)
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

    /// Whether a document changed since its diagnostics were published
    /// and the server should evaluate it ([`Server::publish_diagnostics`]).
    /// Never with [`Options::host_diagnostics`]: the host's runs publish.
    pub fn diagnostics_pending(&self) -> bool {
        !self.host_diagnostics() && !self.state().dirty.is_empty()
    }

    fn host_diagnostics(&self) -> bool {
        self.opts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .host_diagnostics
    }

    /// With [`Options::host_diagnostics`]: the diagnostics of a run the
    /// host made of the document at `path` (`Log::diagnostics_json`), and
    /// the exact text it read. Returns the `publishDiagnostics`
    /// notifications for the client's version with that text, if it has
    /// sent it; otherwise they go out once it does (from
    /// [`Server::handle`]). A later run replaces an earlier one.
    pub fn supply(
        &self,
        session: &Session,
        path: &Path,
        text: Arc<[u8]>,
        diagnostics: Vec<Value>,
    ) -> Vec<String> {
        let path = session::normal(path);
        {
            let mut st = self.state();
            st.supplied
                .insert(path.clone(), (text, Arc::new(diagnostics)));
            // The same text evaluated again (a render after a preview) is
            // news even for a version already published.
            let uris: Vec<String> = st
                .docs
                .iter()
                .filter(|(_, d)| session::normal(&d.path) == path)
                .map(|(u, _)| u.clone())
                .collect();
            for u in uris {
                if !st.dirty.contains(&u) {
                    st.dirty.push(u);
                }
            }
        }
        self.publish_supplied(session)
    }

    /// [`Server::supply`] with a whole run's log: its diagnostics, and
    /// its sketches for hover and "Pin drawing". What a host that has the
    /// log calls.
    pub fn supply_log(
        &self,
        session: &Session,
        path: &Path,
        text: Arc<[u8]>,
        log: &session::Log,
    ) -> Vec<String> {
        self.keep_sketches(path, text.clone(), log);
        self.supply(session, path, text, log.diagnostics_json())
    }

    /// Keep a run's sketches for the document at `path` (dropping an
    /// older run's, also when this one has none).
    fn keep_sketches(&self, path: &Path, text: Arc<[u8]>, log: &session::Log) {
        let mut st = self.state();
        let key = session::normal(path);
        // Fillet reports come only from a run that rendered (a host's
        // supplied log); the server's own runs only evaluate. One of
        // those must not drop a rendered run's reports of the same text.
        let fillets = match st.sketches.get(&key) {
            Some(old) if log.fillets.count == 0 && *old.text == *text => old.fillets.clone(),
            _ => log.fillets.clone(),
        };
        let facts = sketch::Facts {
            text,
            sketches: log.sketches.clone(),
            fillets,
        };
        st.sketches.insert(key, facts);
    }

    /// Publish every due document whose text a supplied run read; the
    /// others stay due.
    fn publish_supplied(&self, session: &Session) -> Vec<String> {
        let (jobs, open, uris) = {
            let mut st = self.state();
            let mut jobs = Vec::new();
            let dirty = std::mem::take(&mut st.dirty);
            for uri in dirty {
                let run = st.docs.get(&uri).and_then(|d| {
                    let (text, diags) = st.supplied.get(&session::normal(&d.path))?;
                    (**text == *d.text).then(|| (d.clone(), diags.clone()))
                });
                match run {
                    Some((doc, diags)) => jobs.push((uri, doc, diags)),
                    None if st.docs.contains_key(&uri) => st.dirty.push(uri),
                    None => {}
                }
            }
            let (open, uris) = open_docs(&st);
            (jobs, open, uris)
        };
        let mut out = Vec::new();
        for (uri, doc, diags) in jobs {
            out.extend(self.publish_one(session, &uri, &doc, &diags, &open, &uris, None));
        }
        out
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
                // A version the host has already run arrived: its
                // diagnostics go out with nothing else to wait for.
                if self.host_diagnostics() {
                    catch_unwind(AssertUnwindSafe(|| self.publish_supplied(session)))
                        .unwrap_or_default()
                } else {
                    Vec::new()
                }
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
                let mut actions =
                    diagnose::code_actions(uri, ctx.file().source(), params, &published);
                let refactor = params
                    .pointer("/context/only")
                    .and_then(Value::as_array)
                    .is_none_or(|o| {
                        o.iter()
                            .any(|k| k.as_str().is_some_and(|k| k.starts_with("refactor")))
                    });
                if let (true, Some(range), Value::Array(list)) = (
                    refactor,
                    params
                        .get("range")
                        .and_then(|r| proto::offsets(ctx.file().source(), r)),
                    &mut actions,
                ) {
                    list.extend(sketch::pin_actions(
                        uri,
                        ctx.file().source(),
                        &ctx.file().path,
                        ctx.sketches(),
                        range,
                    ));
                    list.extend(sketch::pin_count_actions(
                        uri,
                        ctx.file().source(),
                        &ctx.file().path,
                        ctx.fillets(),
                        range,
                    ));
                }
                actions
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
                    if !st.docs.values().any(|d| d.path == doc.path) {
                        st.supplied.remove(&session::normal(&doc.path));
                        st.sketches.remove(&session::normal(&doc.path));
                    }
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
        let (doc, open, uris, sketches) = {
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
            let sketches = st.sketches.get(&session::normal(&path)).cloned();
            (st.docs.get(uri).cloned(), open, uris, sketches)
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
            sketches,
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
        let on = self.extensions_on(session);
        let sketch = on.has(session::Extension::Sketch);
        let query = on.has(session::Extension::Query);
        let fillet = on.has(session::Extension::Fillet);
        let mut slot = doc.world.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some((w, stamps)) = &*slot
            && w.sketch == sketch
            && w.query == query
            && w.fillet == fillet
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
        let mut w = World::new(doc.analyzed(), &loader);
        w.sketch = sketch;
        w.query = query;
        w.fillet = fillet;
        let w = Arc::new(w);
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
    /// document changed meanwhile is skipped (it is due again). With
    /// [`Options::host_diagnostics`] nothing is evaluated: only documents
    /// with a supplied run of their text are published.
    pub fn publish_diagnostics(&self, session: &Session) -> Vec<String> {
        if self.host_diagnostics() {
            return self.publish_supplied(session);
        }
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
            let (open, uris) = open_docs(&st);
            (jobs, open, uris)
        };
        let (limits, extensions) = {
            let o = self.opts.lock().unwrap_or_else(PoisonError::into_inner);
            (o.limits, o.extensions)
        };
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
            run.extensions = extensions;
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
            let diags = ev.log.diagnostics_json();
            self.keep_sketches(&doc.path, doc.text.clone(), &ev.log);
            out.extend(self.publish_one(session, &uri, &doc, &diags, &open, &uris, Some(&flag)));
        }
        out
    }

    /// The notifications for `doc`'s diagnostics `diags` (the session's
    /// JSON), recorded as its last published; nothing if the document
    /// changed meanwhile (or `flag`, its evaluation's, was set).
    #[allow(clippy::too_many_arguments)]
    fn publish_one(
        &self,
        session: &Session,
        uri: &str,
        doc: &Arc<Doc>,
        diags: &[Value],
        open: &HashMap<PathBuf, Arc<Doc>>,
        uris: &HashMap<PathBuf, String>,
        flag: Option<&AtomicBool>,
    ) -> Vec<String> {
        let world = self.world(session, doc, open);
        let fs = session.fs();
        let text_of = |p: &Path| -> Option<Arc<SourceFile>> {
            let t = match open.get(p) {
                Some(d) => d.text.to_vec(),
                None => fs.read(p).ok()?,
            };
            Some(Arc::new(SourceFile::new(p.to_path_buf(), t)))
        };
        let uri_for = |p: &Path| uris.get(p).cloned().unwrap_or_else(|| uri::from_path(p));
        let mut per_uri =
            diagnose::convert(&world, &session.config().libs.0, diags, &text_of, &uri_for);
        let mut out = Vec::new();
        let mut st = self.state();
        // "Pin drawing" on the sketch's own diagnostics, from the run of
        // this very text.
        if let Some(sk) = st
            .sketches
            .get(&session::normal(&doc.path))
            .and_then(|f| f.for_text(&doc.text))
            && let Some((_, list)) = per_uri.first_mut()
        {
            let main = world.main();
            sketch::attach_pins(list, main.source(), &main.path, sk);
        }
        let current = st.docs.get(uri).is_some_and(|d| Arc::ptr_eq(d, doc));
        if !current || flag.is_some_and(|f| f.load(Ordering::Relaxed)) {
            return out;
        }
        st.running.remove(uri);
        let now: HashSet<String> = per_uri.iter().skip(1).map(|(u, _)| u.clone()).collect();
        let before = st
            .published
            .insert(uri.to_string(), now.clone())
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
        out
    }

    /// The analysed files the shared cache holds (for tests and stats).
    pub fn cached_files(&self) -> usize {
        self.cache.len()
    }
}

/// The open documents by path, and the URIs they were opened under.
fn open_docs(st: &State) -> (HashMap<PathBuf, Arc<Doc>>, HashMap<PathBuf, String>) {
    let open = st
        .docs
        .values()
        .map(|d| (d.path.clone(), d.clone()))
        .collect();
    let uris = st
        .docs
        .iter()
        .map(|(u, d)| (d.path.clone(), u.clone()))
        .collect();
    (open, uris)
}

/// The server's capabilities.
fn initialize_result() -> Value {
    json!({
        "capabilities": {
            "positionEncoding": "utf-16",
            "textDocumentSync": {"openClose": true, "change": 2, "save": false},
            "hoverProvider": true,
            "completionProvider": {"triggerCharacters": ["$", "\""], "resolveProvider": false},
            "signatureHelpProvider": {"triggerCharacters": ["(", ","], "retriggerCharacters": [","]},
            "definitionProvider": true,
            "referencesProvider": true,
            "documentFormattingProvider": true,
            "documentRangeFormattingProvider": true,
            "documentSymbolProvider": true,
            "foldingRangeProvider": true,
            "renameProvider": {"prepareProvider": true},
            "codeActionProvider": {"codeActionKinds": ["quickfix", "refactor.rewrite"]},
        },
        "serverInfo": {"name": "neoscad", "version": env!("CARGO_PKG_VERSION")},
    })
}
