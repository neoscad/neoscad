//! `neoscad serve`: a long-lived process holding a [`session::Session`],
//! answering JSON-RPC 2.0 over stdio or a Unix socket
//! (`docs/serve-protocol.md`).
//!
//! - `neoscad serve` speaks on stdin/stdout (an editor or MCP host starts
//!   it as a child) and ends at the end of input or on `exit`.
//! - `neoscad serve --socket [PATH]` listens on a Unix socket, by default
//!   the per-user one the command line looks for ([`client::default_socket`]),
//!   so `neoscad IN -o OUT` and `neoscad snapshot` use it automatically.
//!   The socket's directory is made user-only (0700) and the socket 0600;
//!   there is no network listener. It exits after `--idle-timeout`
//!   seconds with no connection and no request.
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
struct Args {
    /// Listen on a Unix socket instead of stdio: PATH, or without it the
    /// per-user default that the command line looks for.
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
        Some(p) => PathBuf::from(p),
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
            let _ = std::fs::remove_file(s);
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
                std::thread::spawn(move || {
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
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(handle)) {
        Ok(Ok(v)) => rpc::response(id, v),
        Ok(Err((c, m))) => rpc::error(id, c, &m),
        Err(payload) => {
            let what = payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "unknown panic".into());
            rpc::error(
                id,
                code::INTERNAL_ERROR,
                &format!("internal error: the request panicked: {what}"),
            )
        }
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

type Reply = Result<Value, (i64, String)>;

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
                "features": ["part"],
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

/// The session's run for a request's parameters.
fn run_of(params: &Value, id: &Value, w: &Writer) -> Result<session::Run, (i64, String)> {
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
    // neoscad's `part()`: `"enable": ["part"]` as on the command line, or
    // `"parts": true`.
    let enable: Vec<String> = params
        .get("enable")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_string))
        .collect();
    run.parts = crate::parts_enabled(&enable)
        || params
            .get("parts")
            .and_then(Value::as_bool)
            .unwrap_or(false);
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
    if !matches!(
        method,
        "evaluate" | "render" | "export" | "snapshot" | "check" | "measure"
    ) {
        return Err((code::METHOD_NOT_FOUND, format!("unknown method '{method}'")));
    }
    let run = run_of(params, id, w)?;
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
            p["supersede"] = json!(true);
            if run.parts {
                p["parts"] = json!(true);
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
            let settings = crate::check::settings_of(params).map_err(invalid)?;
            let c = s
                .check(&session::check::CheckRequest { run, settings })
                .map_err(cancelled)?;
            publish(w, &doc, &c.log);
            Ok(merge(c.summary, json!({"exit_code": c.exit_code})))
        }
        "measure" => {
            let req = crate::measure::request_of(params, run).map_err(invalid)?;
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
#[cfg(unix)]
fn listen(server: Arc<Server>, path: &Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};

    let dir = path
        .parent()
        .ok_or_else(|| format!("no directory in socket path {}", path.display()))?;
    if !dir.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    // The socket is how anyone talks to this user's server. The default
    // one lives in a directory that must be this user's alone; a socket
    // placed elsewhere on purpose is only made user-only itself.
    if path == client::default_socket() {
        let meta = std::fs::metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        let uid = nix::unistd::getuid().as_raw();
        if meta.uid() != uid {
            return Err(format!("{} belongs to another user", dir.display()));
        }
        if meta.mode() & 0o077 != 0 {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|e| format!("cannot make {} private: {e}", dir.display()))?;
        }
    }
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(format!(
                "a server is already listening at {}",
                path.display()
            ));
        }
        // Left by a server that did not exit cleanly.
        std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    let listener = UnixListener::bind(path)
        .map_err(|e| format!("cannot listen at {}: {e}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("{}: {e}", path.display()))?;
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
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let Ok(write) = stream.try_clone() else {
            continue;
        };
        let s = server.clone();
        std::thread::spawn(move || {
            let w: Box<dyn Write + Send> = Box::new(write);
            connection(s, BufReader::new(stream), Arc::new(Mutex::new(w)));
        });
    }
    Ok(())
}

#[cfg(not(unix))]
fn listen(_server: Arc<Server>, _path: &Path) -> Result<(), String> {
    Err("Unix sockets are not available here; use `neoscad serve` on stdio".into())
}
