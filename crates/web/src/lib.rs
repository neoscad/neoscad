//! The browser demo's worker core: the requests of the web worker protocol
//! (`docs/web-protocol.md`) over one [`client::Client`], the language
//! server and an in-memory file system.
//!
//! [`Worker::handle`] takes one request as JSON text (with its byte
//! buffers beside it) and returns the reply envelope the same way. It is
//! plain Rust, so the native tests drive exactly what the worker runs;
//! `wasm.rs` (wasm32 only) is the thin wasm-bindgen layer the worker's
//! JavaScript calls (`js/worker.js` is a reference worker).
//!
//! What the worker has instead of a machine, since it is a library crate
//! like the others (`CLAUDE.md`, "Rules"):
//!
//! - files: a [`MemFs`] under the bundled libraries (MCAD at
//!   `/neoscad/libraries`) and fonts; the page adds BOSL2 with `addFiles`;
//! - the clock: passed to [`Worker::new`] (`performance.now()` in the
//!   browser), for timings and the time limit;
//! - a measurement of memory in use, for the memory limit: passed to
//!   [`Worker::with_probe`] (the wasm32 build's counting allocator);
//! - the seed of unseeded `rands()` and the creation date of exports: from
//!   the page's requests.
//!
//! Every request runs to the end on the worker's one thread; a stale or
//! runaway one is stopped by the page terminating the worker (see the
//! protocol's "Crashes, cancelling and respawning").

#[cfg(target_arch = "wasm32")]
mod heap;
mod tar;
#[cfg(target_arch = "wasm32")]
mod wasm;

use std::path::PathBuf;
use std::sync::Arc;

use client::{
    CheckOptions, Client, CoreError, DocumentRequest, ExportOptions, ParameterOverride, RenderMode,
    ResourceLimits, RunOptions, SectionAxis,
};
use lang::loader::{FileSystem, LibraryPath};
use lang::vfs::MemFs;
use serde::Deserialize;
use serde_json::{Value, json};

/// Where the bundled libraries (and BOSL2, once added) are mounted: on the
/// library path, as OpenSCAD's `<resources>/libraries`.
pub const LIBRARY_DIR: &str = "/neoscad/libraries";

/// The worker's limits unless `init` says otherwise: the agent limits, as
/// the app runs models, with memory at 1 GiB. A wasm32 instance can grow
/// to 4 GiB, but browsers refuse or kill tabs well before that, and a
/// model that needs more than 1 GiB would take the page down with it
/// rather than fail with OpenSCAD's resource error.
pub const WEB_LIMITS: session::Limits = session::Limits {
    memory: Some(1 << 30),
    ..session::Limits::AGENT
};

/// A monotonic clock in milliseconds.
pub type Clock = session::Clock;

/// One reply: the envelope as JSON text, in which `{"$buffer": n}` stands
/// for `buffers[n]` (the worker moves those to the page as transferable
/// `ArrayBuffer`s instead of copying them through JSON).
#[derive(Debug, Default)]
pub struct Reply {
    pub json: String,
    pub buffers: Vec<Vec<u8>>,
}

/// What `init` makes: everything that lives as long as the worker.
struct State {
    client: Client,
    files: Arc<MemFs>,
    lsp: lsp::Server,
    /// The latest measurement and its handle (see the protocol's
    /// `measure`): one only, so a panel that measures again and again
    /// does not pile up solids in memory that never shrinks.
    measurement: Option<(u32, client::Measurement)>,
    next_measurement: u32,
}

/// The worker: `init` first, then any request (see the crate
/// documentation).
pub struct Worker {
    clock: Option<Clock>,
    probe: Option<session::MemoryProbe>,
    state: Option<State>,
}

impl std::fmt::Debug for Worker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Worker")
            .field("initialised", &self.state.is_some())
            .finish_non_exhaustive()
    }
}

/// A failed request: a [`CoreError`], shaped as the protocol's
/// `{ kind, message }`.
fn error_json(e: &CoreError) -> Value {
    let kind = match e {
        CoreError::Cancelled => "cancelled",
        CoreError::InvalidArgument { .. } => "invalidArgument",
        CoreError::Failed { .. } => "failed",
        CoreError::Panicked { .. } => "panicked",
    };
    json!({ "kind": kind, "message": e.to_string() })
}

fn invalid(message: impl Into<String>) -> CoreError {
    CoreError::InvalidArgument {
        message: message.into(),
    }
}

/// A request's fields as `T`, or which is wrong.
fn fields<T: for<'de> Deserialize<'de>>(request: &Value) -> Result<T, CoreError> {
    T::deserialize(request).map_err(|e| {
        invalid(format!(
            "{}: {e}",
            request["type"].as_str().unwrap_or("request")
        ))
    })
}

fn to_json<T: serde::Serialize>(v: &T) -> Value {
    serde_json::to_value(v).unwrap_or(Value::Null)
}

// --- Requests ---------------------------------------------------------------

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Init {
    limits: Option<ResourceLimits>,
    #[serde(default)]
    seed: u32,
}

#[derive(Deserialize)]
struct PathOnly {
    path: String,
}

#[derive(Deserialize)]
struct Open {
    path: String,
    text: Option<String>,
}

#[derive(Deserialize)]
struct Update {
    path: String,
    text: String,
}

/// An editor position: 0-based line, UTF-16 column (LSP's).
#[derive(Deserialize, Clone, Copy)]
struct Position {
    line: u32,
    character: u32,
}

#[derive(Deserialize)]
struct Edit {
    start: Position,
    end: Position,
    text: String,
}

#[derive(Deserialize)]
struct Edits {
    path: String,
    edits: Vec<Edit>,
}

#[derive(Deserialize)]
struct CameraIn {
    vpt: [f64; 3],
    vpr: [f64; 3],
    vpd: f64,
    vpf: f64,
}

#[derive(Deserialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
enum PreviewerIn {
    #[default]
    OpenCsg,
    ThrownTogether,
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Run {
    path: String,
    mode: RenderMode,
    #[serde(default)]
    overrides: Vec<ParameterOverride>,
    #[serde(default)]
    parts: bool,
    #[serde(default)]
    enable: Vec<String>,
    camera: Option<CameraIn>,
    #[serde(default = "yes")]
    scene: bool,
    color_scheme: Option<String>,
    #[serde(default)]
    previewer: PreviewerIn,
}

#[derive(Deserialize)]
struct Check {
    path: String,
    options: Option<CheckOptions>,
    #[serde(default)]
    run: RunOptions,
}

#[derive(Deserialize)]
struct Measure {
    path: String,
    #[serde(default)]
    run: RunOptions,
}

#[derive(Deserialize)]
struct Section {
    measurement: u32,
    axis: SectionAxis,
    offset: f64,
    part: Option<String>,
}

#[derive(Deserialize)]
struct Between {
    measurement: u32,
    a: String,
    b: String,
}

#[derive(Deserialize)]
struct Pick {
    measurement: u32,
    origin: Vec<f64>,
    direction: Vec<f64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Export {
    path: String,
    format: String,
    #[serde(default)]
    options: ExportOptions,
    #[serde(default)]
    run: RunOptions,
    creation_date: Option<String>,
}

/// A file's contents: text, or one of the request's buffers.
#[derive(Deserialize)]
#[serde(untagged)]
enum Data {
    Text(String),
    Buffer {
        #[serde(rename = "$buffer")]
        index: usize,
    },
}

#[derive(Deserialize)]
struct FileIn {
    path: String,
    data: Data,
}

#[derive(Deserialize)]
struct AddFiles {
    #[serde(default)]
    files: Vec<FileIn>,
    tar: Option<Data>,
    root: Option<String>,
}

#[derive(Deserialize)]
struct Lsp {
    message: String,
}

#[derive(Deserialize)]
struct SetLimits {
    limits: ResourceLimits,
}

/// The MIME type of an export format, for the page's download.
fn mime(format: session::export::Format) -> &'static str {
    match format.id() {
        "stl" | "binstl" => "model/stl",
        "3mf" => "model/3mf",
        "obj" => "model/obj",
        "wrl" => "model/vrml",
        "svg" => "image/svg+xml",
        "dxf" => "image/vnd.dxf",
        "pdf" => "application/pdf",
        "pov" => "text/plain",
        _ => "application/octet-stream",
    }
}

/// Keeps an export's bytes for the reply.
struct Keep {
    data: Vec<u8>,
}

impl session::ExportSink for Keep {
    fn write(&mut self, _: &str, data: &[u8]) -> Result<(), String> {
        self.data = data.to_vec();
        Ok(())
    }

    /// The page shows statistics from the result, not the command line's
    /// summary lines.
    fn summary(&mut self, _: &session::SummaryFacts<'_>, _: &mut eval::Console<Vec<u8>>) -> bool {
        true
    }
}

/// The session configuration of the worker: files in memory with the
/// bundled libraries and fonts over them, the clock, the seed and the
/// limits.
fn config(
    files: Arc<MemFs>,
    clock: Option<Clock>,
    probe: Option<session::MemoryProbe>,
    seed: u32,
) -> session::Config {
    let base: Arc<dyn FileSystem + Send + Sync> = files;
    let fs: Arc<dyn FileSystem + Send + Sync> = Arc::new(assets::libraries(base, LIBRARY_DIR));
    let mut cfg = session::Config::new(fs.clone(), LibraryPath(vec![PathBuf::from(LIBRARY_DIR)]));
    cfg.work_dir = PathBuf::from("/doc");
    // The bundled Liberation fonts, then any `use <font.ttf>` the program
    // makes of a file the page added, as the app's host does.
    cfg.fonts = Arc::new(move |used: &[String]| {
        let mut db = text::FontDb::with_fs(fs.clone());
        assets::add_fonts(&mut db);
        for u in used {
            let p = std::path::Path::new(u);
            if session::is_font(u) && fs.exists(p) && !fs.is_dir(p) {
                db.add_file(p);
            }
        }
        db
    });
    cfg.clock = clock;
    cfg.rng_seed = seed;
    cfg.limits = WEB_LIMITS;
    cfg.memory_probe = probe;
    cfg
}

impl Worker {
    /// A worker that has not been initialised; `clock` times requests and
    /// enforces the time limit (without one, timings are 0 and there is
    /// no time limit).
    pub fn new(clock: Option<Clock>) -> Worker {
        Worker {
            clock,
            probe: None,
            state: None,
        }
    }

    /// This worker, also measuring memory with `probe` against the
    /// memory limit (`session::Config::memory_probe`). The wasm32 build
    /// passes the instance's live heap (`heap.rs`): the estimate does not
    /// count a geometry kernel's working memory, and a wasm32 allocation
    /// that fails aborts the instance instead of returning an error.
    pub fn with_probe(mut self, probe: Option<session::MemoryProbe>) -> Worker {
        self.probe = probe;
        self
    }

    /// Handle one request (`docs/web-protocol.md`): `request` is its JSON
    /// text, in which `{"$buffer": n}` stands for `buffers[n]`. The reply
    /// is the whole envelope, `{ id, ok, result | error }`.
    pub fn handle(&mut self, request: &str, buffers: Vec<Vec<u8>>) -> Reply {
        let request: Value = match serde_json::from_str(request) {
            Ok(v) => v,
            Err(e) => {
                return Reply {
                    json: json!({
                        "id": Value::Null,
                        "ok": false,
                        "error": error_json(&invalid(format!("the request is not JSON: {e}"))),
                    })
                    .to_string(),
                    buffers: Vec::new(),
                };
            }
        };
        let id = request.get("id").cloned().unwrap_or(Value::Null);
        let mut out = Vec::new();
        let envelope = match self.dispatch(&request, buffers, &mut out) {
            Ok(result) => json!({ "id": id, "ok": true, "result": result }),
            Err(e) => {
                out.clear();
                json!({ "id": id, "ok": false, "error": error_json(&e) })
            }
        };
        Reply {
            json: envelope.to_string(),
            buffers: out,
        }
    }

    fn dispatch(
        &mut self,
        request: &Value,
        buffers: Vec<Vec<u8>>,
        out: &mut Vec<Vec<u8>>,
    ) -> Result<Value, CoreError> {
        let kind = request["type"]
            .as_str()
            .ok_or_else(|| invalid("a request needs a \"type\""))?;
        if kind == "init" {
            return self.init(fields(request)?);
        }
        let state = self
            .state
            .as_mut()
            .ok_or_else(|| invalid(format!("'{kind}' before 'init'")))?;
        let c = &state.client;
        match kind {
            "setLimits" => {
                let r: SetLimits = fields(request)?;
                c.set_limits(r.limits)?;
                Ok(json!({ "limits": c.limits() }))
            }
            "defaults" => Ok(json!({
                "checkOptions": CheckOptions::default(),
                "limits": ResourceLimits::from(WEB_LIMITS),
                // The tables every app shows, from the core
                // (`docs/web-protocol.md`, "defaults").
                "tables": {
                    "consoleGroups": client::console_groups(),
                    "printerPresets": client::printer_presets(),
                    "exportFormats": client::export_formats(),
                    "previewDelayMs": client::DEFAULT_PREVIEW_DELAY_MS,
                },
            })),
            "stats" => Ok(json!({
                "memoryBytes": memory_bytes(),
                "heapBytes": heap_bytes(),
            })),
            "open" => {
                let r: Open = fields(request)?;
                Ok(to_json(&c.open(&r.path, r.text)?))
            }
            "update" => {
                let r: Update = fields(request)?;
                Ok(to_json(&c.update(&r.path, r.text)?))
            }
            "edit" => {
                let r: Edits = fields(request)?;
                Ok(to_json(&state.edit(r)?))
            }
            "close" => {
                let r: PathOnly = fields(request)?;
                Ok(json!({ "closed": c.close(&r.path)? }))
            }
            "run" => state.run(fields(request)?, out, self.clock.as_ref()),
            "parameters" => {
                let r: PathOnly = fields(request)?;
                Ok(json!({ "groups": c.parameters(&r.path)? }))
            }
            "check" => {
                let r: Check = fields(request)?;
                let options = r.options.unwrap_or_default();
                options.to_session()?;
                let run = c.detached(&r.path, &r.run, None, None)?;
                Ok(to_json(&c.check(run, &options)?))
            }
            "measure" => state.measure(fields(request)?),
            "section" => {
                let r: Section = fields(request)?;
                let m = state.measurement(r.measurement)?;
                Ok(to_json(&m.section(r.axis, r.offset, r.part.as_deref())?))
            }
            "between" => {
                let r: Between = fields(request)?;
                let m = state.measurement(r.measurement)?;
                Ok(to_json(&m.between(r.a, r.b)?))
            }
            "pick" => {
                let r: Pick = fields(request)?;
                let m = state.measurement(r.measurement)?;
                Ok(json!({ "point": m.pick(&r.origin, &r.direction)? }))
            }
            "export" => state.export(fields(request)?, out),
            "addFiles" => state.add_files(fields(request)?, buffers),
            "readFile" => {
                let r: PathOnly = fields(request)?;
                Ok(json!({ "text": c.read_file(&r.path)? }))
            }
            "lsp" => {
                let r: Lsp = fields(request)?;
                let messages = state.lsp.handle(&c.session, &r.message);
                Ok(json!({ "messages": messages }))
            }
            other => Err(invalid(format!("unknown request type '{other}'"))),
        }
    }

    fn init(&mut self, r: Init) -> Result<Value, CoreError> {
        if self.state.is_some() {
            return Err(invalid("the worker is already initialised"));
        }
        let files = Arc::new(MemFs::new());
        let client = Client::new(config(
            files.clone(),
            self.clock.clone(),
            self.probe.clone(),
            r.seed,
        ));
        if let Some(l) = r.limits {
            client.set_limits(l)?;
        }
        // The page keeps the session's copy of each document (`open`,
        // `update`, `edit`), and a document's diagnostics come from its
        // runs (`language` in `run`'s result): the server never evaluates
        // on its own, which on one thread would block the next run.
        let lsp = lsp::Server::new(lsp::Options {
            sync_session: false,
            limits: None,
            host_diagnostics: true,
        });
        let reply = json!({
            "version": env!("CARGO_PKG_VERSION"),
            "limits": client.limits(),
            "libraryDirs": client.library_dirs(),
        });
        self.state = Some(State {
            client,
            files,
            lsp,
            measurement: None,
            next_measurement: 1,
        });
        Ok(reply)
    }
}

/// The wasm memory's size in bytes (0 natively): what a page watches to
/// decide when to respawn the worker, since wasm memory never shrinks.
fn memory_bytes() -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        core::arch::wasm32::memory_size(0) as u64 * 65_536
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        0
    }
}

/// Bytes the instance has allocated and not freed (0 natively).
fn heap_bytes() -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        heap::live()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        0
    }
}

impl State {
    /// Edits in editor positions, converted to the session's byte offsets
    /// through `lang::source` against the text each applies to.
    fn edit(&self, r: Edits) -> Result<client::DocInfo, CoreError> {
        let c = &self.client;
        let doc = c.doc_path(&r.path)?;
        let mut text = c.text_now(&doc)?.to_vec();
        for e in r.edits {
            let src = lang::source::SourceFile::new(doc.clone(), text.clone());
            let start = src.offset_at_utf16(e.start.line, e.start.character) as usize;
            let end = src.offset_at_utf16(e.end.line, e.end.character) as usize;
            if end < start {
                return Err(invalid("an edit's end is before its start"));
            }
            text.splice(start..end, e.text.into_bytes());
        }
        let text = String::from_utf8(text).map_err(|_| invalid("an edit split a character"))?;
        c.update(&r.path, text)
    }

    fn run(
        &mut self,
        r: Run,
        out: &mut Vec<Vec<u8>>,
        clock: Option<&Clock>,
    ) -> Result<Value, CoreError> {
        let now = || clock.map_or(0.0, |c| c());
        let c = &self.client;
        let request = DocumentRequest {
            mode: r.mode,
            overrides: r.overrides,
            parts: r.parts,
            enable: r.enable,
        };
        let scheme = match &r.color_scheme {
            None => render::ColorScheme::cornfield(),
            Some(name) => render::scheme::find(name)
                .ok_or_else(|| invalid(format!("unknown colour scheme '{name}'")))?,
        };
        let (mut run, doc, text) = c.document_run(&r.path, &request)?;
        match r.camera {
            // The program sees the view it is shown in, as in OpenSCAD's
            // GUI (`setRenderVariables`); no "Viewall and autocenter
            // disabled" warning, which only the command line's
            // `--viewall` earns.
            Some(cam) => {
                run.camera = eval::Camera {
                    vpt: cam.vpt,
                    vpr: cam.vpr,
                    vpd: cam.vpd,
                    vpf: cam.vpf,
                    auto: false,
                    locked: false,
                }
            }
            None => run.camera.auto = false,
        }
        let rendered = c.session.render(&run, request.mode.into(), &scheme)?;
        let language = self.lsp.supply(
            &c.session,
            &doc,
            text.clone(),
            rendered.log.diagnostics_json(),
        );
        // The time the scene took, which the session's timings do not
        // include (see below).
        let mut scene_ms = 0.0;
        let scene = if r.scene {
            let previewer = match r.previewer {
                PreviewerIn::OpenCsg => render::Previewer::OpenCsg,
                PreviewerIn::ThrownTogether => render::Previewer::ThrownTogether,
            };
            // Under the request's limits: the products' booleans of a big
            // difference ran minutes past the time limit before they
            // checked it.
            let started = now();
            let scene = client::run_scene(&rendered, &scheme, previewer)?;
            // The renderer's own packing (`render::packed`), which the
            // page's viewer (`crates/web-view`) reads back with
            // `PackedScene::from_parts`: the two byte arrays as
            // transferable buffers, the rest as its JSON text.
            let packed = scene.map(|s| {
                let p = s.pack();
                let v = json!({
                    "faces": { "$buffer": out.len() },
                    "edges": { "$buffer": out.len() + 1 },
                    "meta": p.meta.to_json(),
                });
                out.push(p.faces);
                out.push(p.edges);
                v
            });
            scene_ms = now() - started;
            packed
        } else {
            None
        };
        let a = rendered.camera_assigned;
        let file_view = a.any().then(|| {
            let cam = &rendered.camera;
            let mut v = json!({});
            if a.vpt {
                v["vpt"] = json!(cam.vpt);
            }
            if a.vpr {
                v["vpr"] = json!(cam.vpr);
            }
            if a.vpd {
                v["vpd"] = json!(cam.vpd);
            }
            if a.vpf {
                v["vpf"] = json!(cam.vpf);
            }
            v
        });
        let files: Vec<String> = c
            .run_files(&rendered, &doc)
            .map(|f| f.to_string_lossy().into_owned())
            .collect();
        let mut render = client::render_result(&rendered, &scheme);
        // A preview's scene is real geometry: each CSG product's boolean
        // (`render::preview`, standing in for OpenCSG's image-space CSG).
        // The session's timings end before it, so a threaded-ring preview
        // said "Previewed in 615 ms" and showed its model 2.6 s later.
        // Counted as geometry, the summary is the time the model took to
        // appear (bar the page drawing it), as a render's is.
        render.timings.geometry_ms += scene_ms;
        render.timings.total_ms += scene_ms;
        Ok(json!({
            // The console's summary line and its tooltip, worded by the
            // core as every app words them (`client::describe_render`).
            "summary": client::describe_render(&render, request.mode),
            "timingsText": client::describe_timings(&render.timings),
            "render": render,
            "console": c.console_lines(&rendered.log, &doc, text),
            "files": files,
            "language": language,
            "scene": scene,
            "fileView": file_view,
        }))
    }

    fn measure(&mut self, r: Measure) -> Result<Value, CoreError> {
        let c = &self.client;
        let run = c.detached(&r.path, &r.run, None, None)?;
        // The old solids go before the new ones are made, so two never
        // share the memory.
        self.measurement = None;
        let (report, m) = c.measure(run)?;
        let handle = m.map(|m| {
            let id = self.next_measurement;
            self.next_measurement = self.next_measurement.wrapping_add(1).max(1);
            self.measurement = Some((id, m));
            id
        });
        let mut v = to_json(&report);
        v["measurement"] = json!(handle);
        Ok(v)
    }

    fn measurement(&self, id: u32) -> Result<&client::Measurement, CoreError> {
        match &self.measurement {
            Some((i, m)) if *i == id => Ok(m),
            _ => Err(invalid(format!(
                "measurement {id} is gone (only the latest is kept): measure again"
            ))),
        }
    }

    fn export(&self, r: Export, out: &mut Vec<Vec<u8>>) -> Result<Value, CoreError> {
        let c = &self.client;
        let format = client::export_format(Some(&r.format), "")?;
        let doc = c.doc_path(&r.path)?;
        let ext = match format.id() {
            "binstl" => "stl",
            id => id,
        };
        let output = doc.with_extension(ext).to_string_lossy().into_owned();
        let run = c.detached(&r.path, &r.run, None, None)?;
        let date = r
            .creation_date
            .unwrap_or_else(|| "1970-01-01T00:00:00Z".into());
        let mut sink = Keep { data: Vec::new() };
        let mut result = c.export(run, &output, format, &r.options, date, &mut sink)?;
        result.bytes = sink.data.len() as u64;
        let mut v = to_json(&result);
        v["mime"] = json!(mime(format));
        v["data"] = if result.exit_code == 0 {
            let at = out.len();
            out.push(sink.data);
            json!({ "$buffer": at })
        } else {
            Value::Null
        };
        Ok(v)
    }

    fn add_files(&self, r: AddFiles, buffers: Vec<Vec<u8>>) -> Result<Value, CoreError> {
        let buffer = |d: &Data| -> Result<Vec<u8>, CoreError> {
            match d {
                Data::Text(s) => Ok(s.clone().into_bytes()),
                Data::Buffer { index } => buffers
                    .get(*index)
                    .cloned()
                    .ok_or_else(|| invalid(format!("addFiles: no buffer {index}"))),
            }
        };
        let mut added = 0u32;
        for f in &r.files {
            let path = self.client.doc_path(&f.path)?;
            self.files.insert(path, buffer(&f.data)?);
            added += 1;
        }
        if let Some(t) = &r.tar {
            let root = self
                .client
                .doc_path(r.root.as_deref().unwrap_or(LIBRARY_DIR))?;
            let data = buffer(t)?;
            let entries = tar::files(&data).map_err(|message| CoreError::Failed { message })?;
            for (name, body) in entries {
                self.files.insert(root.join(name), body.to_vec());
                added += 1;
            }
        }
        Ok(json!({ "added": added }))
    }
}

#[cfg(test)]
mod tests;
