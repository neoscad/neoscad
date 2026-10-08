//! `neoscad serve`: a long-lived process holding a [`session::Session`],
//! answering JSON-RPC 2.0 over stdio, a Unix socket or a Windows named
//! pipe (`docs/serve-protocol.md`).
//!
//! - `neoscad serve` speaks on stdin/stdout (an editor or MCP host starts
//!   it as a child) and ends at the end of input or on `exit`.
//! - `neoscad serve --socket [PATH]` listens on a Unix socket (on Windows,
//!   a named pipe), by default the per-user one the command line looks for
//!   ([`client::default_socket`]), so `neoscad IN -o OUT` and
//!   `neoscad snapshot` use it automatically. Either is restricted to the
//!   user who started it ([`crate::transport`]); there is no network
//!   listener. It exits after `--idle-timeout` seconds with no connection
//!   and no request.
//! - `neoscad serve --status` and `--stop` ask the socket's server.
//!
//! Requests that change documents (`open`, `update`, `close`) are handled
//! in the order they arrive, before the next message is read, so a
//! `render` sent after an `update` sees the new text. Everything else runs
//! on its own thread; a newer request on a document cancels an older one
//! (the session's rule), which answers with error -32800.

use std::collections::HashMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clap::Parser;
use serde_json::{Value, json};

use crate::client;
use crate::rpc::{self, code};

/// The protocol's version; see the `capabilities` handshake.
pub const PROTOCOL_VERSION: u32 = 1;

/// OpenSCAD's general failure exit code.
const EXIT_ERROR: u8 = 1;

#[derive(Parser, Debug)]
#[command(
    name = "neoscad serve",
    about = "Keep NeoSCAD's caches warm and answer JSON-RPC requests (docs/serve-protocol.md)"
)]
pub(crate) struct Args {
    /// Listen on a Unix socket instead of stdio: PATH, or without it the
    /// per-user default that the command line looks for. On Windows, a
    /// named pipe: NAME or \\.\pipe\NAME.
    #[arg(long, value_name = "PATH", num_args = 0..=1, default_missing_value = "")]
    socket: Option<String>,

    /// Stop the server listening on the socket.
    #[arg(long, conflicts_with = "status")]
    stop: bool,

    /// Report on the server listening on the socket.
    #[arg(long)]
    status: bool,

    /// With --socket: exit after this many seconds with no connection and
    /// no request (0: never).
    #[arg(long = "idle-timeout", value_name = "SECS", default_value = "1800")]
    idle_timeout: u64,

    /// Geometry cache budget in MiB (per colour scheme and font set).
    #[arg(long = "cache-mb", value_name = "MiB")]
    cache_mb: Option<usize>,

    /// `json`: --status prints the server's status object.
    #[arg(long, value_name = "FORMAT")]
    format: Option<String>,

    /// Change a resource limit, NAME=VALUE (repeatable; 'off' for none):
    /// time (s, default 60), memory (MiB or 4G, default 4G), fragments
    /// (10000), slices (10000), list (1e7), string (64 MiB), rands (1e7),
    /// triangles (1e7), sketch_unknowns (5000), queries (10000). The
    /// command line's own requests are unlimited.
    #[arg(long = "limit", value_name = "NAME=VALUE", action = clap::ArgAction::Append)]
    limit: Vec<String>,
}

/// Run `neoscad serve` with the arguments after `serve`.
pub fn main(args: Vec<OsString>) -> u8 {
    let argv = std::iter::once(OsString::from("neoscad serve")).chain(args);
    let a = match Args::try_parse_from(argv) {
        Ok(a) => a,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() { EXIT_ERROR } else { 0 };
        }
    };
    let socket = match a.socket.as_deref() {
        None | Some("") => client::default_socket(),
        Some(p) => crate::transport::from_arg(p),
    };
    if a.status {
        return status(&socket, a.format.as_deref() == Some("json"));
    }
    if a.stop {
        return match client::call(&socket, "shutdown", Value::Null) {
            Ok(_) => {
                eprintln!("neoscad serve: stopped the server at {}", socket.display());
                0
            }
            Err(e) => {
                eprintln!("neoscad serve: {e} at {}", socket.display());
                EXIT_ERROR
            }
        };
    }
    let host = crate::host::Host::from_env();
    let mut cfg = host.session_config(crate::host::entropy_seed());
    if let Some(mb) = a.cache_mb {
        cfg.geometry_budget = mb << 20;
    }
    cfg.limits = match crate::limits::from_flags(session::Limits::AGENT, &a.limit) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("neoscad serve: {e}");
            return EXIT_ERROR;
        }
    };
    let server = Arc::new(Server {
        session: session::Session::new(cfg),
        binary: client::binary_id(),
        environment: client::environment(),
        socket: a.socket.is_some().then(|| socket.clone()),
        started: Instant::now(),
        last: Mutex::new(Instant::now()),
        active: AtomicUsize::new(0),
        connections: AtomicUsize::new(0),
        idle_timeout: Duration::from_secs(a.idle_timeout),
        stopping: AtomicBool::new(false),
        running: Mutex::new(HashMap::new()),
    });
    if a.socket.is_none() {
        let stdin = std::io::stdin();
        let out: Box<dyn Write + Send> = Box::new(std::io::stdout());
        connection(server, stdin.lock(), Arc::new(Mutex::new(out)));
        return 0;
    }
    match listen(server, &socket) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("neoscad serve: {e}");
            EXIT_ERROR
        }
    }
}

fn status(socket: &Path, as_json: bool) -> u8 {
    match client::call(socket, "status", Value::Null) {
        Ok(v) => {
            if as_json {
                println!("{v}");
            } else {
                let s = &v["stats"];
                println!(
                    "neoscad serve: running at {} (pid {}, up {} s, idle {} s); {} documents, {} requests; geometry cache {} entries, {:.1} MiB of {:.0} MiB",
                    socket.display(),
                    v["pid"],
                    v["uptime_s"],
                    v["idle_s"],
                    s["documents"],
                    s["requests"],
                    s["geometry_cache"]["entries"],
                    s["geometry_cache"]["bytes"].as_f64().unwrap_or(0.0) / 1048576.0,
                    s["geometry_cache"]["budget"].as_f64().unwrap_or(0.0) / 1048576.0,
                );
            }
            0
        }
        Err(e) => {
            if as_json {
                println!(
                    "{}",
                    json!({"running": false, "socket": socket, "error": e.to_string()})
                );
            } else {
                eprintln!("neoscad serve: {e} at {}", socket.display());
            }
            EXIT_ERROR
        }
    }
}

struct Server {
    session: session::Session,
    binary: String,
    environment: Value,
    socket: Option<PathBuf>,
    started: Instant,
    last: Mutex<Instant>,
    active: AtomicUsize,
    connections: AtomicUsize,
    idle_timeout: Duration,
    stopping: AtomicBool,
    /// Request id (as JSON text) to its document, for `$/cancelRequest`.
    running: Mutex<HashMap<String, PathBuf>>,
}

type Writer = Arc<Mutex<Box<dyn Write + Send>>>;

fn send(w: &Writer, msg: &Value) {
    let mut w = w.lock().unwrap_or_else(|p| p.into_inner());
    let _ = rpc::write(&mut *w, msg);
}

impl Server {
    fn touch(&self) {
        *self
            .last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Instant::now();
    }

    fn shutdown(&self) -> ! {
        if let Some(s) = &self.socket {
            crate::transport::remove(s);
        }
        std::process::exit(0)
    }
}

/// Serve one connection until it ends.
fn connection(server: Arc<Server>, mut r: impl BufRead, w: Writer) {
    server.connections.fetch_add(1, Ordering::SeqCst);
    loop {
        let msg = match rpc::read(&mut r) {
            Ok(Some(m)) => m,
            Ok(None) => break,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                send(
                    &w,
                    &rpc::error(&Value::Null, code::PARSE_ERROR, &e.to_string()),
                );
                continue;
            }
            Err(_) => break,
        };
        server.touch();
        let (Some(method), id) = (msg.get("method").and_then(Value::as_str), msg.get("id")) else {
            // A response to nothing we sent, or not a request at all.
            if msg.get("id").is_some() && msg.get("result").is_none() && msg.get("error").is_none()
            {
                send(
                    &w,
                    &rpc::error(&msg["id"], code::INVALID_REQUEST, "not a request"),
                );
            }
            continue;
        };
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        let Some(id) = id.cloned() else {
            notification(&server, method, &params);
            continue;
        };
        match method {
            // Ordered: handled before the next message is read.
            "initialize" | "open" | "update" | "close" | "cancel" | "stats" | "status"
            | "documents" => {
                let reply = answer(&id, || quick(&server, method, &params));
                send(&w, &reply);
            }
            "shutdown" => {
                send(&w, &rpc::response(&id, Value::Null));
                server.stopping.store(true, Ordering::SeqCst);
                if server.socket.is_some() {
                    server.shutdown();
                }
            }
            _ => {
                let (server, w) = (server.clone(), w.clone());
                let method = method.to_string();
                server.active.fetch_add(1, Ordering::SeqCst);
                // The evaluator's stack: `format`, `docs` and `test` parse
                // (and format) on this thread, recursing on the source's
                // nesting, which a default thread's 2 MiB does not hold
                // at the parser's limit. Evaluations make their own.
                let spawned = std::thread::Builder::new()
                    .stack_size(eval::DEFAULT_THREAD_STACK)
                    .spawn(move || {
                        let reply = answer(&id, || heavy(&server, &id, &method, &params, &w));
                        send(&w, &reply);
                        server
                            .running
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .remove(&id.to_string());
                        server.touch();
                        server.active.fetch_sub(1, Ordering::SeqCst);
                    });
                spawned.expect("cannot spawn a request thread");
            }
        }
    }
    server.connections.fetch_sub(1, Ordering::SeqCst);
    server.touch();
}

/// The response to request `id`: `handle`'s result or error, or when it
/// panics, error -32603 with the panic's message. A bug in one request
/// (in the kernel, say) must not end the server and every other client's
/// warm caches with it; the session's locks tolerate a request that
/// panicked while holding one, so the next request runs normally.
fn answer(id: &Value, handle: impl FnOnce() -> Reply) -> Value {
    match guarded(handle) {
        Ok(v) => rpc::response(id, v),
        Err((c, m)) => rpc::error(id, c, &m),
    }
}

/// `handle`'s reply, or error -32603 with the panic's message when it
/// panics (see [`answer`]).
fn guarded(handle: impl FnOnce() -> Reply) -> Reply {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(handle)) {
        Ok(r) => r,
        Err(payload) => {
            let what = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            Err((
                code::INTERNAL_ERROR,
                format!("internal error: the request panicked: {what}"),
            ))
        }
    }
}

/// The server's methods with no transport, for `neoscad mcp` ([`crate::mcp`]):
/// the same session, caches, cancellation and parameter handling as
/// `neoscad serve`, so the two can never disagree about what a request
/// means. Notifications (progress, diagnostics) go nowhere.
pub(crate) struct Local {
    server: Arc<Server>,
    sink: Writer,
}

impl std::fmt::Debug for Local {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Local").finish_non_exhaustive()
    }
}

impl Local {
    pub(crate) fn new(cfg: session::Config) -> Local {
        let server = Arc::new(Server {
            session: session::Session::new(cfg),
            binary: client::binary_id(),
            environment: client::environment(),
            socket: None,
            started: Instant::now(),
            last: Mutex::new(Instant::now()),
            active: AtomicUsize::new(0),
            connections: AtomicUsize::new(0),
            idle_timeout: Duration::ZERO,
            stopping: AtomicBool::new(false),
            running: Mutex::new(HashMap::new()),
        });
        let sink: Box<dyn Write + Send> = Box::new(std::io::sink());
        Local {
            server,
            sink: Arc::new(Mutex::new(sink)),
        }
    }

    pub(crate) fn session(&self) -> &session::Session {
        &self.server.session
    }

    /// Answer one request of `docs/serve-protocol.md` (a model method:
    /// `evaluate`, `render`, `export`, `check`, `measure`, `format`,
    /// `docs`, `test`). `id` keys the request for cancellation.
    pub(crate) fn call(&self, id: &Value, method: &str, params: &Value) -> Reply {
        let mut params = params.clone();
        params["progress"] = json!(false);
        let r = guarded(|| heavy(&self.server, id, method, &params, &self.sink));
        self.server
            .running
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&id.to_string());
        r
    }

    /// Stop request `id` (LSP's `$/cancelRequest`, MCP's
    /// `notifications/cancelled`): it cancels the request's document.
    pub(crate) fn cancel(&self, id: &Value) {
        notification(&self.server, "$/cancelRequest", &json!({"id": id}));
    }
}

fn notification(server: &Server, method: &str, params: &Value) {
    match method {
        "exit" => server.shutdown(),
        "$/cancelRequest" => {
            let key = params.get("id").map(Value::to_string).unwrap_or_default();
            let doc = server
                .running
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .get(&key)
                .cloned();
            if let Some(doc) = doc {
                server.session.cancel(&doc);
            }
        }
        "cancel" => {
            if let Some(p) = params.get("path").and_then(Value::as_str) {
                server.session.cancel(Path::new(p));
            }
        }
        _ => {}
    }
}

pub(crate) type Reply = Result<Value, (i64, String)>;

fn invalid(m: impl Into<String>) -> (i64, String) {
    (code::INVALID_PARAMS, m.into())
}

fn path_param(params: &Value) -> Result<PathBuf, (i64, String)> {
    params
        .get("path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| invalid("missing \"path\""))
}

fn doc_json(d: &session::DocInfo) -> Value {
    json!({"path": d.path, "version": d.version, "length": d.len})
}

/// The names of the methods, for `initialize`.
const METHODS: &[&str] = &[
    "initialize",
    "shutdown",
    "status",
    "stats",
    "documents",
    "open",
    "update",
    "close",
    "cancel",
    "evaluate",
    "render",
    "export",
    "snapshot",
    "check",
    "measure",
    "format",
    "docs",
    "test",
    "cli.export",
    "cli.snapshot",
    "cli.check",
    "cli.measure",
];

fn quick(server: &Server, method: &str, params: &Value) -> Reply {
    let s = &server.session;
    match method {
        "initialize" => Ok(json!({
            "protocol": PROTOCOL_VERSION,
            "server": {"name": "neoscad", "version": env!("CARGO_PKG_VERSION"), "binary": server.binary},
            "capabilities": {
                "methods": METHODS,
                "notifications": {"server": ["progress", "diagnostics"], "client": ["exit", "cancel", "$/cancelRequest"]},
                "export_formats": ["stl", "binstl", "off", "obj", "3mf", "wrl", "pov", "svg", "dxf", "pdf", "png", "echo", "ast", "csg"],
                "render_modes": ["render", "force", "preview"],
                "incremental_edits": true,
                "snapshot": true,
                "check": true,
                "measure": true,
                "format": true,
                "docs": true,
                "test": true,
                // The extensions a client can use: only the implemented
                // ones, so a client does not send sketches to a server
                // that would answer "unknown module". `exact` is left out
                // too: it is STEP export, which only the one-shot command
                // line does so far (`export_formats` has no `step`), and a
                // client that saw it would ask for a file it cannot get.
                "features": eval::Extension::ALL
                    .into_iter()
                    .filter(|e| e.implemented() && *e != eval::Extension::Exact)
                    .map(eval::Extension::name)
                    .collect::<Vec<_>>(),
            },
        })),
        "status" => {
            let idle = server
                .last
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .elapsed()
                .as_secs();
            Ok(json!({
                "pid": std::process::id(),
                "socket": server.socket,
                "protocol": PROTOCOL_VERSION,
                "binary": server.binary,
                "uptime_s": server.started.elapsed().as_secs(),
                "idle_s": idle,
                "idle_timeout_s": server.idle_timeout.as_secs(),
                "connections": server.connections.load(Ordering::SeqCst),
                "stats": s.stats().json(),
            }))
        }
        "stats" => Ok(s.stats().json()),
        "documents" => Ok(json!(
            s.documents().iter().map(doc_json).collect::<Vec<_>>()
        )),
        "open" => {
            let p = path_param(params)?;
            let text = params
                .get("text")
                .and_then(Value::as_str)
                .map(|t| t.as_bytes().to_vec());
            Ok(doc_json(&s.open(&p, text)))
        }
        "update" => {
            let p = path_param(params)?;
            if let Some(t) = params.get("text").and_then(Value::as_str) {
                return Ok(doc_json(&s.update(&p, t.as_bytes().to_vec())));
            }
            let edits: Vec<session::TextEdit> = params
                .get("edits")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid("update needs \"text\" or \"edits\""))?
                .iter()
                .map(|e| {
                    Some(session::TextEdit {
                        start: e.get("start")?.as_u64()? as usize,
                        end: e.get("end")?.as_u64()? as usize,
                        text: e.get("text")?.as_str()?.to_string(),
                    })
                })
                .collect::<Option<_>>()
                .ok_or_else(|| invalid("each edit needs \"start\", \"end\" and \"text\""))?;
            s.edit(&p, &edits).map(|d| doc_json(&d)).map_err(invalid)
        }
        "close" => Ok(json!({"closed": s.close(&path_param(params)?)})),
        "cancel" => Ok(json!({"cancelled": s.cancel(&path_param(params)?)})),
        _ => Err((code::METHOD_NOT_FOUND, format!("unknown method '{method}'"))),
    }
}

/// The session's run for a request's parameters. `base` is the server's
/// limits, which a request's `limits` object changes.
fn run_of(
    params: &Value,
    id: &Value,
    w: &Writer,
    base: session::Limits,
) -> Result<session::Run, (i64, String)> {
    let input = params
        .get("path")
        .or_else(|| params.get("input"))
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("missing \"path\""))?;
    let mut run = session::Run::new(input);
    run.cwd = params.get("cwd").and_then(Value::as_str).map(PathBuf::from);
    run.defines = params
        .get("defines")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    run.quiet = params
        .get("quiet")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    run.rng_seed = params.get("seed").and_then(Value::as_u64).map(|n| n as u32);
    run.limits = crate::limits::of_params(params, base).map_err(invalid)?;
    // An editor's requests supersede older ones on the document (their
    // text is stale); `neoscad mcp` turns that off, because an agent's
    // parallel calls on one file (a check and a measure) are both wanted.
    run.supersede = params
        .get("supersede")
        .and_then(Value::as_bool)
        .unwrap_or(true);
    // NeoSCAD's extensions: `"enable": ["part", "sketch"]` as on the
    // command line, or `"parts": true` for `part()` alone.
    let enable: Vec<String> = params
        .get("enable")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    run.extensions = crate::extensions(&enable).with_if(
        eval::Extension::Part,
        params
            .get("parts")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    );
    // OpenSCAD's experimental features, by the same names.
    run.features = crate::features(&enable);
    if params
        .get("progress")
        .and_then(Value::as_bool)
        .unwrap_or(true)
    {
        let (id, w) = (id.clone(), w.clone());
        run.progress = Some(Arc::new(move |stage: session::Stage| {
            send(
                &w,
                &rpc::notification("progress", json!({"id": id, "stage": stage.name()})),
            );
        }));
    }
    Ok(run)
}

/// A message of the parsers the command line shares with the server
/// (`--nozzle must be ...`) with each flag named as a request spells the
/// parameter (`nozzle`): a server's or an agent's client has never seen
/// the flags.
pub(crate) fn param_names(msg: &str) -> String {
    const NAMES: &[(&str, &str)] = &[
        ("`--enable part`", "`parts: true`"),
        ("--enable part", "`parts: true`"),
        ("--max-overhang", "`max_overhang`"),
        ("--min-wall", "`min_wall`"),
        ("--nozzle", "`nozzle`"),
        ("--bed", "`bed`"),
        ("--between", "`between`"),
        ("--section", "`section`"),
        ("--axis", "`axis`"),
        ("--center", "`center`"),
        ("--profile", "`profile`"),
        ("--size", "`size`"),
        ("--lighting", "`lighting`"),
        ("--highlight", "`highlight`"),
        ("--issues", "`issues`"),
        ("--sketch", "`sketch`"),
    ];
    let mut out = msg.to_string();
    for (flag, name) in NAMES {
        out = out.replace(flag, name);
    }
    out
}

fn cancelled(_: session::Cancelled) -> (i64, String) {
    (
        code::CANCELLED,
        "cancelled by a newer request on the document".into(),
    )
}

fn log_json(log: &session::Log) -> Value {
    use lang::diag::Severity;
    json!({
        "counts": {
            "errors": log.count(Severity::Error),
            "warnings": log.count(Severity::Warning),
            "echoes": log.count(Severity::Echo),
        },
        "diagnostics": log.diagnostics_json(),
        "echo": log.echo(),
    })
}

fn merge(mut a: Value, b: Value) -> Value {
    if let (Value::Object(a), Value::Object(b)) = (&mut a, b) {
        a.extend(b);
    }
    a
}

fn publish(w: &Writer, doc: &Path, log: &session::Log) {
    send(
        w,
        &rpc::notification(
            "diagnostics",
            json!({"path": session::normal(doc), "diagnostics": log.diagnostics_json()}),
        ),
    );
}

fn heavy(server: &Server, id: &Value, method: &str, params: &Value, w: &Writer) -> Reply {
    let s = &server.session;
    // A request that panics on purpose, for the tests of [`answer`]; only
    // a server started with this variable set has it.
    if method == "debug.panic" && std::env::var_os("NEOSCAD_SERVE_TEST_PANIC").is_some() {
        panic!("debug.panic requested");
    }
    if method.starts_with("cli.") {
        return cli(server, method, params);
    }
    match method {
        "format" => return format_method(s, params),
        "docs" => return docs_method(s, params),
        "test" => return test_method(s, params),
        _ => {}
    }
    if !matches!(
        method,
        "evaluate" | "render" | "export" | "snapshot" | "check" | "measure"
    ) {
        return Err((code::METHOD_NOT_FOUND, format!("unknown method '{method}'")));
    }
    let run = run_of(params, id, w, s.config().limits)?;
    let cwd = run
        .cwd
        .clone()
        .unwrap_or_else(|| s.config().work_dir.clone());
    let doc = cwd.join(&run.input);
    server
        .running
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(id.to_string(), doc.clone());
    match method {
        "evaluate" => {
            let csg = params.get("csg").and_then(Value::as_bool).unwrap_or(false);
            let r = s.evaluate(&run, csg).map_err(cancelled)?;
            publish(w, &doc, &r.log);
            Ok(merge(
                json!({"exit_code": r.exit_code, "aborted": r.aborted, "csg": r.csg,
                       "timings_ms": r.timings.json()}),
                log_json(&r.log),
            ))
        }
        "render" => {
            let mode = match params
                .get("mode")
                .and_then(Value::as_str)
                .unwrap_or("render")
            {
                "render" => session::Mode::Render,
                "force" => session::Mode::Force,
                "preview" => session::Mode::Preview,
                m => {
                    return Err(invalid(format!(
                        "unknown mode '{m}' (render, force or preview)"
                    )));
                }
            };
            let scheme = render::ColorScheme::cornfield();
            let r = s.render(&run, mode, &scheme).map_err(cancelled)?;
            publish(w, &doc, &r.log);
            let preview_bbox = r.tree.as_ref().map(|t| {
                t.bounding_box(false)
                    .map_or(Value::Null, |(lo, hi)| session::stats::bbox_json(&lo, &hi))
            });
            Ok(merge(
                json!({
                    "exit_code": r.exit_code,
                    "geometry": r.geometry_json(&scheme.geometry_scheme()),
                    "preview_bbox": preview_bbox,
                    "cache_entries": r.cache_entries,
                    "timings_ms": r.timings.json(),
                }),
                log_json(&r.log),
            ))
        }
        "export" => export(server, run, params, w, &doc),
        "snapshot" => {
            let mut p = params.clone();
            p["model"] = json!(run.input);
            p["json"] = json!(true);
            p["supersede"] = json!(run.supersede);
            p["limits"] = crate::limits::json(&run.limits.unwrap_or(s.config().limits));
            if run.extensions.has(eval::Extension::Part) {
                p["parts"] = json!(true);
            }
            if let Some(e) = params.get("enable") {
                p["enable"] = e.clone();
            }
            let out = crate::snapshot::execute(s, &p, &cwd, run.progress.clone());
            let summary: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
            if out.exit_code != 0 && summary.is_null() {
                return Err((
                    code::FAILED,
                    String::from_utf8_lossy(&out.stderr).trim().to_string(),
                ));
            }
            Ok(merge(summary, json!({"exit_code": out.exit_code})))
        }
        "check" => {
            let settings =
                crate::check::settings_of(params).map_err(|e| invalid(param_names(&e)))?;
            let c = s
                .check(&session::check::CheckRequest { run, settings })
                .map_err(cancelled)?;
            publish(w, &doc, &c.log);
            Ok(merge(c.summary, json!({"exit_code": c.exit_code})))
        }
        "measure" => {
            let req =
                crate::measure::request_of(params, run).map_err(|e| invalid(param_names(&e)))?;
            let m = s.measure(&req).map_err(cancelled)?;
            publish(w, &doc, &m.log);
            let mut summary = m.summary;
            match (params.get("svg"), &m.svg) {
                // A file name: the server writes it, relative to `cwd`.
                (Some(Value::String(file)), Some(svg)) => {
                    std::fs::write(cwd.join(file), svg)
                        .map_err(|e| (code::FAILED, format!("cannot write '{file}': {e}")))?;
                    summary["section"]["svg"] = json!(file);
                }
                // `true`: the SVG text in the result.
                (Some(Value::Bool(true)), Some(svg)) => {
                    summary["section"]["svg_text"] = json!(svg);
                }
                _ => {}
            }
            Ok(summary)
        }
        _ => Err((code::METHOD_NOT_FOUND, format!("unknown method '{method}'"))),
    }
}

fn str_param(params: &Value, key: &str) -> Option<String> {
    params.get(key).and_then(Value::as_str).map(str::to_string)
}

fn usize_param(params: &Value, key: &str) -> Result<Option<usize>, (i64, String)> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(|n| Some(n as usize))
            .ok_or_else(|| invalid(format!("\"{key}\" must be a whole number"))),
    }
}

/// `format`: a file's or a text's formatted text; nothing is written.
fn format_method(s: &session::Session, params: &Value) -> Reply {
    let req = session::format::FormatRequest {
        input: str_param(params, "path").or_else(|| str_param(params, "input")),
        text: str_param(params, "text").map(String::into_bytes),
        cwd: str_param(params, "cwd").map(PathBuf::from),
        indent: usize_param(params, "indent")?,
        width: usize_param(params, "width")?,
    };
    if req.input.is_none() && req.text.is_none() {
        return Err(invalid("missing \"path\" or \"text\""));
    }
    let diff = params.get("diff").and_then(Value::as_bool).unwrap_or(false);
    let f = s.format(&req);
    let exit_code = u8::from(f.result.is_err());
    Ok(merge(f.json(true, diff), json!({"exit_code": exit_code})))
}

/// `docs`: a builtin's or a file's definition's reference.
fn docs_method(s: &session::Session, params: &Value) -> Reply {
    let r = s.docs(&session::docs::DocsRequest {
        name: str_param(params, "name"),
        file: str_param(params, "file").or_else(|| str_param(params, "in")),
        cwd: str_param(params, "cwd").map(PathBuf::from),
        full: params.get("full").and_then(Value::as_bool).unwrap_or(false),
        brief: params
            .get("brief")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        // A request names its file with `file`; a client that wraps the
        // method (the MCP server's `path`) says how its own users do.
        file_arg: Some(str_param(params, "file_arg").unwrap_or_else(|| "`file`".into())),
    });
    Ok(merge(r.json, json!({"text": r.text})))
}

/// `test`: discover and run model tests.
fn test_method(s: &session::Session, params: &Value) -> Reply {
    let mut paths: Vec<String> = params
        .get("paths")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    paths.extend(str_param(params, "path"));
    let enable: Vec<String> = params
        .get("enable")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    let jobs = usize_param(params, "jobs")?.unwrap_or_else(|| {
        std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
    });
    let req = session::modeltest::TestRequest {
        paths,
        cwd: str_param(params, "cwd").map(PathBuf::from),
        filter: str_param(params, "filter"),
        extensions: crate::extensions(&enable).with_if(
            eval::Extension::Part,
            params
                .get("parts")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        ),
        features: crate::features(&enable),
        jobs: jobs.max(1),
    };
    let r = s.test(&req).map_err(cancelled)?;
    Ok(r.json)
}

/// `export`: one output, written by the server.
fn export(server: &Server, run: session::Run, params: &Value, w: &Writer, doc: &Path) -> Reply {
    let output = params
        .get("output")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid("missing \"output\""))?;
    let id = match params.get("format").and_then(Value::as_str) {
        Some(f) => f.to_string(),
        None => Path::new(output)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default(),
    };
    let cwd = run
        .cwd
        .clone()
        .unwrap_or_else(|| server.session.config().work_dir.clone());
    let Some(format) = session::export::Format::from_id(&id) else {
        return export_other(server, run, &id, output, params, w, doc, &cwd);
    };
    let options: Vec<String> = params
        .get("options")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    let scheme = render::ColorScheme::cornfield();
    let settings = crate::run::encode_settings(
        &crate::export_options::ExportOptions::parse(&options),
        scheme.geometry_scheme(),
        &run.input,
        &run.camera,
        // `"enable": ["predictible-output"]` (or `"all"`) reached
        // `run.features` with the request's other features; the session
        // applies it again from there when it encodes.
        run.features.union(server.session.config().features),
    );
    struct Files<'a> {
        cwd: &'a Path,
        bytes: usize,
    }
    impl session::ExportSink for Files<'_> {
        fn write(&mut self, target: &str, data: &[u8]) -> Result<(), String> {
            self.bytes = data.len();
            std::fs::write(self.cwd.join(target), data)
                .map_err(|e| format!("ERROR: Can't write to '{target}': {e}"))
        }
        fn summary(
            &mut self,
            _: &session::SummaryFacts<'_>,
            _: &mut eval::Console<Vec<u8>>,
        ) -> bool {
            true
        }
    }
    let req = session::ExportRequest {
        run,
        outputs: vec![(output.to_string(), format)],
        force: params
            .get("force")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        scheme,
        settings,
    };
    let mut files = Files {
        cwd: &cwd,
        bytes: 0,
    };
    let r = server.session.export(&req, &mut files).map_err(cancelled)?;
    publish(w, doc, &r.log);
    Ok(merge(
        json!({
            "exit_code": r.exit_code,
            "output": output,
            "format": format.id(),
            "bytes": files.bytes,
            "geometry": r.geometry.as_ref().map(|g| session::stats::geometry(g, &req.scheme.geometry_scheme())),
            "timings_ms": r.timings.json(),
        }),
        log_json(&r.log),
    ))
}

/// `export` to a format that is not a mesh: `echo` (every message, as the
/// `.echo` export holds them), `ast`, `csg` (the node tree) and `png`
/// (OpenSCAD's image at its default camera: the preview, or with
/// `"mode": "render"` the rendered geometry).
#[allow(clippy::too_many_arguments)]
fn export_other(
    server: &Server,
    run: session::Run,
    id: &str,
    output: &str,
    params: &Value,
    w: &Writer,
    doc: &Path,
    cwd: &Path,
) -> Reply {
    let s = &server.session;
    let (exit_code, data, log, timings) = match id {
        "echo" | "csg" => {
            let r = s.evaluate(&run, id == "csg").map_err(cancelled)?;
            let data = match id {
                "echo" => Some(r.log.stderr.clone()),
                _ => r.csg.map(String::into_bytes),
            };
            (r.exit_code, data, r.log, r.timings.json())
        }
        "ast" => {
            let (text, log) = s.ast(&run).map_err(cancelled)?;
            let code = if text.is_some() { 0 } else { 1 };
            (code, text.map(String::into_bytes), log, Value::Null)
        }
        "png" => {
            let render = params.get("mode").and_then(Value::as_str) == Some("render");
            let mode = if render {
                session::Mode::Render
            } else {
                session::Mode::Preview
            };
            let scheme = render::ColorScheme::cornfield();
            let r = s.render(&run, mode, &scheme).map_err(cancelled)?;
            let data = if r.exit_code == 0 {
                let settings = crate::png::Settings {
                    camera: crate::png::camera(None, false, false, None, None)
                        .map_err(|_| (code::INTERNAL_ERROR, "no default camera".to_string()))?,
                    scheme,
                    previewer: (!render).then_some(render::Previewer::OpenCsg),
                    view: render::ViewOptions::default(),
                    csg_limit: geom::csg::DEFAULT_TERM_LIMIT,
                };
                let drawn = match &r.tree {
                    Some(t) => crate::png::preview_png(&settings, t, &r.camera),
                    None => crate::png::render_png(&settings, r.geometry.as_ref(), &r.camera),
                };
                Some(
                    drawn
                        .map_err(|e| (code::FAILED, format!("cannot export PNG: {e}")))?
                        .0,
                )
            } else {
                None
            };
            (r.exit_code, data, r.log, r.timings.json())
        }
        _ => return Err(invalid(format!("cannot export '{id}'"))),
    };
    let bytes = data.as_ref().map_or(0, Vec::len);
    if let Some(d) = &data {
        std::fs::write(cwd.join(output), d).map_err(|e| {
            (
                code::FAILED,
                format!("ERROR: Can't write to '{output}': {e}"),
            )
        })?;
    }
    publish(w, doc, &log);
    Ok(merge(
        json!({
            "exit_code": exit_code,
            "output": output,
            "format": id,
            "bytes": bytes,
            "geometry": Value::Null,
            "timings_ms": timings,
        }),
        log_json(&log),
    ))
}

/// `cli.export` and `cli.snapshot`: a command line run for a client of the
/// same build in the same environment.
fn cli(server: &Server, method: &str, params: &Value) -> Reply {
    if params.get("binary").and_then(Value::as_str) != Some(server.binary.as_str()) {
        return Err((
            code::FAILED,
            "the server is another build of neoscad".into(),
        ));
    }
    if params.get("environment") != Some(&server.environment) {
        return Err((
            code::FAILED,
            "the server runs with another OPENSCADPATH or font path".into(),
        ));
    }
    let cwd = params
        .get("cwd")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| invalid("missing \"cwd\""))?;
    // The command line is unlimited, as OpenSCAD is, whether it runs in
    // its own process or here; its `--limit` runs never come here.
    let mut params = params.clone();
    if params.get("limits").is_none_or(Value::is_null) {
        params["limits"] = crate::limits::json(&session::Limits::NONE);
    }
    let params = &params;
    let out = match method {
        "cli.export" => crate::delegate::execute(&server.session, params, &cwd),
        "cli.snapshot" => crate::snapshot::execute(&server.session, params, &cwd, None),
        "cli.check" => crate::check::execute(&server.session, params, &cwd, None),
        "cli.measure" => crate::measure::execute(&server.session, params, &cwd, None),
        _ => return Err((code::METHOD_NOT_FOUND, format!("unknown method '{method}'"))),
    };
    Ok(out.json())
}

/// Listen on `path` until `shutdown` or the idle timeout.
fn listen(server: Arc<Server>, path: &Path) -> Result<(), String> {
    let listener = crate::transport::Listener::bind(path)?;
    eprintln!(
        "neoscad serve: listening at {} (protocol {PROTOCOL_VERSION}, pid {})",
        path.display(),
        std::process::id()
    );
    if !server.idle_timeout.is_zero() {
        let s = server.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(500));
                let idle = s
                    .last
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .elapsed();
                if s.connections.load(Ordering::SeqCst) == 0
                    && s.active.load(Ordering::SeqCst) == 0
                    && idle >= s.idle_timeout
                {
                    eprintln!("neoscad serve: idle for {} s, exiting", idle.as_secs());
                    s.shutdown();
                }
            }
        });
    }
    loop {
        let conn = match listener.accept() {
            Ok(c) => c,
            Err(_) => {
                // A client that went away mid-connect. Pause, so an error
                // that repeats (a pipe instance Windows will not create)
                // cannot spin a core.
                std::thread::sleep(Duration::from_millis(10));
                continue;
            }
        };
        let s = server.clone();
        std::thread::spawn(move || {
            connection(
                s,
                BufReader::new(conn.reader),
                Arc::new(Mutex::new(conn.writer)),
            );
        });
    }
}
