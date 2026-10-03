//! The app's side of the link: listen while the user allows agents, answer
//! each connected `neoscad mcp` from the open documents, and tell the app
//! who is connected and what they are doing.
//!
//! Threads, all started by [`AgentLink::start`] and none before: one
//! accepts; each connection has a reader and a writer (so the app's calls,
//! such as a focus change that every connection is told about, never wait
//! on a socket); each request runs on a thread of its own, since an edit
//! can wait minutes for the user's Apply while the agent reads or captures
//! in parallel. [`AgentLink::stop`] (or turning consent off, or dropping
//! the link) closes the socket and every connection.

use std::collections::HashMap;
use std::io::BufReader;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, mpsc};

use client::agent::{
    self, AgentClient, AgentDocuments, AgentHost, AgentStatus, Incoming, NOT_ALLOWED,
};
use serde_json::Value;

use crate::discovery::Rendezvous;
use crate::frame;
use crate::transport::{self, Address, Listener};

/// Told the link's state after every change: listening or not, who is
/// connected, what they are doing. Called on the link's threads (and on
/// the caller's for the app's own calls), with nothing locked; hand the
/// status to the UI thread without waiting for it. Statuses from
/// different threads can arrive out of order: keep the one with the
/// highest `sequence`.
pub trait AgentObserver: Send + Sync {
    fn status_changed(&self, status: AgentStatus, sequence: u64);
}

/// What the app says about itself in `hello`.
#[derive(Debug, Clone)]
pub struct LinkConfig {
    /// "NeoSCAD".
    pub app: String,
    /// The app's version, so a mismatched `neoscad` can say which is old.
    pub version: String,
    /// "macos", "linux", "windows".
    pub platform: String,
    /// Where to listen; `None` for this user's place
    /// ([`Rendezvous::from_env`]).
    pub rendezvous: Option<Rendezvous>,
}

/// At most this many agents at once: each is a `neoscad mcp`, and a dozen
/// is already far more than a person runs. A broken client that keeps
/// connecting cannot pile up threads past it.
const MAX_CLIENTS: usize = 8;
/// At most this many requests of one connection running at once; the
/// command line sends a handful in parallel at most.
const MAX_IN_FLIGHT: usize = 16;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The app's agent link (see the module documentation).
pub struct AgentLink {
    inner: Arc<Inner>,
}

impl std::fmt::Debug for AgentLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentLink")
            .field("status", &self.status())
            .finish()
    }
}

struct Inner {
    host: Arc<dyn AgentHost>,
    config: LinkConfig,
    observer: Mutex<Option<Arc<dyn AgentObserver>>>,
    allowed: AtomicBool,
    documents: Mutex<AgentDocuments>,
    state: Mutex<State>,
    next_client: AtomicU64,
    sequence: AtomicU64,
}

#[derive(Default)]
struct State {
    listening: Option<Listening>,
    clients: HashMap<u64, Arc<Client>>,
    error: Option<String>,
}

struct Listening {
    address: Address,
    stop: Arc<AtomicBool>,
}

/// One connected `neoscad mcp`.
struct Client {
    id: u64,
    name: Mutex<Option<String>>,
    /// Requests running now, by request id: what each is (for the status
    /// line), newest last.
    running: Mutex<Vec<(u64, &'static str, Option<u64>)>>,
    /// The bridge's own tool on the app's document (`activity` note).
    tool: Mutex<Option<(String, Option<u64>)>>,
    in_flight: AtomicUsize,
    /// It has said `welcome`: only then is it shown as connected. A
    /// process that connects and leaves at once (another app instance
    /// checking for a stale socket) never flickers in the status.
    welcomed: AtomicBool,
    out: mpsc::Sender<Out>,
}

enum Out {
    Message(Value),
    /// Send this (a `bye`), then end the connection.
    Close(Value),
}

impl Client {
    fn send(&self, msg: Value) {
        let _ = self.out.send(Out::Message(msg));
    }

    fn view(&self) -> AgentClient {
        let running = lock(&self.running);
        let tool = lock(&self.tool);
        let (activity, document) = match (running.last(), tool.as_ref()) {
            (Some((_, method, doc)), _) => (Some(agent::activity_text(method)), *doc),
            (None, Some((t, doc))) => (Some(agent::activity_text(t)), *doc),
            (None, None) => (None, None),
        };
        AgentClient {
            id: self.id,
            name: lock(&self.name).clone(),
            activity: activity.map(str::to_string),
            document,
        }
    }
}

impl AgentLink {
    /// A link for the app whose documents `host` reads and changes. Starts
    /// nothing: no thread, no socket, until [`AgentLink::start`].
    pub fn new(host: Arc<dyn AgentHost>, config: LinkConfig) -> AgentLink {
        AgentLink {
            inner: Arc::new(Inner {
                host,
                config,
                observer: Mutex::new(None),
                allowed: AtomicBool::new(false),
                documents: Mutex::new(AgentDocuments::default()),
                state: Mutex::new(State::default()),
                next_client: AtomicU64::new(1),
                sequence: AtomicU64::new(0),
            }),
        }
    }

    /// Tell `observer` about every later change (replacing any before).
    pub fn set_observer(&self, observer: Option<Arc<dyn AgentObserver>>) {
        *lock(&self.inner.observer) = observer;
        self.inner.changed();
    }

    /// Whether the user has allowed agents. Until this is true, `start`
    /// refuses and every request is refused; turning it off stops the
    /// link at once and tells each agent why.
    pub fn set_allowed(&self, allowed: bool) {
        self.inner.allowed.store(allowed, Ordering::SeqCst);
        if allowed {
            self.inner.changed();
        } else {
            self.inner
                .stop("the user turned off AI agents in NeoSCAD", false);
        }
    }

    /// Listen for agents, and give the address. Refused until the user
    /// has allowed agents ([`AgentLink::set_allowed`]). Listening already
    /// is not an error.
    pub fn start(&self) -> Result<String, String> {
        if !self.inner.allowed.load(Ordering::SeqCst) {
            return Err(NOT_ALLOWED.into());
        }
        let r = Inner::start(&self.inner);
        if let Err(e) = &r {
            lock(&self.inner.state).error = Some(e.clone());
        }
        self.inner.changed();
        r
    }

    /// Stop listening and end every connection (the agents are told so,
    /// and come back by themselves on the next [`AgentLink::start`]).
    pub fn stop(&self) {
        self.inner
            .stop("NeoSCAD stopped listening for agents", true);
    }

    /// End one agent's connection ("Disconnect"). It is asked not to
    /// reconnect to this app by itself; restarting the agent's session, or
    /// the app, connects again.
    pub fn disconnect(&self, client: u64) {
        let c = lock(&self.inner.state).clients.remove(&client);
        if let Some(c) = c {
            let _ = c.out.send(Out::Close(agent::bye(
                "the user disconnected this agent in NeoSCAD",
                false,
            )));
        }
        self.inner.changed();
    }

    pub fn status(&self) -> AgentStatus {
        self.inner.status()
    }

    /// A document was opened, or renamed or saved under another path
    /// (`file` is its name as the window shows it, `path` its file once
    /// saved). Cheap, and fine to call while the link is stopped: the
    /// list is kept for when it starts.
    pub fn document_opened(&self, id: u64, file: &str, path: Option<&str>) {
        if lock(&self.inner.documents).open(id, file, path) {
            self.inner.documents_changed();
        }
    }

    /// The user focused this document's window: requests without a
    /// `document` act on it from now on.
    pub fn document_focused(&self, id: u64) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
            .unwrap_or(0);
        if lock(&self.inner.documents).focus(id, now) {
            self.inner.documents_changed();
        }
    }

    pub fn document_closed(&self, id: u64) {
        if lock(&self.inner.documents).close(id) {
            self.inner.documents_changed();
        }
    }
}

impl Drop for AgentLink {
    fn drop(&mut self) {
        self.inner.stop("NeoSCAD closed", true);
    }
}

impl Inner {
    fn status(&self) -> AgentStatus {
        let state = lock(&self.state);
        let mut clients: Vec<AgentClient> = state
            .clients
            .values()
            .filter(|c| c.welcomed.load(Ordering::SeqCst))
            .map(|c| c.view())
            .collect();
        clients.sort_by_key(|c| c.id);
        AgentStatus {
            allowed: self.allowed.load(Ordering::SeqCst),
            listening: state.listening.is_some(),
            address: state
                .listening
                .as_ref()
                .map(|l| l.address.display().to_string()),
            clients,
            error: state.error.clone(),
        }
    }

    /// Tell the observer, with nothing locked.
    fn changed(&self) {
        let observer = lock(&self.observer).clone();
        if let Some(o) = observer {
            let status = self.status();
            let sequence = self.sequence.fetch_add(1, Ordering::SeqCst) + 1;
            o.status_changed(status, sequence);
        }
    }

    fn clients(&self) -> Vec<Arc<Client>> {
        lock(&self.state).clients.values().cloned().collect()
    }

    fn documents_changed(&self) {
        let note = agent::documents_note(lock(&self.documents).list());
        for c in self.clients() {
            c.send(note.clone());
        }
    }

    fn start(this: &Arc<Inner>) -> Result<String, String> {
        let mut state = lock(&this.state);
        if let Some(l) = &state.listening {
            return Ok(l.address.display().to_string());
        }
        let rendezvous = this
            .config
            .rendezvous
            .clone()
            .unwrap_or_else(Rendezvous::from_env);
        sweep(&rendezvous);
        let address = rendezvous.new_address();
        let listener = Listener::bind(&address, true)?;
        let stop = Arc::new(AtomicBool::new(false));
        state.listening = Some(Listening {
            address: address.clone(),
            stop: stop.clone(),
        });
        state.error = None;
        drop(state);
        let inner = this.clone();
        std::thread::Builder::new()
            .name("agent-link accept".into())
            .spawn(move || inner.accept(&listener, &stop))
            .map_err(|e| format!("cannot start the agent link: {e}"))?;
        Ok(address.display().to_string())
    }

    fn stop(&self, reason: &str, reconnect: bool) {
        let (listening, clients) = {
            let mut state = lock(&self.state);
            (
                state.listening.take(),
                state.clients.drain().map(|(_, c)| c).collect::<Vec<_>>(),
            )
        };
        let had_any = listening.is_some() || !clients.is_empty();
        for c in clients {
            let _ = c.out.send(Out::Close(agent::bye(reason, reconnect)));
        }
        if let Some(l) = listening {
            l.stop.store(true, Ordering::SeqCst);
            // Wake the accepting thread, which then sees `stop` and drops
            // the listener; then nothing is left at the address.
            drop(transport::connect(&l.address));
            transport::remove(&l.address);
        }
        if had_any {
            self.changed();
        }
    }

    fn accept(self: Arc<Self>, listener: &Listener, stop: &AtomicBool) {
        loop {
            let conn = listener.accept_from_user();
            if stop.load(Ordering::SeqCst) {
                return;
            }
            match conn {
                Ok(Some(conn)) => {
                    if lock(&self.state).clients.len() >= MAX_CLIENTS {
                        let mut w = conn.writer;
                        let _ = frame::write_message(
                            &mut w,
                            &agent::bye("too many agents are connected to NeoSCAD", true),
                        );
                        conn.closer.close();
                        continue;
                    }
                    let inner = self.clone();
                    let _ = std::thread::Builder::new()
                        .name("agent-link connection".into())
                        .spawn(move || inner.connection(conn));
                }
                // Another user's process: refused and closed.
                Ok(None) => {}
                Err(e) => {
                    // A listener that fails to accept will fail again; end
                    // rather than spin, and say why.
                    let mut state = lock(&self.state);
                    state.error = Some(format!("the agent link stopped accepting: {e}"));
                    if let Some(l) = state.listening.take() {
                        transport::remove(&l.address);
                    }
                    drop(state);
                    self.changed();
                    return;
                }
            }
        }
    }

    fn connection(self: Arc<Self>, conn: transport::Conn) {
        let (tx, rx) = mpsc::channel();
        let client = Arc::new(Client {
            id: self.next_client.fetch_add(1, Ordering::SeqCst),
            name: Mutex::new(None),
            running: Mutex::new(Vec::new()),
            tool: Mutex::new(None),
            in_flight: AtomicUsize::new(0),
            welcomed: AtomicBool::new(false),
            out: tx,
        });
        // The writer: everything this connection sends goes through it, in
        // order, so no caller waits on the socket. It ends when the last
        // sender goes (the client is forgotten and its requests are done)
        // or after a `bye`.
        let mut writer = conn.writer;
        let closer = conn.closer;
        let writer_thread = std::thread::Builder::new()
            .name("agent-link writer".into())
            .spawn(move || {
                for out in rx {
                    match out {
                        Out::Message(m) => {
                            if frame::write_message(&mut writer, &m).is_err() {
                                break;
                            }
                        }
                        Out::Close(m) => {
                            let _ = frame::write_message(&mut writer, &m);
                            break;
                        }
                    }
                }
                // Ends the reader too (on Unix; on Windows the peer closes
                // when it reads the `bye`).
                closer.close();
            });
        if writer_thread.is_err() {
            return;
        }
        {
            let mut state = lock(&self.state);
            if state.listening.is_none() {
                // Stopped while this one was being accepted.
                return;
            }
            state.clients.insert(client.id, client.clone());
        }
        let docs = lock(&self.documents).list().to_vec();
        client.send(agent::hello(
            &self.config.app,
            &self.config.version,
            &self.config.platform,
            &docs,
        ));
        self.changed();
        let mut reader = BufReader::new(conn.reader);
        while let Ok(Some(msg)) = frame::read_message(&mut reader) {
            if !lock(&self.state).clients.contains_key(&client.id) {
                break;
            }
            match agent::incoming(&msg) {
                Incoming::Welcome { client: name, .. } => {
                    *lock(&client.name) = name;
                    client.welcomed.store(true, Ordering::SeqCst);
                    self.changed();
                }
                Incoming::Activity { tool, document } => {
                    *lock(&client.tool) = tool.map(|t| (t, document));
                    self.changed();
                }
                Incoming::Request { id, method, params } => {
                    self.request(&client, id, method, params);
                }
                Incoming::Other => {}
            }
        }
        // The agent went away (or was disconnected): forget it, and end the
        // writer by dropping this connection's last sender.
        lock(&self.state).clients.remove(&client.id);
        self.changed();
    }

    fn request(self: &Arc<Self>, client: &Arc<Client>, id: u64, method: String, params: Value) {
        if client.in_flight.load(Ordering::SeqCst) >= MAX_IN_FLIGHT {
            client.send(agent::reply(
                id,
                Err("NeoSCAD is busy with this agent's other requests; try again".into()),
            ));
            return;
        }
        client.in_flight.fetch_add(1, Ordering::SeqCst);
        let inner = self.clone();
        let c = client.clone();
        let spawned = std::thread::Builder::new()
            .name("agent-link request".into())
            .spawn(move || {
                let what: &'static str = match method.as_str() {
                    "read" => "read",
                    "edit" => "edit",
                    "reveal" => "reveal",
                    "camera" => "camera",
                    "capture" => "capture",
                    "annotate" => "annotate",
                    "console" => "console",
                    _ => "other",
                };
                let document = params.get("document").and_then(Value::as_u64);
                let shown = what != "other";
                if shown {
                    lock(&c.running).push((id, what, document));
                    inner.changed();
                }
                // A snapshot of the documents, so the app's calls (from its
                // UI thread, which the host may be waiting on) never wait
                // for this request.
                let documents = lock(&inner.documents).clone();
                let name = lock(&c.name).clone();
                let allowed = inner.allowed.load(Ordering::SeqCst);
                let result = catch_unwind(AssertUnwindSafe(|| {
                    agent::handle_request(
                        inner.host.as_ref(),
                        &documents,
                        allowed,
                        name.as_deref(),
                        &method,
                        &params,
                    )
                }))
                .unwrap_or_else(|_| Err("NeoSCAD failed to answer (an internal error)".into()));
                c.send(agent::reply(id, result));
                c.in_flight.fetch_sub(1, Ordering::SeqCst);
                if shown {
                    lock(&c.running).retain(|r| r.0 != id);
                    inner.changed();
                }
            });
        if spawned.is_err() {
            client.in_flight.fetch_sub(1, Ordering::SeqCst);
            client.send(agent::reply(
                id,
                Err("NeoSCAD could not start the request".into()),
            ));
        }
    }
}

/// Remove sockets that apps which did not exit cleanly left in this
/// user's listening directory: nothing answers at them. Only names an app
/// listens at, only in its own directory, and only once nothing answers,
/// so a running app is never touched.
fn sweep(r: &Rendezvous) {
    if cfg!(unix) {
        for address in r.scan() {
            if address.parent() == Some(r.listen_dir()) && transport::is_stale(&address) {
                transport::remove(&address);
            }
        }
    }
}
