//! The editor's language server: the core's (`crates/lsp`), one per
//! editor page, reached from the page's `@codemirror/lsp-client` through
//! the bridge's `lsp` message. The Linux port of
//! `apple/App/Editor/LanguageClient.swift` and
//! `windows/NeoSCAD.Host/LanguageBridge.cs`, calling `lsp` directly
//! rather than through UniFFI (`crates/ffi/src/language.rs`).
//!
//! Threads. Messages are handled in order on a worker thread of the
//! editor's own, never on the GTK main thread: an answer takes a
//! millisecond or two, but the first request on a BOSL2 model indexes the
//! library, which would freeze the window. The worker hands each answer
//! to the host's [`Sink`], which moves it to the main thread in the order
//! it was given; the page's client matches replies by id but applies
//! notifications (markers) in arrival order, so the order matters.
//!
//! Diagnostics. A document window's server is made with
//! `host_diagnostics`: it never evaluates, and its markers come from the
//! document's own runs ([`crate::run::run_document`] supplies each run's
//! diagnostics and gives the publications to the same sink). Such a
//! server never has diagnostics pending (`lsp::Server::diagnostics_pending`
//! is false with `host_diagnostics`), so unlike the macOS and Windows
//! bridges there is no debounce timer here: a version the client sends
//! after its run was supplied is published from `handle`, by the worker.
//! A library file shown read-only uses the same kind of server and is
//! never supplied a run, so it shows no markers for code the user cannot
//! change, and nothing is ever evaluated for it.
//!
//! Every server shares the process's [`lsp::Cache`] of analysed library
//! files, so BOSL2 is indexed once however many windows use it.

use std::panic::AssertUnwindSafe;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc};

use client::Client;
use serde_json::Value;

/// Where the server's messages go: the host moves them to the page, in
/// the order given. Called on the worker thread and on a run's engine
/// thread, never on the main thread.
pub type Sink = Arc<dyn Fn(Vec<String>) + Send + Sync>;

/// One editor's language server and its worker. Clones share both; the
/// worker ends when [`Language::stop`] is called or the last clone drops.
#[derive(Clone)]
pub struct Language {
    inner: Arc<Inner>,
}

struct Inner {
    server: Arc<lsp::Server>,
    client: Arc<Client>,
    sink: Sink,
    /// The worker's queue; `None` once stopped.
    queue: Mutex<Option<mpsc::Sender<String>>>,
    stopped: Arc<AtomicBool>,
}

impl std::fmt::Debug for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Language")
            .field("stopped", &self.is_stopped())
            .finish_non_exhaustive()
    }
}

impl Language {
    /// A server over `client`'s session, sharing `cache`, delivering to
    /// `sink`. The worker thread has the evaluator's stack: parsing
    /// recurses over the text, and a default thread's 2 MiB would
    /// overflow on a deeply nested file (as `LanguageServer::handle` in
    /// `crates/ffi` runs under `eval::with_stack`).
    pub fn start(client: Arc<Client>, cache: Arc<lsp::Cache>, sink: Sink) -> Language {
        let options = lsp::Options {
            // The window keeps the session's copy of the document itself
            // (`Client::edit`); a server writing the client's versions
            // into the session would fight it.
            sync_session: false,
            host_diagnostics: true,
            limits: None,
            ..lsp::Options::default()
        };
        let server = Arc::new(lsp::Server::with_cache(options, cache));
        let (tx, rx) = mpsc::channel::<String>();
        let stopped = Arc::new(AtomicBool::new(false));
        let (s, c, k, st) = (
            server.clone(),
            client.clone(),
            sink.clone(),
            stopped.clone(),
        );
        let spawned = std::thread::Builder::new()
            .name("neoscad-lsp".into())
            .stack_size(eval::DEFAULT_THREAD_STACK)
            .spawn(move || {
                for message in rx {
                    if st.load(Ordering::SeqCst) {
                        break;
                    }
                    // The server answers a request that panics with an
                    // internal error itself; this only keeps the worker
                    // (and so every later answer) alive if something
                    // outside a request does.
                    let out = std::panic::catch_unwind(AssertUnwindSafe(|| {
                        s.handle(&c.session, &message)
                    }))
                    .unwrap_or_default();
                    if !out.is_empty() && !st.load(Ordering::SeqCst) {
                        k(out);
                    }
                }
            });
        // Without a thread (the system refused one) every request would
        // wait for its timeout; answering "method not found" at once is
        // what the other apps do before their server starts.
        let queue = match spawned {
            Ok(_) => Some(tx),
            Err(_) => None,
        };
        Language {
            inner: Arc::new(Inner {
                server,
                client,
                sink,
                queue: Mutex::new(queue),
                stopped,
            }),
        }
    }

    /// A message from the page for the server; its answers go to the sink.
    pub fn send(&self, message: String) {
        if self.is_stopped() {
            return;
        }
        let queue = self
            .inner
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let refused = match queue.as_ref() {
            Some(tx) => tx.send(message).err().map(|e| e.0),
            None => Some(message),
        };
        drop(queue);
        if let Some(m) = refused
            && let Some(reply) = not_found_reply(&m)
        {
            (self.inner.sink)(vec![reply]);
        }
    }

    /// Hand the server a run of the document at `path`: the exact text it
    /// read and its log (the diagnostics, and the sketches hover and "Pin
    /// drawing" use), and deliver the markers that are due now (for the
    /// client's version with that text; later, from `handle`, if the
    /// client has not sent it yet).
    pub fn supply(&self, path: &Path, text: Arc<[u8]>, log: &session::Log) {
        if self.is_stopped() {
            return;
        }
        // Publishing parses the document (and its includes) for the
        // markers' positions when the worker has not yet, so it needs the
        // evaluator's stack as the worker's `handle` does; the caller is
        // a run's thread.
        let out = eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
            self.inner
                .server
                .supply_log(&self.inner.client.session, path, text, log)
        });
        if !out.is_empty() && !self.is_stopped() {
            (self.inner.sink)(out);
        }
    }

    /// The `--enable` names the window runs its document with
    /// (Preferences > Language, `crate::extensions`): with `sketch`,
    /// sketch bodies bind the sketch vocabulary for completion, hover and
    /// navigation. OpenSCAD's feature names are ignored here.
    pub fn set_enable(&self, names: &[String]) {
        self.inner
            .server
            .set_extensions(eval::Extensions::from_names(names));
    }

    /// The editor closed or its page went away: deliver nothing more. The
    /// worker ends once it sees the flag or its queue close.
    pub fn stop(&self) {
        self.inner.stopped.store(true, Ordering::SeqCst);
        self.inner
            .queue
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
    }

    pub fn is_stopped(&self) -> bool {
        self.inner.stopped.load(Ordering::SeqCst)
    }
}

/// The `file://` URI the language server knows a document by: the core's
/// path for it (an untitled document's too; the server never reads it
/// from disk, since the client sends its text).
pub fn document_uri(core_path: &str) -> String {
    lsp::uri::from_path(Path::new(core_path))
}

/// JSON-RPC's "method not found" for a request (a message with an `id`
/// and a `method`) that no server will answer, so the page's client does
/// not wait for its timeout; `None` for a notification or a reply.
pub fn not_found_reply(message: &str) -> Option<String> {
    let v: Value = serde_json::from_str(message).ok()?;
    let id = v.get("id")?;
    v.get("method")?;
    Some(
        serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": -32601, "message": "The language server is not running"},
        })
        .to_string(),
    )
}

/// A message in a line, for the debug log (`G_MESSAGES_DEBUG=neoscad`):
/// the method, or which request a reply answers, and for the two the
/// smoke test looks for, what they carry (`initialize`'s capabilities,
/// how many markers a publication has).
pub fn describe(message: &str) -> String {
    let Ok(v) = serde_json::from_str::<Value>(message) else {
        return "not JSON".into();
    };
    let id = v.get("id").map(Value::to_string);
    if let Some(method) = v.get("method").and_then(Value::as_str) {
        if method == "textDocument/publishDiagnostics" {
            let n = v
                .pointer("/params/diagnostics")
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            let uri = v
                .pointer("/params/uri")
                .and_then(Value::as_str)
                .unwrap_or("");
            return format!("{method} {uri} ({n} diagnostics)");
        }
        return match id {
            Some(id) => format!("{method} #{id}"),
            None => method.to_string(),
        };
    }
    let id = id.unwrap_or_else(|| "?".into());
    if let Some(e) = v.get("error") {
        return format!("error #{id}: {}", e.get("message").unwrap_or(&Value::Null));
    }
    if let Some(caps) = v.pointer("/result/capabilities").and_then(Value::as_object) {
        let mut names: Vec<&str> = caps.keys().map(String::as_str).collect();
        names.sort_unstable();
        return format!("reply #{id}: capabilities {}", names.join(" "));
    }
    format!("reply #{id}")
}

/// Where a definition in another file opens (the macOS app's rule,
/// `apple/App/Editor/LibraryViewer.swift`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A file of the user's own (on disk, writable, outside the library
    /// directories): a document window like any other, editable.
    Document(PathBuf),
    /// A library file (under a library directory, bundled, or not
    /// writable): read-only in a viewer, because editing BOSL2 in place
    /// from a jump would change every model that uses it. The bundled
    /// MCAD exists only in the core's memory, so the viewer reads it
    /// through the core (`Client::read_file`).
    Library(PathBuf),
}

/// Where `uri` opens, given the library directories in search order and
/// whether a path is a writable file on disk; `None` for a URI that is
/// not a file's.
pub fn target(
    uri: &str,
    library_dirs: &[String],
    writable_file: impl Fn(&Path) -> bool,
) -> Option<Target> {
    let path = lsp::uri::to_path(uri)?;
    let in_library = library_dirs
        .iter()
        .any(|d| path.starts_with(Path::new(d)) && path != Path::new(d));
    Some(if !in_library && writable_file(&path) {
        Target::Document(path)
    } else {
        Target::Library(path)
    })
}

/// A library file's name as a reader knows it: relative to its library
/// directory (`BOSL2/shapes3d.scad`), else in full.
pub fn display_path(path: &Path, library_dirs: &[String]) -> String {
    library_dirs
        .iter()
        .find_map(|d| path.strip_prefix(d).ok())
        .filter(|rest| !rest.as_os_str().is_empty())
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    /// A client without a GPU, under the agent limits.
    pub(crate) fn client() -> Arc<Client> {
        let mut cfg = crate::host::config();
        cfg.gpu = None;
        cfg.limits = session::Limits::AGENT;
        Arc::new(Client::new(cfg))
    }

    /// A server over `client` whose deliveries arrive on a channel, as
    /// the GTK sink forwards them to the main loop.
    pub(crate) fn server_over(client: Arc<Client>) -> (Language, mpsc::Receiver<Vec<String>>) {
        let (tx, rx) = mpsc::channel();
        let tx = Mutex::new(tx);
        let sink: Sink = Arc::new(move |m| {
            let _ = tx.lock().unwrap().send(m);
        });
        (Language::start(client, Arc::default(), sink), rx)
    }

    fn server() -> (Language, mpsc::Receiver<Vec<String>>) {
        server_over(client())
    }

    pub(crate) fn next(rx: &mpsc::Receiver<Vec<String>>) -> Vec<Value> {
        rx.recv_timeout(Duration::from_secs(30))
            .expect("an answer")
            .iter()
            .map(|m| serde_json::from_str(m).unwrap())
            .collect()
    }

    pub(crate) fn msg(v: Value) -> String {
        v.to_string()
    }

    #[test]
    fn initialize_gets_real_capabilities_and_answers_come_in_order() {
        let (ls, rx) = server();
        ls.send(msg(
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"capabilities": {}}}),
        ));
        ls.send(msg(
            json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
        ));
        ls.send(msg(
            json!({"jsonrpc": "2.0", "id": 2, "method": "shutdown"}),
        ));
        let first = next(&rx);
        assert_eq!(first[0]["id"], 1);
        let caps = &first[0]["result"]["capabilities"];
        assert!(caps.get("completionProvider").is_some(), "{caps}");
        assert!(caps.get("definitionProvider").is_some(), "{caps}");
        assert!(describe(&first[0].to_string()).contains("completionProvider"));
        // `initialized` has no answer; `shutdown` is the next delivery.
        assert_eq!(next(&rx)[0]["id"], 2);
        ls.stop();
    }

    #[test]
    fn a_stopped_server_delivers_nothing() {
        let (ls, rx) = server();
        ls.stop();
        ls.send(msg(
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"capabilities": {}}}),
        ));
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
    }

    #[test]
    fn requests_without_a_server_are_not_found_and_notifications_get_nothing() {
        let r: Value = serde_json::from_str(
            &not_found_reply(r#"{"jsonrpc":"2.0","id":"a","method":"textDocument/hover"}"#)
                .unwrap(),
        )
        .unwrap();
        assert_eq!(r["id"], "a");
        assert_eq!(r["error"]["code"], -32601);
        assert!(not_found_reply(r#"{"jsonrpc":"2.0","method":"initialized"}"#).is_none());
        assert!(not_found_reply(r#"{"jsonrpc":"2.0","id":1,"result":null}"#).is_none());
        assert!(not_found_reply("not json").is_none());
    }

    #[test]
    fn messages_are_described_for_the_log() {
        let p = json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
            "params": {"uri": "file:///a.scad", "diagnostics": [{}, {}]}});
        assert_eq!(
            describe(&p.to_string()),
            "textDocument/publishDiagnostics file:///a.scad (2 diagnostics)"
        );
        assert_eq!(
            describe(r#"{"jsonrpc":"2.0","id":3,"method":"textDocument/hover"}"#),
            "textDocument/hover #3"
        );
        assert_eq!(
            describe(r#"{"jsonrpc":"2.0","id":3,"result":null}"#),
            "reply #3"
        );
    }

    #[test]
    fn uris_are_the_core_paths_escaped() {
        assert_eq!(
            document_uri("/home/a b/m.scad"),
            "file:///home/a%20b/m.scad"
        );
    }

    #[test]
    fn users_files_open_as_documents_and_libraries_read_only() {
        let libs = vec![
            "/usr/share/openscad/libraries".to_string(),
            "/mcad".to_string(),
        ];
        let writable = |p: &Path| p.starts_with("/home");
        assert_eq!(
            target("file:///home/me/part.scad", &libs, writable),
            Some(Target::Document("/home/me/part.scad".into()))
        );
        // Not writable (or not on disk): read-only.
        assert_eq!(
            target("file:///opt/x.scad", &libs, writable),
            Some(Target::Library("/opt/x.scad".into()))
        );
        // Under a library directory, even if writable.
        let all = |_: &Path| true;
        assert_eq!(
            target("file:///mcad/gears.scad", &libs, all),
            Some(Target::Library("/mcad/gears.scad".into()))
        );
        // A sibling whose name only starts like a library directory.
        assert_eq!(
            target("file:///mcad2/g.scad", &libs, all),
            Some(Target::Document("/mcad2/g.scad".into()))
        );
        assert_eq!(target("https://example.org/x.scad", &libs, all), None);
        assert_eq!(
            display_path(
                Path::new("/usr/share/openscad/libraries/BOSL2/std.scad"),
                &libs
            ),
            "BOSL2/std.scad"
        );
        assert_eq!(display_path(Path::new("/opt/x.scad"), &libs), "/opt/x.scad");
    }
}
