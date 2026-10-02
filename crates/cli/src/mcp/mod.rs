//! `neoscad mcp`: a Model Context Protocol server over stdio (`docs/mcp.md`).
//!
//! It implements MCP revision 2026-07-28, the current one
//! (<https://modelcontextprotocol.io/specification/2026-07-28>), and is
//! "dual-era" in that revision's terms (`basic/versioning`): a request
//! that carries `io.modelcontextprotocol/protocolVersion` in `_meta` is
//! served statelessly as 2026-07-28 says, and a client that opens with
//! `initialize` gets the legacy handshake of 2025-11-25 (or the older
//! revision it asks for). Hosts in use today still send `initialize`;
//! a modern-only server would be unusable from them, and the spec's
//! compatibility matrix names the dual-era server as the one that works
//! with both.
//!
//! Framing is MCP's stdio binding: one JSON-RPC message per line, nothing
//! but messages on stdout. The tools ([`tools`]) run on the same
//! [`crate::serve::Local`] session as `neoscad serve`, so a warm cache
//! makes a one-line edit re-render in milliseconds here too.
//!
//! Hand-rolled rather than on the official Rust SDK (`rmcp`): the server
//! needs ten methods and no transport but stdio, the rest of the command
//! line is synchronous threads (`rmcp` needs tokio), and `rmcp` 3.4.1
//! (2026-09-23) still defaults to 2025-11-25.

mod bridge;

/// Also what `neoscad bench --submit` opens its issue link with.
pub(crate) use bridge::open_in_browser;
pub mod roots;
mod tools;

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clap::Parser;
use serde_json::{Value, json};

use crate::rpc::code;

/// The revision this server implements.
pub const MODERN: &str = "2026-07-28";
/// The `initialize`-era revisions it also answers, newest first.
pub const LEGACY: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// MCP's error codes (`basic/index`, "Error Codes").
const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32022;
/// Resource not found before 2026-07-28 (then -32602).
const LEGACY_RESOURCE_NOT_FOUND: i64 = -32002;

/// How long a client may cache the tool list and the docs: they change
/// only with the binary.
const TTL_MS: u64 = 3_600_000;

/// Guidance for the model, sent once per session (legacy `initialize`,
/// `server/discover`); the tool descriptions carry the rest.
///
/// It steers an agent towards one `check` per edit. An agent pays for
/// every turn in model time and in re-reading its whole context, while the
/// tools answer in milliseconds, so the wording avoids reading as a ladder
/// of separate calls (evaluate, then render, then snapshot, then check).
const INSTRUCTIONS: &str = "NeoSCAD is an OpenSCAD-compatible modeller; every tool answers in milliseconds. After writing or editing a model, one `check` (with the spec's minimum wall as `min_wall`) reports its errors, warnings and echo, bbox, volume, manifold and printability findings, so there is no need to `evaluate` or `render` first. Call tools on independent files or questions in parallel. `measure` gives exact numbers (sections, distances); `snapshot` shows the shape when it is in doubt. `render` with `export` writes the file and reports its path and size. A model is a file you write (`path`) or inline `source`.";

/// What `--browser` adds to [`INSTRUCTIONS`].
const BROWSER_INSTRUCTIONS: &str = " The user may have NeoSCAD's web page open: browser_connect gives the link that connects it. Once it is connected, work on the page's text rather than files: omit path and source to use it, change it with editor_edit (the user sees each change), look with view_capture and point with view_annotate.";

const EXIT_ERROR: u8 = 1;

#[derive(Parser, Debug)]
#[command(
    name = "neoscad mcp",
    about = "Serve NeoSCAD's tools to an AI agent over the Model Context Protocol (stdio; docs/mcp.md)"
)]
pub(crate) struct Args {
    /// Allow reading and writing under DIR (repeatable). The working
    /// directory is always allowed; library and font directories are
    /// readable.
    #[arg(long = "root", value_name = "DIR", action = clap::ArgAction::Append)]
    roots: Vec<PathBuf>,

    /// Geometry cache budget in MiB (as `neoscad serve --cache-mb`).
    #[arg(long = "cache-mb", value_name = "MiB")]
    cache_mb: Option<usize>,

    /// Append every message received and sent to FILE (for debugging a
    /// client).
    #[arg(long = "log", value_name = "FILE")]
    log: Option<PathBuf>,

    /// Change a resource limit, NAME=VALUE (repeatable; 'off' for none):
    /// time (s, default 60), memory (MiB or 4G, default 4G), fragments
    /// (10000), slices (10000), list (1e7), string (64 MiB), rands (1e7),
    /// triangles (1e7).
    #[arg(long = "limit", value_name = "NAME=VALUE", action = clap::ArgAction::Append)]
    limit: Vec<String>,

    /// Turn on one of OpenSCAD's experimental features for every call, as
    /// the command line's --enable does (repeatable): textmetrics,
    /// object-function, import-function, vector-swizzle, predictible-output
    /// (sorted mesh exports). Off by default,
    /// as in OpenSCAD. (A server-wide flag rather than a tool argument, so
    /// it costs the agent's context nothing.)
    #[arg(long = "enable", value_name = "FEATURE", action = clap::ArgAction::Append)]
    enable: Vec<String>,

    /// Let the NeoSCAD web page (neoscad.org/try) connect, so the agent
    /// can work on the text open there and see its 3D view: listens on
    /// 127.0.0.1 (a free port) for a tab with the random token of the
    /// link the browser_connect tool gives.
    #[arg(long)]
    browser: bool,

    /// The page the connect link opens, whose origin alone may connect
    /// (default https://neoscad.org/try/; a local copy for development).
    #[arg(long = "browser-url", value_name = "URL", requires = "browser")]
    browser_url: Option<String>,

    /// Open the connect link in the default browser at startup.
    #[arg(long, requires = "browser")]
    open: bool,
}

/// Run `neoscad mcp` with the arguments after `mcp`.
pub fn main(args: Vec<OsString>) -> u8 {
    let argv = std::iter::once(OsString::from("neoscad mcp")).chain(args);
    let a = match Args::try_parse_from(argv) {
        Ok(a) => a,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() { EXIT_ERROR } else { 0 };
        }
    };
    // A parent can start us in a verbatim (`\\?\`) directory on Windows;
    // `--root ../x` joined onto that would not fold its `..`.
    let cwd = lang::paths::plain(std::env::current_dir().unwrap_or_default());
    let mut write = vec![cwd.clone()];
    write.extend(a.roots.iter().map(|r| cwd.join(r)));
    // Libraries and fonts are readable wherever they are: the unrestricted
    // host says where they are, then the real one is built over the
    // rooted file system.
    let open = crate::host::Host::from_env();
    let mut read: Vec<PathBuf> = open.libs.0.clone();
    for f in open.font_sources() {
        if let crate::host::FontSource::Dir(d) = f {
            read.push(d);
        }
    }
    let roots = roots::Roots::new(&write, &read);
    if roots.writable().len() < write.len() {
        eprintln!("neoscad mcp: warning: a --root that does not exist was ignored");
    }
    let fs = Arc::new(roots::RootedFs::new(
        Arc::new(lang::loader::StdFs),
        roots.clone(),
    ));
    let host = crate::host::Host::from_env_over(fs);
    let mut cfg = host.session_config(crate::host::entropy_seed());
    if let Some(mb) = a.cache_mb {
        cfg.geometry_budget = mb << 20;
    }
    for w in crate::enable_warnings(&a.enable) {
        eprintln!("neoscad mcp: {w}");
    }
    cfg.features = crate::features(&a.enable);
    // An agent's generated code is exactly what one runaway `$fn` comes
    // from: the agent limits are on unless the user changes them.
    cfg.limits = match crate::limits::from_flags(session::Limits::AGENT, &a.limit) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("neoscad mcp: {e}");
            return EXIT_ERROR;
        }
    };
    let bridge = if a.browser {
        let page = a.browser_url.as_deref().unwrap_or(bridge::DEFAULT_PAGE);
        match bridge::Bridge::start(page) {
            Ok(b) => {
                // stderr: stdout carries only MCP messages. Clients keep
                // it in their logs; the agent gets the link from
                // browser_connect.
                eprintln!("neoscad mcp: connect the web page with {}", b.link());
                if a.open
                    && let Err(e) = bridge::open_in_browser(&b.link())
                {
                    eprintln!("neoscad mcp: {e}");
                }
                Some(b)
            }
            Err(e) => {
                eprintln!("neoscad mcp: {e}");
                return EXIT_ERROR;
            }
        }
    } else {
        None
    };
    let server = Arc::new(Server {
        instructions: if bridge.is_some() {
            format!("{INSTRUCTIONS}{BROWSER_INSTRUCTIONS}")
        } else {
            INSTRUCTIONS.to_string()
        },
        bridge: bridge.clone(),
        tools: tools::Tools::new(crate::serve::Local::new(cfg), roots, bridge),
        out: Mutex::new(Box::new(std::io::stdout())),
        log: a.log.and_then(|p| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .ok()
                .map(Mutex::new)
        }),
        cancelled: Mutex::new(HashSet::new()),
    });
    let stdin = std::io::stdin();
    serve(&server, stdin.lock());
    0
}

struct Server {
    tools: tools::Tools,
    instructions: String,
    /// The web page's bridge, told the client's name for the page's
    /// "connected to" line.
    bridge: Option<Arc<bridge::Bridge>>,
    out: Mutex<Box<dyn Write + Send>>,
    log: Option<Mutex<std::fs::File>>,
    /// Requests the client cancelled: their answers are not sent
    /// (`basic/patterns/cancellation`: no further messages for them).
    cancelled: Mutex<HashSet<String>>,
}

impl Server {
    fn trace(&self, dir: &str, line: &str) {
        if let Some(f) = &self.log {
            let mut f = f.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
            let _ = writeln!(f, "{dir} {line}");
        }
    }

    fn send(&self, msg: &Value) {
        // serde_json never writes a raw newline inside a message (they are
        // escaped in strings), which the stdio framing requires.
        let line = msg.to_string();
        self.trace("->", &line);
        let mut out = self
            .out
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = writeln!(out, "{line}");
        let _ = out.flush();
    }
}

/// How long the calls still running at the end of input get to notice
/// their cancellation before the process exits anyway (a single kernel
/// operation cannot be interrupted).
const EXIT_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

fn serve(server: &Arc<Server>, r: impl BufRead) {
    let mut running: Vec<(Value, std::thread::JoinHandle<()>)> = Vec::new();
    for line in r.lines() {
        running.retain(|(_, h)| !h.is_finished());
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        server.trace("<-", &line);
        let msg: Value = match serde_json::from_str(&line) {
            Ok(m) => m,
            Err(e) => {
                server.send(&error(
                    &Value::Null,
                    code::PARSE_ERROR,
                    &e.to_string(),
                    None,
                ));
                continue;
            }
        };
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            // A response: the server sends no requests, so there is
            // nothing to match it with.
            continue;
        };
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let Some(id) = msg.get("id").cloned().filter(|i| !i.is_null()) else {
            notification(server, method, &params);
            continue;
        };
        if method == "tools/call" {
            // Tools run concurrently; the session cancels an older request
            // on the same document when a newer one arrives.
            let (server, method, key) = (server.clone(), method.to_string(), id.clone());
            running.push((
                key,
                std::thread::spawn(move || {
                    let reply = request(&server, &id, &method, &params);
                    respond(&server, &id, reply);
                }),
            ));
        } else {
            let reply = request(server, &id, method, &params);
            respond(server, &id, reply);
        }
    }
    // The end of input means the client is gone (or is stopping the
    // server): nobody will read an answer. Cancel what is still running
    // and exit, rather than computing on as an orphan; a render stuck in
    // one kernel operation is cut off when the process exits after the
    // grace period.
    running.retain(|(_, h)| !h.is_finished());
    for (id, _) in &running {
        server
            .cancelled
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.to_string());
    }
    server.tools.cancel_all();
    let deadline = std::time::Instant::now() + EXIT_GRACE;
    while running.iter().any(|(_, h)| !h.is_finished()) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

fn respond(server: &Server, id: &Value, reply: Result<Value, Failure>) {
    if server
        .cancelled
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .remove(&id.to_string())
    {
        return;
    }
    let msg = match reply {
        Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
        Err(f) => error(id, f.code, &f.message, f.data),
    };
    server.send(&msg);
}

fn error(id: &Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut e = json!({"code": code, "message": message});
    if let Some(d) = data {
        e["data"] = d;
    }
    json!({"jsonrpc": "2.0", "id": id, "error": e})
}

#[derive(Debug)]
struct Failure {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl Failure {
    fn new(code: i64, message: impl Into<String>) -> Failure {
        Failure {
            code,
            message: message.into(),
            data: None,
        }
    }
}

fn notification(server: &Server, method: &str, params: &Value) {
    // `notifications/initialized` and anything else unknown need nothing.
    if method == "notifications/cancelled"
        && let Some(id) = params.get("requestId")
    {
        server
            .cancelled
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(id.to_string());
        server.tools.cancel(id);
    }
}

/// Remember the client's name (`clientInfo`) for the web page's status
/// line, when there is a page to tell.
fn note_client(server: &Server, info: Option<&Value>) {
    let name = info.and_then(|i| i.get("title").or_else(|| i.get("name")));
    if let (Some(b), Some(n)) = (&server.bridge, name.and_then(Value::as_str)) {
        b.set_client(n);
    }
}

fn server_info() -> Value {
    json!({"name": "neoscad", "title": "NeoSCAD", "version": env!("CARGO_PKG_VERSION")})
}

fn capabilities() -> Value {
    json!({"tools": {"listChanged": false}, "resources": {"subscribe": false, "listChanged": false}})
}

/// Which revision a request speaks: modern when its `_meta` names a
/// version (then it must be one we implement), legacy otherwise.
fn era(params: &Value) -> Result<bool, Failure> {
    let meta = params.get("_meta");
    let Some(v) = meta.and_then(|m| m.get("io.modelcontextprotocol/protocolVersion")) else {
        return Ok(false);
    };
    let requested = v.as_str().unwrap_or_default();
    if requested != MODERN {
        let mut supported = vec![MODERN];
        supported.extend(LEGACY);
        return Err(Failure {
            code: UNSUPPORTED_PROTOCOL_VERSION,
            message: "Unsupported protocol version".into(),
            data: Some(json!({"supported": supported, "requested": requested})),
        });
    }
    if meta
        .and_then(|m| m.get("io.modelcontextprotocol/clientCapabilities"))
        .is_none()
    {
        return Err(Failure::new(
            code::INVALID_PARAMS,
            "_meta must include io.modelcontextprotocol/clientCapabilities",
        ));
    }
    Ok(true)
}

fn request(server: &Server, id: &Value, method: &str, params: &Value) -> Result<Value, Failure> {
    if method == "initialize" {
        // The legacy handshake: answer the client's revision when it is
        // one we speak, else the newest legacy one (2025-11-25
        // `basic/lifecycle`, "Version Negotiation").
        let asked = params
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let version = LEGACY.iter().find(|v| **v == asked).unwrap_or(&LEGACY[0]);
        note_client(server, params.get("clientInfo"));
        return Ok(json!({
            "protocolVersion": version,
            "capabilities": capabilities(),
            "serverInfo": server_info(),
            "instructions": server.instructions,
        }));
    }
    let modern = era(params)?;
    if modern {
        note_client(
            server,
            params["_meta"].get("io.modelcontextprotocol/clientInfo"),
        );
    }
    let mut result = match method {
        "ping" => json!({}),
        "server/discover" => json!({
            "supportedVersions": [MODERN],
            "capabilities": capabilities(),
            "instructions": server.instructions,
        }),
        "tools/list" => json!({"tools": server.tools.list()}),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            if !server.tools.knows(name) {
                return Err(Failure::new(
                    code::INVALID_PARAMS,
                    format!("Unknown tool: {name}"),
                ));
            }
            server.tools.call(id, name, &args)
        }
        "resources/list" => json!({"resources": tools::resources()}),
        "resources/templates/list" => json!({"resourceTemplates": tools::resource_templates()}),
        "resources/read" => {
            let uri = params.get("uri").and_then(Value::as_str).unwrap_or("");
            match server.tools.read_resource(uri) {
                Some(text) => {
                    json!({"contents": [{"uri": uri, "mimeType": "text/plain", "text": text}]})
                }
                None => {
                    let code = if modern {
                        code::INVALID_PARAMS
                    } else {
                        LEGACY_RESOURCE_NOT_FOUND
                    };
                    return Err(Failure {
                        code,
                        message: "Resource not found".into(),
                        data: Some(json!({"uri": uri})),
                    });
                }
            }
        }
        _ => {
            return Err(Failure::new(
                code::METHOD_NOT_FOUND,
                format!("Method not found: {method}"),
            ));
        }
    };
    if modern {
        result["resultType"] = json!("complete");
        result["_meta"] = json!({"io.modelcontextprotocol/serverInfo": server_info()});
        if matches!(
            method,
            "server/discover"
                | "tools/list"
                | "resources/list"
                | "resources/templates/list"
                | "resources/read"
        ) {
            // `server/utilities/caching`: required on these results.
            result["ttlMs"] = json!(TTL_MS);
            result["cacheScope"] = json!("public");
        }
    }
    Ok(result)
}
