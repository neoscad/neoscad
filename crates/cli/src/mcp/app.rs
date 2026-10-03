//! The command line's side of the desktop apps' agent link
//! (docs/agent-bridge.md, "Desktop apps"; the app's side is
//! `crates/agent-link`): find every running NeoSCAD app that allows
//! agents, connect to each, and send it the editor and view tools'
//! requests, as the browser bridge sends them to the web page.
//!
//! Plain `neoscad mcp` does this unless `--no-app` says not to. It looks
//! once at startup, before answering anything, so the first `tools/list`
//! already includes the app's tools when an app is running; then a thread
//! looks again every [`RESCAN`] (a directory listing: nothing measurable),
//! so an app started, restarted or newly allowed later is connected
//! without the agent doing anything. Nothing here is keyed to a session or
//! a token, so app restarts and agent restarts both reconnect by
//! themselves.
//!
//! Documents: each app lists its open documents, most recently focused
//! first, with the time of that focus on the wall clock, so documents of
//! several app processes (the Windows app is one per window) order
//! together. The agent gets a number per document, stable while the app
//! runs; a request without one acts on the most recently focused of all.

use std::collections::{HashMap, HashSet};
use std::io::{BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError, mpsc};
use std::time::{Duration, Instant};

use agent_link::discovery::Rendezvous;
use agent_link::frame;
use agent_link::transport::{self, Closer};
use serde_json::{Value, json};

use super::roots::Roots;

/// How often the watcher looks for apps.
const RESCAN: Duration = Duration::from_secs(2);
/// How long a request waits for an app when none is connected: the user
/// may be restarting it, or allowing agents in it just now. As the web
/// bridge's wait for a tab.
pub const APP_GRACE: Duration = Duration::from_secs(5);
/// How long connecting waits for the app's `hello` at startup.
const HELLO_WAIT: Duration = Duration::from_millis(500);
/// While waiting for an app, look this often.
const POLL: Duration = Duration::from_millis(200);

/// The answer to an editor or view tool when no app is connected.
pub const NO_APP: &str = "no NeoSCAD app is connected: ask the user to open the model in the NeoSCAD app and allow AI agents there (its \"Connect your AI agent\" control). Or give path or source to work on files";

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A document open in a connected app.
#[derive(Debug, Clone, PartialEq)]
pub struct AppDocument {
    /// The agent's number for it.
    pub number: u64,
    /// The app's id for it.
    id: u64,
    pub file: String,
    pub path: Option<String>,
    focused: u64,
    conn: u64,
}

/// The running apps this server is connected to.
pub struct Apps {
    rendezvous: Rendezvous,
    roots: Roots,
    state: Mutex<State>,
    /// Signalled with `state` held whenever an app connects, says hello,
    /// lists its documents or goes.
    changed: Condvar,
    next_request: AtomicU64,
    next_conn: AtomicU64,
    /// The MCP client's name, for the app's "Claude Code connected".
    client: Mutex<Option<String>>,
    /// Told when the first app connects or the last one goes, so the
    /// server can send `notifications/tools/list_changed`.
    on_presence: Mutex<Option<Box<dyn Fn() + Send + Sync>>>,
    /// An app has been connected in this session (`Apps::seen`).
    seen: AtomicBool,
}

impl std::fmt::Debug for Apps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Apps")
            .field("rendezvous", &self.rendezvous)
            .finish_non_exhaustive()
    }
}

#[derive(Default)]
struct State {
    conns: Vec<Arc<Conn>>,
    /// Addresses not to connect to again: the app said `bye` with
    /// `reconnect: false` (the user disconnected this agent there).
    refused: HashSet<PathBuf>,
    /// Addresses that failed the owner check, already reported on stderr.
    untrusted: HashSet<PathBuf>,
    /// The agent's document numbers, by app address and app id.
    numbers: HashMap<(PathBuf, u64), u64>,
    next_number: u64,
}

/// One connected app.
struct Conn {
    serial: u64,
    address: PathBuf,
    out: Mutex<Box<dyn Write + Send>>,
    closer: Closer,
    pending: Mutex<HashMap<u64, mpsc::Sender<Result<Value, String>>>>,
    /// The app's `hello` (app, version, platform), once it has said it.
    hello: Mutex<Option<Value>>,
    documents: Mutex<Vec<AppDocument>>,
}

impl Conn {
    fn send(&self, msg: &Value) -> bool {
        frame::write_message(lock(&self.out).as_mut(), msg).is_ok()
    }

    fn ready(&self) -> bool {
        lock(&self.hello).is_some()
    }
}

impl Apps {
    /// Look for apps now (connecting to each, and waiting briefly for its
    /// documents), then keep looking on a thread of its own.
    pub fn start(rendezvous: Rendezvous, roots: Roots) -> Arc<Apps> {
        let apps = Arc::new(Apps {
            rendezvous,
            roots,
            state: Mutex::new(State {
                next_number: 1,
                ..State::default()
            }),
            changed: Condvar::new(),
            next_request: AtomicU64::new(1),
            next_conn: AtomicU64::new(1),
            client: Mutex::new(None),
            on_presence: Mutex::new(None),
            seen: AtomicBool::new(false),
        });
        apps.scan();
        apps.wait_for_hellos(HELLO_WAIT);
        let watcher = Arc::downgrade(&apps);
        let _ = std::thread::Builder::new()
            .name("mcp app watcher".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(RESCAN);
                    // Ends with the server.
                    let Some(apps) = watcher.upgrade() else {
                        return;
                    };
                    apps.scan();
                }
            });
        apps
    }

    /// Call `f` whenever the first app connects or the last one goes.
    pub fn on_presence(&self, f: Box<dyn Fn() + Send + Sync>) {
        *lock(&self.on_presence) = Some(f);
    }

    /// The MCP client's name: tell every app (a `welcome` again).
    pub fn set_client(&self, name: &str) {
        *lock(&self.client) = Some(name.to_string());
        for c in self.conns() {
            c.send(&self.welcome());
        }
    }

    fn welcome(&self) -> Value {
        json!({
            "type": "welcome",
            "client": lock(&self.client).clone(),
            "server": format!("neoscad {}", env!("CARGO_PKG_VERSION")),
            "protocol": neoscad_client::agent::AGENT_PROTOCOL,
        })
    }

    fn conns(&self) -> Vec<Arc<Conn>> {
        lock(&self.state).conns.clone()
    }

    /// Whether an app is connected and has said who it is.
    pub fn connected(&self) -> bool {
        self.conns().iter().any(|c| c.ready())
    }

    /// Whether an app has been connected at some point in this session.
    pub fn seen(&self) -> bool {
        self.seen.load(Ordering::SeqCst)
    }

    /// Connect to every app listening that is not connected yet.
    fn scan(self: &Arc<Self>) {
        let (known, refused): (HashSet<PathBuf>, HashSet<PathBuf>) = {
            let s = lock(&self.state);
            (
                s.conns.iter().map(|c| c.address.clone()).collect(),
                s.refused.clone(),
            )
        };
        for address in self.rendezvous.scan() {
            if !known.contains(&address) && !refused.contains(&address) {
                self.connect(address);
            }
        }
    }

    fn connect(self: &Arc<Self>, address: PathBuf) {
        // The owner check comes first: nothing is sent to a socket or pipe
        // that is not this user's.
        let conn = match transport::connect(&address) {
            Ok(conn) => conn,
            // Gone since the listing (an app that just quit): nothing to say.
            Err(transport::ConnectError::NoServer) => return,
            // Said once per address (the scan retries every RESCAN, and a
            // directory's permissions may yet be fixed). Skipped silently,
            // a user whose app was running could not tell why the agent
            // never saw it.
            Err(transport::ConnectError::Untrusted(why)) => {
                if lock(&self.state).untrusted.insert(address.clone()) {
                    eprintln!(
                        "neoscad mcp: not connecting to {}: {why}",
                        address.display()
                    );
                }
                return;
            }
        };
        let c = Arc::new(Conn {
            serial: self.next_conn.fetch_add(1, Ordering::SeqCst),
            address,
            out: Mutex::new(conn.writer),
            closer: conn.closer,
            pending: Mutex::new(HashMap::new()),
            hello: Mutex::new(None),
            documents: Mutex::new(Vec::new()),
        });
        if !c.send(&self.welcome()) {
            eprintln!(
                "neoscad mcp: the NeoSCAD app at {} closed the connection at once",
                c.address.display()
            );
            return;
        }
        lock(&self.state).conns.push(c.clone());
        let apps = self.clone();
        let reader = conn.reader;
        let spawned = std::thread::Builder::new()
            .name("mcp app connection".into())
            .spawn(move || apps.read(&c, BufReader::new(reader)));
        if spawned.is_err() {
            eprintln!("neoscad mcp: cannot read from the NeoSCAD app");
        }
    }

    /// Wait until every connected app has said hello, or `timeout`.
    fn wait_for_hellos(&self, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        let mut s = lock(&self.state);
        while s.conns.iter().any(|c| !c.ready()) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return;
            }
            s = self
                .changed
                .wait_timeout(s, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    fn read(&self, c: &Arc<Conn>, mut r: BufReader<Box<dyn std::io::Read + Send>>) {
        loop {
            let msg = match frame::read_message(&mut r) {
                Ok(Some(m)) => m,
                Ok(None) => break,
                Err(e) => {
                    eprintln!("neoscad mcp: the NeoSCAD app's connection ended: {e}");
                    break;
                }
            };
            if let Some(id) = msg.get("id").and_then(Value::as_u64) {
                let Some(tx) = lock(&c.pending).remove(&id) else {
                    continue;
                };
                let reply = match msg.get("error") {
                    Some(e) => Err(e["message"]
                        .as_str()
                        .unwrap_or("the NeoSCAD app could not do it")
                        .to_string()),
                    None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                };
                let _ = tx.send(reply);
                continue;
            }
            match msg["type"].as_str() {
                Some("hello") => {
                    let first = !self.connected();
                    self.seen.store(true, Ordering::SeqCst);
                    if msg["protocol"].as_u64()
                        != Some(u64::from(neoscad_client::agent::AGENT_PROTOCOL))
                    {
                        eprintln!(
                            "neoscad mcp: the NeoSCAD app at {} speaks agent protocol {} and this neoscad {}: update the older one",
                            c.address.display(),
                            msg["protocol"],
                            neoscad_client::agent::AGENT_PROTOCOL
                        );
                    }
                    self.set_documents(c, &msg["documents"]);
                    *lock(&c.hello) = Some(msg);
                    self.notify();
                    if first {
                        self.presence();
                    }
                }
                Some("documents") => {
                    self.set_documents(c, &msg["documents"]);
                    self.notify();
                }
                Some("bye") => {
                    eprintln!(
                        "neoscad mcp: the NeoSCAD app ended the connection: {}",
                        msg["reason"].as_str().unwrap_or("no reason given")
                    );
                    if msg["reconnect"] == false {
                        lock(&self.state).refused.insert(c.address.clone());
                    }
                    break;
                }
                _ => {}
            }
        }
        c.closer.close();
        let had = self.connected();
        lock(&self.state).conns.retain(|x| x.serial != c.serial);
        // Dropping the senders ends every wait on this app at once.
        lock(&c.pending).clear();
        self.update_roots();
        self.notify();
        if had && !self.connected() {
            self.presence();
        }
    }

    fn set_documents(&self, c: &Conn, list: &Value) {
        let mut docs = Vec::new();
        // Numbers are given in the order of the app's ids (the order it
        // opened them, as apps count), not of focus: "document 1" is the
        // first one the agent saw opened, whichever the user is in.
        let mut listed: Vec<&Value> = list.as_array().into_iter().flatten().collect();
        listed.sort_by_key(|d| d["id"].as_u64());
        {
            let mut s = lock(&self.state);
            for d in listed {
                let Some(id) = d["id"].as_u64() else { continue };
                let key = (c.address.clone(), id);
                let number = match s.numbers.get(&key) {
                    Some(n) => *n,
                    None => {
                        let n = s.next_number;
                        s.next_number += 1;
                        s.numbers.insert(key, n);
                        n
                    }
                };
                docs.push(AppDocument {
                    number,
                    id,
                    file: d["file"].as_str().unwrap_or("Untitled").to_string(),
                    path: d["path"].as_str().map(str::to_string),
                    focused: d["focused"].as_u64().unwrap_or(0),
                    conn: c.serial,
                });
            }
        }
        *lock(&c.documents) = docs;
        self.update_roots();
    }

    /// Read access follows the open documents' directories.
    fn update_roots(&self) {
        let dirs: Vec<PathBuf> = self
            .documents()
            .iter()
            .filter_map(|d| d.path.as_ref())
            .filter_map(|p| PathBuf::from(p).parent().map(PathBuf::from))
            .collect();
        self.roots.set_document_dirs(&dirs);
    }

    fn notify(&self) {
        let _s = lock(&self.state);
        self.changed.notify_all();
    }

    fn presence(&self) {
        if let Some(f) = lock(&self.on_presence).as_ref() {
            f();
        }
    }

    /// Every connected app's documents, most recently focused first.
    pub fn documents(&self) -> Vec<AppDocument> {
        let mut all: Vec<AppDocument> = self
            .conns()
            .iter()
            .flat_map(|c| lock(&c.documents).clone())
            .collect();
        all.sort_by(|a, b| b.focused.cmp(&a.focused).then(a.number.cmp(&b.number)));
        all
    }

    /// Wait up to `timeout` for an app that has said hello.
    fn wait_for_app(self: &Arc<Self>, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.connected() {
                return true;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            self.scan();
            let s = lock(&self.state);
            drop(
                self.changed
                    .wait_timeout(s, left.min(POLL))
                    .unwrap_or_else(PoisonError::into_inner),
            );
        }
    }

    /// The document a request acts on: number `document`, else the most
    /// recently focused of every app's.
    pub fn target(self: &Arc<Self>, document: Option<u64>) -> Result<AppDocument, String> {
        if !self.wait_for_app(APP_GRACE) {
            return Err(NO_APP.into());
        }
        let docs = self.documents();
        match document {
            Some(n) => docs.into_iter().find(|d| d.number == n).ok_or_else(|| {
                format!("document {n} is not open in NeoSCAD (editor_read lists the open ones)")
            }),
            None => docs
                .into_iter()
                .next()
                .ok_or_else(|| "NeoSCAD has no document open".to_string()),
        }
    }

    /// Ask the app with `doc` open to do `method` on it, and wait up to
    /// `timeout` for its answer.
    pub fn request(
        &self,
        doc: &AppDocument,
        method: &str,
        mut params: Value,
        timeout: Duration,
    ) -> Result<Value, String> {
        let gone = || "the NeoSCAD app disconnected before it answered".to_string();
        let c = self
            .conns()
            .into_iter()
            .find(|c| c.serial == doc.conn)
            .ok_or_else(gone)?;
        let id = self.next_request.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        lock(&c.pending).insert(id, tx);
        params["document"] = json!(doc.id);
        if !c.send(&json!({"id": id, "method": method, "params": params})) {
            lock(&c.pending).remove(&id);
            return Err(gone());
        }
        match rx.recv_timeout(timeout) {
            Ok(r) => r,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                lock(&c.pending).remove(&id);
                Err(format!(
                    "the NeoSCAD app did not answer {method} within {} s (is it showing a dialog, or busy?)",
                    timeout.as_secs()
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(gone()),
        }
    }

    /// Tell the app with `doc` open that this server is running `tool` on
    /// it, until the guard goes.
    pub fn activity(self: &Arc<Self>, doc: &AppDocument, tool: &str) -> Activity {
        let conn = self.conns().into_iter().find(|c| c.serial == doc.conn);
        if let Some(c) = &conn {
            c.send(&json!({"type": "activity", "tool": tool, "document": doc.id}));
        }
        Activity(conn)
    }
}

/// The app is shown an activity while this lives.
pub struct Activity(Option<Arc<Conn>>);

impl Drop for Activity {
    fn drop(&mut self) {
        if let Some(c) = &self.0 {
            c.send(&json!({"type": "activity", "tool": null}));
        }
    }
}

impl std::fmt::Debug for Activity {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Activity")
    }
}
