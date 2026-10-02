//! The browser bridge of `neoscad mcp --browser`: a WebSocket on
//! 127.0.0.1 that the web demo's tab (neoscad.org/try) connects to, so the
//! agent's tools can read and edit the text the user is editing and see
//! and point at the 3D view (docs/agent-bridge.md).
//!
//! The server picks a free port and a random token; the link the user
//! opens (`https://neoscad.org/try/#connect=PORT.TOKEN`) carries both, in
//! the fragment, which browsers never send to the site. A connection is
//! accepted only with that token, a `Host` of this machine's loopback
//! address and port (so a DNS-rebound page cannot reach it under another
//! name), and an `Origin` of the page's own origin or of this server
//! itself (the relay page below). One tab at a time: a new one replaces
//! the old, which is told so, because the likeliest second tab is the same
//! user reloading or reopening the link, and refusing it would strand them
//! behind a tab that may already be gone.
//!
//! Some browsers will not let an https page open `ws://127.0.0.1`: WebKit
//! blocks it as mixed content, and Chrome asks the user first (Local
//! Network Access) and blocks it if they say no. For those the page opens
//! `/relay` from this server in a small window: that page is on this
//! server's origin, so its WebSocket is neither mixed content nor a
//! request to the local network, and it passes messages to and from the
//! tab with `postMessage`, to the page's origin only.
//!
//! Synchronous threads, as in the rest of the command line: one accepts,
//! one serves each connection. The tab's socket has one owner thread that
//! alternates between sending queued requests and reading with a short
//! timeout, since a tungstenite socket cannot be split between a reader
//! and a writer.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tungstenite::protocol::frame::CloseFrame;
use tungstenite::protocol::frame::coding::CloseCode;
use tungstenite::protocol::{Role, WebSocket, WebSocketConfig};
use tungstenite::{Error as WsError, Message};

/// The page the link opens unless `--browser-url` names another.
pub const DEFAULT_PAGE: &str = "https://neoscad.org/try/";

/// What a request's head may be: a browser's WebSocket handshake is well
/// under this.
const MAX_HEAD: usize = 16 * 1024;
/// How long a connection may take to send its request head.
const HEAD_TIMEOUT: Duration = Duration::from_secs(5);
/// How long the tab's owner thread waits for a message before it looks
/// for requests to send: the most a request waits to go out.
const POLL: Duration = Duration::from_millis(15);
/// The largest message a tab may send: a capture's PNG in base64 with room
/// to spare. A tab never needs more, and a bound keeps a broken one from
/// growing this process.
const MAX_MESSAGE: usize = 32 << 20;
/// The close code a replaced tab gets (4000-4999 are the application's).
const REPLACED: u16 = 4001;
/// How long a request waits for a tab when none is connected: the user
/// may be reloading the page, or still on the way through its connection
/// window, and failing at once leaves the agent nothing to do but retry.
const TAB_GRACE: Duration = Duration::from_secs(5);
/// The most `browser_connect` waits for a tab (its `wait_seconds`).
pub const MAX_WAIT: Duration = Duration::from_secs(120);

/// The answer to a tool call that needs a tab when none is connected.
pub const NOT_CONNECTED: &str = "no NeoSCAD web page is connected: call browser_connect and give the user its link to open (or paste into the page's \"Connect your AI agent\" panel)";

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[derive(Debug)]
pub struct Bridge {
    port: u16,
    token: String,
    /// The page's URL without a fragment, and its origin.
    page: String,
    page_origin: String,
    tab: Mutex<Option<Arc<Tab>>>,
    /// Signalled with `tab` held whenever a tab connects, says hello or
    /// goes, for the calls waiting for one ([`Bridge::wait_for_tab`]).
    tab_changed: Condvar,
    generation: AtomicU64,
    next_id: AtomicU64,
    /// The MCP client's name (`initialize`'s `clientInfo`), for the page's
    /// "connected to ..." line.
    client: Mutex<Option<String>>,
}

/// The connected tab.
#[derive(Debug)]
struct Tab {
    generation: u64,
    out: mpsc::Sender<Outgoing>,
    pending: Mutex<HashMap<u64, mpsc::Sender<Result<Value, String>>>>,
    /// The tab's `hello`: its page, browser and file.
    hello: Mutex<Value>,
    via: &'static str,
}

#[derive(Debug)]
enum Outgoing {
    Text(String),
    Close(u16, &'static str),
}

/// What the tab is, for `browser_connect`'s answer.
#[derive(Debug, Clone)]
pub struct TabInfo {
    pub via: &'static str,
    pub hello: Value,
}

impl Bridge {
    /// Listen on a free port of 127.0.0.1 for the page at `page`.
    pub fn start(page: &str) -> Result<Arc<Bridge>, String> {
        let (page, page_origin) = page_and_origin(page)?;
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .map_err(|e| format!("cannot listen on 127.0.0.1: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let mut bytes = [0u8; 16];
        getrandom::fill(&mut bytes).map_err(|e| format!("no random token: {e}"))?;
        let bridge = Arc::new(Bridge {
            port,
            token: bytes.iter().map(|b| format!("{b:02x}")).collect(),
            page,
            page_origin,
            tab: Mutex::new(None),
            tab_changed: Condvar::new(),
            generation: AtomicU64::new(0),
            next_id: AtomicU64::new(1),
            client: Mutex::new(None),
        });
        let b = bridge.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let b = b.clone();
                std::thread::spawn(move || b.connection(stream));
            }
        });
        Ok(bridge)
    }

    /// The link that connects a tab: the page with the port and token in
    /// its fragment.
    pub fn link(&self) -> String {
        format!("{}#connect={}.{}", self.page, self.port, self.token)
    }

    pub fn set_client(&self, name: &str) {
        *lock(&self.client) = Some(name.to_string());
    }

    /// The connected tab, if any.
    pub fn tab(&self) -> Option<TabInfo> {
        lock(&self.tab).as_ref().map(|t| TabInfo {
            via: t.via,
            hello: lock(&t.hello).clone(),
        })
    }

    /// Wait up to `timeout` for a tab that has said hello (so its file and
    /// browser are known), and give it. A tab that connected but has not
    /// said hello by then is given as it is; none at all is `None`.
    ///
    /// This is what `browser_connect`'s `wait_seconds` blocks on, so an
    /// agent waiting for the user to open the link makes one call instead
    /// of calling it over and over while the user finds the window.
    pub fn wait_for_tab(&self, timeout: Duration) -> Option<TabInfo> {
        let deadline = Instant::now() + timeout.min(MAX_WAIT);
        let mut tab = lock(&self.tab);
        loop {
            // `hello` is only ever locked after `tab` here, and `incoming`
            // never holds both, so this order cannot deadlock.
            if let Some(t) = tab.as_ref()
                && lock(&t.hello).get("type").is_some()
            {
                break;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            tab = self
                .tab_changed
                .wait_timeout(tab, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        tab.as_ref().map(|t| TabInfo {
            via: t.via,
            hello: lock(&t.hello).clone(),
        })
    }

    /// The connected tab, waiting up to `timeout` for one to connect.
    fn wait_for_socket(&self, timeout: Duration) -> Option<Arc<Tab>> {
        let deadline = Instant::now() + timeout;
        let mut tab = lock(&self.tab);
        loop {
            if let Some(t) = tab.as_ref() {
                return Some(t.clone());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return None;
            }
            tab = self
                .tab_changed
                .wait_timeout(tab, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }

    /// Tell the waiting calls that the tab changed. Takes `tab`'s lock so
    /// a waiter that has just checked and not yet slept cannot miss it.
    fn tab_changed(&self) {
        let _tab = lock(&self.tab);
        self.tab_changed.notify_all();
    }

    /// Ask the tab to do `method` and wait up to `timeout` for its answer.
    /// With no tab, wait a few seconds for one first (a reload, or a user
    /// still opening the connection window). An `Err` is a sentence for
    /// the agent.
    pub fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let tab = self
            .wait_for_socket(TAB_GRACE.min(timeout))
            .ok_or_else(|| NOT_CONNECTED.to_string())?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        lock(&tab.pending).insert(id, tx);
        let msg = json!({"id": id, "method": method, "params": params});
        if tab.out.send(Outgoing::Text(msg.to_string())).is_err() {
            lock(&tab.pending).remove(&id);
            return Err(NOT_CONNECTED.into());
        }
        match rx.recv_timeout(timeout) {
            Ok(r) => r,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                lock(&tab.pending).remove(&id);
                Err(format!(
                    "the web page did not answer {method} within {} s (is its tab in the background, or the page busy?)",
                    timeout.as_secs()
                ))
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                Err("the web page disconnected before it answered".into())
            }
        }
    }

    // --- Connections -----------------------------------------------------

    fn connection(self: Arc<Self>, mut stream: TcpStream) {
        let _ = stream.set_read_timeout(Some(HEAD_TIMEOUT));
        let Some((head, rest)) = read_head(&mut stream) else {
            return;
        };
        let Some(req) = Request::parse(&head) else {
            respond(
                &mut stream,
                "400 Bad Request",
                "text/plain",
                &[],
                "bad request\n",
            );
            return;
        };
        if !self.host_ok(req.header("host")) {
            // Another name for this address (a DNS-rebound page's) is not
            // this server.
            respond(
                &mut stream,
                "421 Misdirected Request",
                "text/plain",
                &[],
                "wrong host\n",
            );
            return;
        }
        match (req.method.as_str(), req.path()) {
            ("GET", "/ws") => self.websocket(stream, &req, rest),
            ("GET", "/relay") => {
                let csp = format!(
                    "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src ws://127.0.0.1:{p} ws://localhost:{p}; frame-ancestors 'none'; base-uri 'none'; form-action 'none'",
                    p = self.port
                );
                let headers = [
                    ("Content-Security-Policy", csp.as_str()),
                    ("X-Frame-Options", "DENY"),
                    ("Referrer-Policy", "no-referrer"),
                    ("Cache-Control", "no-store"),
                ];
                respond(
                    &mut stream,
                    "200 OK",
                    "text/html; charset=utf-8",
                    &headers,
                    &relay_page(&self.page_origin),
                );
            }
            _ => respond(
                &mut stream,
                "404 Not Found",
                "text/plain",
                &[],
                "not found\n",
            ),
        }
    }

    fn host_ok(&self, host: Option<&str>) -> bool {
        let p = self.port;
        host.is_some_and(|h| {
            h.eq_ignore_ascii_case(&format!("127.0.0.1:{p}"))
                || h.eq_ignore_ascii_case(&format!("localhost:{p}"))
        })
    }

    /// Whether `origin` may connect, and how: the page directly, or the
    /// relay page this server serves.
    fn origin_kind(&self, origin: Option<&str>) -> Option<&'static str> {
        let o = origin?.to_ascii_lowercase();
        if o == self.page_origin {
            Some("direct")
        } else if o == format!("http://127.0.0.1:{}", self.port)
            || o == format!("http://localhost:{}", self.port)
        {
            Some("relay")
        } else {
            None
        }
    }

    fn websocket(self: Arc<Self>, mut stream: TcpStream, req: &Request, rest: Vec<u8>) {
        let Some(via) = self.origin_kind(req.header("origin")) else {
            respond(
                &mut stream,
                "403 Forbidden",
                "text/plain",
                &[],
                "origin not allowed\n",
            );
            return;
        };
        let token = req.query("token").unwrap_or_default();
        if !same(token.as_bytes(), self.token.as_bytes()) {
            respond(
                &mut stream,
                "403 Forbidden",
                "text/plain",
                &[],
                "wrong token\n",
            );
            return;
        }
        let upgrade = req
            .header("upgrade")
            .is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
        let (Some(key), true) = (req.header("sec-websocket-key"), upgrade) else {
            respond(
                &mut stream,
                "400 Bad Request",
                "text/plain",
                &[],
                "not a WebSocket\n",
            );
            return;
        };
        let accept = tungstenite::handshake::derive_accept_key(key.as_bytes());
        let head = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
        );
        if stream.write_all(head.as_bytes()).is_err() {
            return;
        }
        let _ = stream.set_read_timeout(Some(POLL));
        let _ = stream.set_nodelay(true);
        let config = WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE))
            .max_frame_size(Some(MAX_MESSAGE));
        let mut ws = WebSocket::from_partially_read(stream, rest, Role::Server, Some(config));

        let (tx, rx) = mpsc::channel();
        let tab = Arc::new(Tab {
            generation: self.generation.fetch_add(1, Ordering::Relaxed) + 1,
            out: tx,
            pending: Mutex::new(HashMap::new()),
            hello: Mutex::new(json!({})),
            via,
        });
        let welcome = json!({
            "type": "welcome",
            "client": lock(&self.client).clone(),
            "server": format!("neoscad {}", env!("CARGO_PKG_VERSION")),
            "via": via,
        });
        let _ = tab.out.send(Outgoing::Text(welcome.to_string()));
        {
            let mut current = lock(&self.tab);
            if let Some(old) = current.replace(tab.clone()) {
                let _ = old
                    .out
                    .send(Outgoing::Close(REPLACED, "another tab connected"));
            }
            self.tab_changed.notify_all();
        }
        if let Err(e) = self.pump(&mut ws, &tab, &rx) {
            eprintln!("neoscad mcp: the web page's connection ended: {e}");
        }
        {
            let mut current = lock(&self.tab);
            if current
                .as_ref()
                .is_some_and(|t| t.generation == tab.generation)
            {
                *current = None;
                self.tab_changed.notify_all();
            }
        }
        // Dropping the senders ends every wait on this tab at once.
        lock(&tab.pending).clear();
    }

    /// The tab's owner loop: send what is queued, then read for a moment.
    /// Ends when the socket does; an error says why (for the log: a client
    /// keeps the server's stderr, and "the page disconnected" alone would
    /// not say which side ended it).
    fn pump(
        &self,
        ws: &mut WebSocket<TcpStream>,
        tab: &Tab,
        rx: &mpsc::Receiver<Outgoing>,
    ) -> Result<(), WsError> {
        let mut closing: Option<Instant> = None;
        loop {
            loop {
                match rx.try_recv() {
                    Ok(Outgoing::Text(s)) => ws.write(Message::text(s))?,
                    Ok(Outgoing::Close(code, reason)) => {
                        let _ = ws.close(Some(CloseFrame {
                            code: CloseCode::from(code),
                            reason: reason.into(),
                        }));
                        closing = Some(Instant::now());
                    }
                    Err(_) => break,
                }
            }
            match ws.flush() {
                Ok(()) => {}
                Err(WsError::Io(e)) if would_block(&e) => {}
                Err(e) => return Err(e),
            }
            if closing.is_some_and(|t| t.elapsed() > Duration::from_secs(1)) {
                return Ok(());
            }
            match ws.read() {
                Ok(Message::Text(t)) => self.incoming(tab, t.as_str()),
                Ok(_) => {}
                Err(WsError::Io(e)) if would_block(&e) => {}
                Err(WsError::ConnectionClosed | WsError::AlreadyClosed) => return Ok(()),
                Err(e) => return Err(e),
            }
        }
    }

    fn incoming(&self, tab: &Tab, text: &str) {
        let Ok(msg) = serde_json::from_str::<Value>(text) else {
            return;
        };
        if let Some(id) = msg.get("id").and_then(Value::as_u64) {
            let Some(tx) = lock(&tab.pending).remove(&id) else {
                return;
            };
            let reply = match msg.get("error") {
                Some(e) => Err(e
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("the web page could not do it")
                    .to_string()),
                None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
            };
            let _ = tx.send(reply);
        } else if msg["type"] == "hello" {
            *lock(&tab.hello) = msg;
            self.tab_changed();
        }
    }
}

fn would_block(e: &io::Error) -> bool {
    // A read timeout is `WouldBlock` on Unix and `TimedOut` on Windows.
    matches!(
        e.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

/// Compare without stopping at the first difference, so the time a wrong
/// token takes says nothing about how much of it was right.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |d, (x, y)| d | (x ^ y)) == 0
}

/// The page's URL without a fragment, and its origin (`scheme://host[:port]`,
/// lower case). Only http and https pages can connect.
pub fn page_and_origin(url: &str) -> Result<(String, String), String> {
    let url = url.split('#').next().unwrap_or_default().to_string();
    let bad = || format!("--browser-url must be an http or https URL, not '{url}'");
    let (scheme, rest) = url.split_once("://").ok_or_else(bad)?;
    let scheme = scheme.to_ascii_lowercase();
    if scheme != "http" && scheme != "https" {
        return Err(bad());
    }
    let host = rest.split(['/', '?']).next().unwrap_or_default();
    if host.is_empty() || host.contains('@') || host.contains(char::is_whitespace) {
        return Err(bad());
    }
    let host = host.to_ascii_lowercase();
    // The origin leaves out the scheme's default port, as browsers send it.
    let host = match (scheme.as_str(), host.rsplit_once(':')) {
        ("http", Some((h, "80"))) | ("https", Some((h, "443"))) => h.to_string(),
        _ => host,
    };
    Ok((url.clone(), format!("{scheme}://{host}")))
}

/// Read up to the blank line that ends a request's head; the head and
/// whatever came after it.
fn read_head(stream: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    let mut buf = Vec::with_capacity(1024);
    let mut chunk = [0u8; 2048];
    loop {
        if let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let rest = buf.split_off(end + 4);
            return Some((String::from_utf8_lossy(&buf).into_owned(), rest));
        }
        if buf.len() > MAX_HEAD {
            return None;
        }
        match stream.read(&mut chunk) {
            Ok(0) | Err(_) => return None,
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
        }
    }
}

fn respond(stream: &mut TcpStream, status: &str, kind: &str, headers: &[(&str, &str)], body: &str) {
    let mut head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\nX-Content-Type-Options: nosniff\r\n",
        body.len()
    );
    for (k, v) in headers {
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    head.push_str("\r\n");
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

/// A request's line and headers (names lower-cased).
#[derive(Debug)]
struct Request {
    method: String,
    target: String,
    headers: Vec<(String, String)>,
}

impl Request {
    fn parse(head: &str) -> Option<Request> {
        let mut lines = head.split("\r\n");
        let mut first = lines.next()?.split(' ');
        let (method, target) = (first.next()?.to_string(), first.next()?.to_string());
        let headers = lines
            .filter_map(|l| l.split_once(':'))
            .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
            .collect();
        Some(Request {
            method,
            target,
            headers,
        })
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    fn path(&self) -> &str {
        self.target.split('?').next().unwrap_or_default()
    }

    fn query(&self, key: &str) -> Option<&str> {
        self.target
            .split_once('?')?
            .1
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .find(|(k, _)| *k == key)
            .map(|(_, v)| v)
    }
}

/// The relay page, allowed to talk to the page at `origin` only.
fn relay_page(origin: &str) -> String {
    // A JSON string is a JavaScript string literal; `<` escaped so the
    // origin cannot end the script element.
    let origin = Value::String(origin.to_string())
        .to_string()
        .replace('<', "\\u003c");
    include_str!("relay.html").replace("\"__PAGE_ORIGIN__\"", &origin)
}

/// Open `url` in the user's default browser (`browser_connect`'s `open`,
/// `--open`).
pub fn open_in_browser(url: &str) -> Result<(), String> {
    let mut cmd = if cfg!(target_os = "macos") {
        std::process::Command::new("open")
    } else if cfg!(windows) {
        // `start` would read `&` and `^` in a URL as the shell's; the URL
        // handler takes it as it is.
        let mut c = std::process::Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler");
        c
    } else {
        std::process::Command::new("xdg-open")
    };
    let mut child = cmd
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot open a browser: {e}"))?;
    // Reap it, so it does not linger as a zombie while the server runs.
    std::thread::spawn(move || child.wait());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins_are_the_scheme_and_host_as_browsers_send_them() {
        let o = |u: &str| page_and_origin(u).map(|p| p.1);
        assert_eq!(
            o("https://neoscad.org/try/"),
            Ok("https://neoscad.org".into())
        );
        assert_eq!(
            o("https://NeoSCAD.org:443/try/#x"),
            Ok("https://neoscad.org".into())
        );
        assert_eq!(
            o("http://127.0.0.1:8123/try/"),
            Ok("http://127.0.0.1:8123".into())
        );
        assert_eq!(o("http://localhost:80"), Ok("http://localhost".into()));
        assert!(o("file:///try/index.html").is_err());
        assert!(o("https://user@evil.example/").is_err());
        assert!(o("neoscad.org/try").is_err());
        assert_eq!(
            page_and_origin("https://neoscad.org/try/#connect=1.2")
                .unwrap()
                .0,
            "https://neoscad.org/try/"
        );
    }

    #[test]
    fn requests_parse_and_only_this_server_and_page_are_accepted() {
        let b = Bridge::start("https://neoscad.org/try/").unwrap();
        let p = b.port;
        let r = Request::parse(&format!(
            "GET /ws?token=abc&x=1 HTTP/1.1\r\nHost: 127.0.0.1:{p}\r\nOrigin: https://neoscad.org\r\nUpgrade: websocket"
        ))
        .unwrap();
        assert_eq!((r.method.as_str(), r.path()), ("GET", "/ws"));
        assert_eq!(r.query("token"), Some("abc"));
        assert_eq!(r.query("y"), None);
        assert!(b.host_ok(r.header("host")));
        assert!(b.host_ok(Some(&format!("LOCALHOST:{p}"))));
        assert!(!b.host_ok(Some("127.0.0.1")));
        assert!(!b.host_ok(Some(&format!("evil.example:{p}"))));
        assert!(!b.host_ok(None));
        assert_eq!(b.origin_kind(r.header("origin")), Some("direct"));
        assert_eq!(
            b.origin_kind(Some(&format!("http://127.0.0.1:{p}"))),
            Some("relay")
        );
        assert_eq!(
            b.origin_kind(Some("https://neoscad.org.evil.example")),
            None
        );
        assert_eq!(b.origin_kind(Some("http://neoscad.org")), None);
        assert_eq!(b.origin_kind(Some("null")), None);
        assert_eq!(b.origin_kind(None), None);
        // The link carries the port and a 128-bit token.
        let link = b.link();
        let (port, token) = link
            .strip_prefix("https://neoscad.org/try/#connect=")
            .unwrap()
            .split_once('.')
            .unwrap();
        assert_eq!(port, p.to_string());
        assert_eq!(token.len(), 32);
        assert!(token.bytes().all(|c| c.is_ascii_hexdigit()));
        assert!(same(token.as_bytes(), b.token.as_bytes()));
        assert!(!same(b"", b.token.as_bytes()));
    }

    #[test]
    fn the_relay_page_names_only_the_page_origin() {
        let html = relay_page("https://neoscad.org");
        assert!(html.contains("const PAGE_ORIGIN = \"https://neoscad.org\";"));
        assert!(!html.contains("__PAGE_ORIGIN__"));
        // An origin cannot close the script element.
        assert!(!relay_page("http://x</script>").contains("x</script>"));
    }

    #[test]
    fn requests_without_a_tab_say_how_to_connect() {
        let b = Bridge::start(DEFAULT_PAGE).unwrap();
        let e = b
            .request("read", json!({}), Duration::from_millis(10))
            .unwrap_err();
        assert!(e.contains("browser_connect"), "{e}");
        assert!(b.tab().is_none());
        assert!(b.wait_for_tab(Duration::from_millis(20)).is_none());
    }

    /// A stand-in for the page: connects to `b` as the page's origin does.
    fn fake_tab(b: &Bridge) -> WebSocket<TcpStream> {
        use tungstenite::client::IntoClientRequest;
        let mut req = format!("ws://127.0.0.1:{}/ws?token={}", b.port, b.token)
            .into_client_request()
            .unwrap();
        req.headers_mut().insert(
            "origin",
            tungstenite::http::HeaderValue::from_static("https://neoscad.org"),
        );
        let stream = TcpStream::connect(("127.0.0.1", b.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        tungstenite::client::client(req, stream).unwrap().0
    }

    /// The next request the bridge sends a fake tab (skipping `welcome`).
    fn next_request(ws: &mut WebSocket<TcpStream>) -> Value {
        loop {
            let m: Value = serde_json::from_str(ws.read().unwrap().to_text().unwrap()).unwrap();
            if m.get("id").is_some() {
                return m;
            }
        }
    }

    #[test]
    fn waiting_for_a_tab_returns_when_it_says_hello() {
        let b = Bridge::start(DEFAULT_PAGE).unwrap();
        let b2 = b.clone();
        let page = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            let mut ws = fake_tab(&b2);
            let hello = json!({"type": "hello", "file": "gears.scad", "browser": "Chrome 150"});
            ws.send(Message::text(hello.to_string())).unwrap();
            ws
        });
        let start = Instant::now();
        let t = b.wait_for_tab(Duration::from_secs(20)).expect("a tab");
        // It returned on the hello, not at the end of the wait.
        assert!(start.elapsed() < Duration::from_secs(10));
        assert_eq!(t.hello["file"], "gears.scad");
        assert_eq!(t.via, "direct");
        drop(page.join().unwrap());
    }

    #[test]
    fn a_request_waits_briefly_for_a_tab_to_connect() {
        let b = Bridge::start(DEFAULT_PAGE).unwrap();
        let b2 = b.clone();
        let page = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            let mut ws = fake_tab(&b2);
            let req = next_request(&mut ws);
            assert_eq!(req["method"], "read");
            let reply = json!({"id": req["id"], "result": {"text": "cube(1);"}});
            ws.send(Message::text(reply.to_string())).unwrap();
            // Keep the socket until the answer is read.
            std::thread::sleep(Duration::from_millis(300));
        });
        let r = b
            .request("read", json!({}), Duration::from_secs(15))
            .unwrap();
        assert_eq!(r["text"], "cube(1);");
        page.join().unwrap();
    }
}
