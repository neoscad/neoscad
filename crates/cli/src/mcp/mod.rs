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

mod app;
mod bridge;

/// Also what `neoscad bench --submit` opens its issue link with.
pub(crate) use bridge::open_in_browser;
mod recipes;
pub mod roots;
mod tools;

use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
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
/// With the recipes it has to fit [`INSTRUCTIONS_LIMIT`].
const INSTRUCTIONS: &str = "NeoSCAD is an OpenSCAD-compatible modeller; every tool answers in milliseconds. After writing or editing a model, one `check` (with the spec's minimum wall as `min_wall`) reports its errors, warnings, echo, geometry and printability findings, so there is no need to `evaluate` or `render` first. Call tools on independent files or questions in parallel. `measure` gives exact numbers (sections, distances); `snapshot` shows the shape when it is in doubt. `check` also takes `export` and `sections`, so one call can verify, measure and write the file; an export is read back, so the file needs no other inspection. Info findings (short bridges, thread flanks, slivers) need no action. A model is a file you write (`path`) or inline `source`.";

/// The most of the instructions a client is known to keep, in UTF-16
/// code units. Claude Code (2.1.286) cuts a server's instructions after
/// 2,048 characters and adds "… [truncated]": the model then saw the
/// recipes end mid-comment, without the snap hook, and went looking for
/// it with `docs`. Text past the limit costs tokens and helps no one.
const INSTRUCTIONS_LIMIT: usize = 2048;

/// Idioms for the features printable parts keep needing (a countersink,
/// rounded corners, a fillet, a thread, a snap hook), as OpenSCAD modules,
/// appended to [`INSTRUCTIONS`] and served as `neoscad://recipes`. Agents
/// worked these out from scratch in every session, and a first thread was
/// sometimes an inside-out sweep. Each module is rendered and checked by
/// `crates/cli/tests/mcp.rs`; every session pays for the text, so it stays
/// at five modules.
pub const RECIPES: &str = include_str!("recipes.scad");

/// What introduces [`RECIPES`] in the instructions.
const RECIPES_INTRO: &str = "\n\nPrinting recipes (tested; adapt the numbers):\n";

/// What stands in for [`RECIPES`] when they would not fit the limit
/// (with `--browser`'s extra paragraph): a pointer, rather than recipes
/// the client cuts short.
const RECIPES_POINTER: &str = " Printing recipes (countersink, rounded plate, fillet, thread, snap hook) are the resource neoscad://recipes, and `docs` gives each by name.";

/// What the server's instructions add to [`INSTRUCTIONS`]: the web page's
/// paragraph, or the app's while one is connected, or nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Extra {
    None,
    Browser,
    App,
}

/// The server's instructions: the guidance, `--browser`'s paragraph when
/// there is a page bridge (or the app's when an app is connected), then
/// the recipes if they fit [`INSTRUCTIONS_LIMIT`] and a pointer to them if
/// not.
fn instructions(extra: Extra) -> String {
    let head = format!(
        "{INSTRUCTIONS}{}",
        match extra {
            Extra::None => "",
            Extra::Browser => BROWSER_INSTRUCTIONS,
            Extra::App => APP_INSTRUCTIONS,
        }
    );
    let full = format!("{head}{RECIPES_INTRO}{RECIPES}");
    if full.encode_utf16().count() <= INSTRUCTIONS_LIMIT {
        full
    } else {
        format!("{head}{RECIPES_POINTER}")
    }
}

/// What `--browser` adds to [`INSTRUCTIONS`].
const BROWSER_INSTRUCTIONS: &str = " The user may have NeoSCAD's web page open: browser_connect gives the link that connects it. Once it is connected, work on the page's text rather than files: omit path and source to use it, change it with editor_edit (the user sees each change), look with view_capture and point with view_annotate.";

/// What a connected NeoSCAD app adds to [`INSTRUCTIONS`]. Only when an
/// app is connected as the session starts: the tool descriptions say the
/// rest, and a session without an app pays nothing.
const APP_INSTRUCTIONS: &str = " The user has the NeoSCAD app open: omit path and source to work on its focused document, change it with editor_edit (the user sees each change), look with view_capture and point with view_annotate.";

const EXIT_ERROR: u8 = 1;

#[derive(Parser, Debug)]
#[command(
    name = "neoscad mcp",
    about = "Serve NeoSCAD's tools to an AI agent over the Model Context Protocol (stdio; docs/mcp.md)"
)]
pub(crate) struct Args {
    /// Allow reading and writing under DIR (repeatable). The working
    /// directory is allowed too, unless it is /, the home folder, a hidden
    /// or settings folder in it, or a system folder; library and font
    /// directories are readable.
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

    /// Turn on one of OpenSCAD's experimental features or NeoSCAD's
    /// extensions for every call, as the command line's --enable does
    /// (repeatable): textmetrics, object-function, import-function,
    /// vector-swizzle, predictible-output (sorted mesh exports); part
    /// (named parts; sketch and query are reserved). Off by default, as in
    /// OpenSCAD. (A server-wide flag rather than a tool argument, so it
    /// costs the agent's context nothing.)
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

    /// Do not look for a running NeoSCAD app. By default the server
    /// connects to every NeoSCAD app of this user that allows AI agents
    /// (a per-user socket, no network), and the editor and view tools act
    /// on its open document; without one, everything works on files.
    #[arg(long = "no-app", conflicts_with = "browser")]
    no_app: bool,

    /// Also list an optional tool (repeatable): test (model tests),
    /// format (the formatter; listed anyway with --browser). Left out by
    /// default because every listed tool costs the agent context.
    #[arg(long = "tool", value_name = "NAME", action = clap::ArgAction::Append)]
    tools: Vec<String>,
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
    // The working directory is a root only when it is a project-like
    // folder: a client that starts the server in `/` or the home folder
    // (Claude Desktop does) must not let the agent write anywhere the user
    // can (`roots::unsafe_cwd`). `--root`s are the user's explicit choice.
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .map(|h| lang::paths::plain(h.canonicalize().unwrap_or(h)));
    let cwd_refused = roots::unsafe_cwd(
        &lang::paths::plain(cwd.canonicalize().unwrap_or_else(|_| cwd.clone())),
        home.as_deref(),
        &roots::SystemDirs::here(),
    );
    let mut write = Vec::new();
    match cwd_refused {
        None => write.push(cwd.clone()),
        Some(why) => eprintln!(
            "neoscad mcp: not using the working directory {} as a root ({why}); add --root DIR for the agent's files",
            cwd.display()
        ),
    }
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
    let mut roots = roots::Roots::new(&write, &read);
    if let Some(why) = cwd_refused {
        roots = roots.with_unsafe_cwd(&cwd, why);
    }
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
    cfg.extensions = crate::extensions(&a.enable);
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
    // A running app is looked for before anything is answered, so the
    // first `tools/list` already has its tools.
    let apps = (!a.no_app && bridge.is_none())
        .then(|| app::Apps::start(agent_link::discovery::Rendezvous::from_env(), roots.clone()));
    let extra = if bridge.is_some() {
        Extra::Browser
    } else if apps.as_ref().is_some_and(|a| a.connected()) {
        Extra::App
    } else {
        Extra::None
    };
    let tools = match tools::Tools::new(
        crate::serve::Local::new(cfg),
        roots,
        bridge.clone(),
        apps.clone(),
        &a.tools,
    ) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("neoscad mcp: {e}");
            return EXIT_ERROR;
        }
    };
    let server = Arc::new(Server {
        instructions: instructions(extra),
        bridge: bridge.clone(),
        apps: apps.clone(),
        initialized: AtomicBool::new(false),
        tools,
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
    if let Some(apps) = &apps {
        // The app's tools come and go with the app: tell the client to
        // fetch the list again (Claude Code does, "Dynamic tool updates").
        let weak = Arc::downgrade(&server);
        apps.on_presence(Box::new(move || {
            if let Some(s) = weak.upgrade()
                && s.initialized.load(Ordering::SeqCst)
            {
                s.send(&json!({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}));
            }
        }));
    }
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
    /// The running apps, told the client's name likewise.
    apps: Option<Arc<app::Apps>>,
    /// The client has opened the session, so notifications may be sent.
    initialized: AtomicBool,
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
    if let (Some(a), Some(n)) = (&server.apps, name.and_then(Value::as_str)) {
        a.set_client(n);
    }
}

fn server_info() -> Value {
    json!({"name": "neoscad", "title": "NeoSCAD", "version": env!("CARGO_PKG_VERSION")})
}

/// The tool list changes only when apps are looked for (an app's tools
/// come and go with it).
fn capabilities(server: &Server) -> Value {
    json!({"tools": {"listChanged": server.apps.is_some()}, "resources": {"subscribe": false, "listChanged": false}})
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
        server.initialized.store(true, Ordering::SeqCst);
        return Ok(json!({
            "protocolVersion": version,
            "capabilities": capabilities(server),
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
        server.initialized.store(true, Ordering::SeqCst);
    }
    let mut result = match method {
        "ping" => json!({}),
        "server/discover" => json!({
            "supportedVersions": [MODERN],
            "capabilities": capabilities(server),
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
