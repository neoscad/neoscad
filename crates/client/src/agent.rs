//! The desktop apps' side of the agent bridge (docs/agent-bridge.md,
//! "Desktop apps"): the messages a running app and `neoscad mcp` exchange,
//! and how an app answers each request from its open documents.
//!
//! The protocol is the web page's (`web/src/agent/page.js`), so the
//! command line's editor and view tools serve both with one set of
//! position conversions and texts: the bridge (here, `neoscad mcp`) sends
//! `welcome` and requests `{id, method, params}`; the app sends `hello`
//! and answers `{id, result}` or `{id, error: {message}}`. What the
//! desktop adds: a `document` parameter naming one of the app's open
//! documents (the most recently focused one when it is left out, the one
//! the user is looking at), the `documents` note listing them whenever
//! they change, the bridge's `activity` note for its own tool calls on the
//! app's document, and `bye`, with which the app ends a connection the
//! user disconnected.
//!
//! Positions are the editor's (0-based lines, UTF-16 columns) in both
//! directions: the command line converts them to and from the agent's
//! byte columns through `lang::source`, so nothing here counts bytes and
//! the app hands them to the editor as they come.
//!
//! Pure, like the rest of this crate (`CLAUDE.md`, "Rules"): no sockets,
//! threads or clock. `crates/agent-link` carries the messages over a
//! per-user socket and calls [`handle_request`] on its own threads; the
//! app does what each request needs through [`AgentHost`], and passes in
//! the time a document was focused.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{ConsoleKind, ConsoleLine, ParameterOverride, ParameterValue, RenderMode};
use crate::{ViewLine, ViewMarker};

/// The protocol's version, in `welcome` and `hello`. A side that sees
/// another says which `neoscad` is too old instead of failing on a
/// request it does not know.
pub const AGENT_PROTOCOL: u32 = 1;

/// A capture's longest side: the default, and the range allowed. The
/// command line checks the same range; checking again here bounds what
/// any same-user process that connects can make the app draw.
pub const CAPTURE_DEFAULT: u32 = 768;
pub const CAPTURE_MIN: u32 = 64;
pub const CAPTURE_MAX: u32 = 2048;
/// An agent's marks: at most this many markers and lines, and points in
/// all, so a runaway loop in an agent's arguments cannot stall the view.
pub const MAX_MARKS: usize = 500;
pub const MAX_POINTS: usize = 20_000;
/// A marker's label is cut to this many characters.
const MAX_LABEL: usize = 40;
/// The most edits one request may carry, and the longest summary.
const MAX_EDITS: usize = 10_000;
const MAX_SUMMARY: usize = 200;
/// The colour of an agent's marks unless it names one: the icon's pink,
/// apart from the magenta and violet the panels' measurements use (as on
/// the web page).
const MARK_COLOR: [f32; 4] = [1.0, 90.0 / 255.0, 138.0 / 255.0, 1.0];

/// The answer to every request while the user has not allowed agents (or
/// has turned them off since).
pub const NOT_ALLOWED: &str = "the user has not allowed AI agents in NeoSCAD (they can turn it on in NeoSCAD's agent settings)";

// --- Records the app fills in -------------------------------------------------

/// A place in the editor: 0-based line, UTF-16 column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorPosition {
    pub line: u32,
    pub character: u32,
}

/// The editor's selection; `anchor == head` is a cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorSelection {
    pub anchor: EditorPosition,
    pub head: EditorPosition,
}

/// One replacement, as the editor's `agentEdit` takes it: `from` to `to`
/// (exclusive) in the text the agent read becomes `insert`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTextEdit {
    pub from: EditorPosition,
    pub to: EditorPosition,
    pub insert: String,
}

/// An agent's edit for the app to apply ([`AgentHost::edit`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentEditRequest {
    /// The document's version the edits were made against
    /// ([`AgentDocumentState::version`]). The app applies them only if the
    /// document is still at it.
    pub version: u64,
    /// Sorted, not overlapping, in the positions of that version: apply
    /// them as one transaction (the editor's `agentEdit`).
    pub edits: Vec<AgentTextEdit>,
    /// What changes, for an "Apply?" prompt: "line 12-14".
    pub summary: String,
    /// The agent's self-reported name ("Claude Code"), for that prompt. A
    /// label, not an identity.
    pub client: Option<String>,
}

/// What became of an edit ([`AgentHost::edit`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum AgentEditOutcome {
    /// Applied as one undoable step; the document is now at `version`.
    Applied { version: u64 },
    /// Not applied: the document is at `version`, not the request's (the
    /// user typed since the agent read it).
    Stale { version: u64 },
    /// Not applied: the user rejected it (the app asks first when the user
    /// chose to be asked).
    Declined,
}

/// The document's last run, as its console's summary line shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRunStatus {
    pub mode: Option<RenderMode>,
    /// The summary line ("Rendered in 0.12 s: 1 volume"); empty before any
    /// run.
    pub summary: String,
    /// A run is in progress (a preview after an edit, say).
    pub running: bool,
}

/// What the app knows about a document right now ([`AgentHost::read`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDocumentState {
    /// Counts every change of the text, the user's and the agent's, as the
    /// web page's `revision` does: an edit is accepted only on the version
    /// the agent read. It must never repeat a value for different text
    /// while the document is open.
    pub version: u64,
    /// The editor's text, unsaved changes included.
    pub text: String,
    /// The selection, when the editor shows this document.
    pub selection: Option<EditorSelection>,
    /// The customizer's edited values (`DocumentController::overrides`).
    pub overrides: Vec<ParameterOverride>,
    /// The `part()` switch.
    pub parts: bool,
    pub run: AgentRunStatus,
    /// The console of the last run, as the console panel shows it.
    pub console: Vec<ConsoleLine>,
}

/// The camera, as OpenSCAD's `$vpt`, `$vpr`, `$vpd` and `$vpf`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCamera {
    pub vpt: Vec<f64>,
    pub vpr: Vec<f64>,
    pub vpd: f64,
    pub vpf: f64,
}

/// A change of camera ([`AgentHost::camera`]), applied in this order: the
/// standard `view`, View All (`fit`), then each of `vpt`, `vpr` and `vpd`
/// given.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCameraChange {
    /// One of [`VIEWS`].
    pub view: Option<String>,
    pub fit: bool,
    pub vpt: Option<Vec<f64>>,
    pub vpr: Option<Vec<f64>>,
    pub vpd: Option<f64>,
}

/// The standard views a camera change may name, as OpenSCAD's View menu.
pub const VIEWS: &[&str] = &[
    "top", "bottom", "left", "right", "front", "back", "diagonal",
];

/// The view as the user sees it ([`AgentHost::capture`]).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCapture {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub camera: AgentCamera,
    /// What drew it, for the agent's line ("Metal").
    pub backend: String,
}

/// What an app does for the agent, one call per request, each naming the
/// document by the id the app registered it under ([`AgentDocuments`]).
///
/// Called on the link's threads, never the app's UI thread, with nothing
/// locked: an implementation hops to its UI thread for the editor and the
/// view and may block there (an edit can wait for the user's Apply). An
/// `Err` is a sentence for the agent ("the document was closed").
pub trait AgentHost: Send + Sync {
    /// The document's text, version, selection and last run.
    fn read(&self, document: u64) -> Result<AgentDocumentState, String>;

    /// Apply `edit` as one undoable, highlighted step (the editor's
    /// `agentEdit`), if the document is still at `edit.version`: compare
    /// on the UI thread, where the user's typing is counted, not before.
    /// When the user asked to approve each edit, ask first.
    fn edit(&self, document: u64, edit: AgentEditRequest) -> Result<AgentEditOutcome, String>;

    /// Select and scroll to `from`..`to` in the editor (`revealRange`).
    fn reveal(&self, document: u64, from: EditorPosition, to: EditorPosition)
    -> Result<(), String>;

    /// Change the 3D view's camera as `change` says and give the camera
    /// after it (`Viewport::apply_agent_camera` in the app's core).
    fn camera(&self, document: u64, change: AgentCameraChange) -> Result<AgentCamera, String>;

    /// The 3D view as the user sees it, its longest side at most
    /// `max_side` pixels, after any preview that is pending
    /// (`Viewport::capture_as_shown`).
    fn capture(&self, document: u64, max_side: u32) -> Result<AgentCapture, String>;

    /// Show the agent's marks in the 3D view, replacing its earlier ones
    /// (empty lists clear them). A layer of the agent's own, apart from the
    /// panels' (`Viewport::set_agent_annotations`).
    fn annotate(
        &self,
        document: u64,
        lines: Vec<ViewLine>,
        markers: Vec<ViewMarker>,
    ) -> Result<(), String>;
}

// --- The open documents -----------------------------------------------------

/// An open document as the agent sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDocument {
    /// The app's own id for it, stable while it is open.
    pub id: u64,
    /// Its name as the window shows it ("gears.scad", "Untitled").
    pub file: String,
    /// Its file, when it has been saved: the command line runs the model
    /// tools under this path, so its includes resolve beside it.
    pub path: Option<String>,
    /// When the user last focused it, in milliseconds on the wall clock
    /// (comparable across app processes: the Windows app is one per
    /// window). 0 for never.
    pub focused_ms: u64,
}

/// The app's open documents, most recently focused first: the first is the
/// one a request without `document` acts on.
///
/// Why the last-focused window rather than asking the agent to choose:
/// it is the one the user is looking at when they type "make the teeth
/// smaller" into their agent, and on macOS, where windows share one
/// process, the frontmost. The agent still sees the list (`editor_read`
/// names the others) and can pass `document` to work on another.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AgentDocuments {
    docs: Vec<AgentDocument>,
}

impl AgentDocuments {
    /// Register a document, or change its name or path (after Save As).
    /// Whether anything changed.
    pub fn open(&mut self, id: u64, file: &str, path: Option<&str>) -> bool {
        let path = path.map(str::to_string);
        if let Some(d) = self.docs.iter_mut().find(|d| d.id == id) {
            if d.file == file && d.path == path {
                return false;
            }
            d.file = file.to_string();
            d.path = path;
            return true;
        }
        self.docs.push(AgentDocument {
            id,
            file: file.to_string(),
            path,
            focused_ms: 0,
        });
        self.sort();
        true
    }

    /// The user focused this document's window at `now_ms` (wall clock).
    /// Whether anything changed. The time matters even when the document
    /// was already first here: the bridge compares it with other app
    /// processes' documents.
    pub fn focus(&mut self, id: u64, now_ms: u64) -> bool {
        // Never behind another document: two focus calls in the same
        // millisecond, or a wall clock that stepped back, still put the
        // later one first.
        let others = self
            .docs
            .iter()
            .filter(|d| d.id != id)
            .map(|d| d.focused_ms)
            .max()
            .unwrap_or(0);
        let Some(d) = self.docs.iter_mut().find(|d| d.id == id) else {
            return false;
        };
        let t = now_ms.max(others.saturating_add(1));
        let changed = d.focused_ms != t;
        d.focused_ms = t;
        self.sort();
        changed
    }

    pub fn close(&mut self, id: u64) -> bool {
        let before = self.docs.len();
        self.docs.retain(|d| d.id != id);
        self.docs.len() != before
    }

    /// Most recently focused first (then the most recently opened).
    pub fn list(&self) -> &[AgentDocument] {
        &self.docs
    }

    fn sort(&mut self) {
        self.docs
            .sort_by(|a, b| b.focused_ms.cmp(&a.focused_ms).then(b.id.cmp(&a.id)));
    }

    /// The document a request acts on: the one it names, else the most
    /// recently focused.
    pub fn target(&self, requested: Option<u64>) -> Result<&AgentDocument, String> {
        match requested {
            Some(id) => self.docs.iter().find(|d| d.id == id).ok_or_else(|| {
                format!("document {id} is not open in NeoSCAD (it may have been closed)")
            }),
            None => self
                .docs
                .first()
                .ok_or_else(|| "NeoSCAD has no document open".to_string()),
        }
    }
}

fn documents_json(docs: &[AgentDocument]) -> Value {
    Value::Array(
        docs.iter()
            .map(|d| json!({"id": d.id, "file": d.file, "path": d.path, "focused": d.focused_ms}))
            .collect(),
    )
}

// --- What the user sees ------------------------------------------------------

/// A connected agent, for the app's status control.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentClient {
    /// The link's number for this connection ([`AgentStatus`]), for
    /// "Disconnect".
    pub id: u64,
    /// The MCP client's self-reported name ("Claude Code"); not verified,
    /// so the UI must not present it as an identity.
    pub name: Option<String>,
    /// What it is doing now ("is editing"), if anything.
    pub activity: Option<String>,
    /// The document it is doing it to.
    pub document: Option<u64>,
}

/// The link's state, given to the app after every change.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatus {
    /// The user has allowed agents (the app passes this in).
    pub allowed: bool,
    /// The link is listening: agents can connect.
    pub listening: bool,
    /// Where (a socket path or pipe name), for the app's log.
    pub address: Option<String>,
    pub clients: Vec<AgentClient>,
    /// Why the link could not start or stopped, for the app's log.
    pub error: Option<String>,
}

/// What an agent is doing, for the status control's tooltip: by request
/// (the web page's `DOING`, `web/src/ui/agent.js`) and by the command
/// line's own tool calls on the app's document.
pub fn activity_text(what: &str) -> &'static str {
    match what {
        "read" => "is reading the code",
        "edit" => "is editing",
        "reveal" => "is showing you the code",
        "camera" => "is moving the camera",
        "capture" => "is looking at the view",
        "annotate" => "is pointing at the model",
        "console" => "is reading the console",
        "evaluate" => "is running the model",
        "render" => "is rendering the model",
        "snapshot" => "is taking pictures of the model",
        "check" => "is checking the model",
        "measure" => "is measuring the model",
        "format" => "is formatting the code",
        "test" => "is running the model's tests",
        _ => "is working",
    }
}

/// The status control's line: "Connect your AI agent" when none is
/// connected, "Claude Code connected", "Claude Code is editing", or "2
/// agents connected".
pub fn status_line(status: &AgentStatus) -> String {
    let name = |c: &AgentClient| c.name.clone().unwrap_or_else(|| "An agent".to_string());
    match status.clients.as_slice() {
        [] => "Connect your AI agent".to_string(),
        [c] => match &c.activity {
            Some(a) => format!("{} {a}", name(c)),
            None => format!("{} connected", name(c)),
        },
        cs => match cs.iter().find(|c| c.activity.is_some()) {
            Some(c) => format!("{} {}", name(c), c.activity.as_deref().unwrap_or_default()),
            None => format!("{} agents connected", cs.len()),
        },
    }
}

// --- Messages -----------------------------------------------------------------

/// A message from the bridge.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// Who is connecting (the MCP client's name, once its `initialize`
    /// has said). Sent first, and again when the name becomes known.
    Welcome {
        client: Option<String>,
        server: Option<String>,
        protocol: u32,
    },
    /// The bridge is running one of its own tools on a document (`tool`),
    /// or has finished (`None`).
    Activity {
        tool: Option<String>,
        document: Option<u64>,
    },
    Request {
        id: u64,
        method: String,
        params: Value,
    },
    /// Anything else, ignored (a newer bridge's note).
    Other,
}

/// Read a message from the bridge.
pub fn incoming(msg: &Value) -> Incoming {
    if let Some(id) = msg.get("id").and_then(Value::as_u64) {
        return Incoming::Request {
            id,
            method: msg["method"].as_str().unwrap_or_default().to_string(),
            params: msg.get("params").cloned().unwrap_or(Value::Null),
        };
    }
    let text = |k: &str| msg.get(k).and_then(Value::as_str).map(str::to_string);
    match msg["type"].as_str() {
        Some("welcome") => Incoming::Welcome {
            client: text("client"),
            server: text("server"),
            protocol: msg["protocol"]
                .as_u64()
                .and_then(|p| u32::try_from(p).ok())
                .unwrap_or(0),
        },
        Some("activity") => Incoming::Activity {
            tool: text("tool"),
            document: msg["document"].as_u64(),
        },
        _ => Incoming::Other,
    }
}

/// The app's first message: what it is, and its open documents.
pub fn hello(app: &str, version: &str, platform: &str, docs: &[AgentDocument]) -> Value {
    json!({
        "type": "hello",
        "app": app,
        "version": version,
        "platform": platform,
        "protocol": AGENT_PROTOCOL,
        "documents": documents_json(docs),
    })
}

/// The open documents changed (opened, closed, renamed, focused).
pub fn documents_note(docs: &[AgentDocument]) -> Value {
    json!({"type": "documents", "documents": documents_json(docs)})
}

/// The app is ending the connection: the user disconnected this agent, or
/// turned agents off. `reconnect: false` asks the bridge not to connect to
/// this app again by itself.
pub fn bye(reason: &str, reconnect: bool) -> Value {
    json!({"type": "bye", "reason": reason, "reconnect": reconnect})
}

/// The answer to request `id`.
pub fn reply(id: u64, result: Result<Value, String>) -> Value {
    match result {
        Ok(r) => json!({"id": id, "result": r}),
        Err(message) => json!({"id": id, "error": {"message": message}}),
    }
}

// --- Requests -------------------------------------------------------------------

/// Answer the bridge's request `method` with `params` from the app's open
/// `documents` through `host`. `allowed` is whether the user has allowed
/// agents: nothing is read or changed without it, whatever connected. An
/// `Err` is a sentence for the agent.
pub fn handle_request(
    host: &dyn AgentHost,
    documents: &AgentDocuments,
    allowed: bool,
    client: Option<&str>,
    method: &str,
    params: &Value,
) -> Result<Value, String> {
    if !allowed {
        return Err(NOT_ALLOWED.into());
    }
    if method == "documents" {
        return Ok(json!({"documents": documents_json(documents.list())}));
    }
    let requested = match params.get("document").filter(|v| !v.is_null()) {
        None => None,
        Some(v) => Some(v.as_u64().ok_or("`document` must be a document's id")?),
    };
    let doc = documents.target(requested)?;
    let id = doc.id;
    let named = |mut v: Value| {
        v["document"] = json!(id);
        v["file"] = json!(doc.file);
        v["path"] = json!(doc.path);
        v
    };
    match method {
        "read" => {
            let s = host.read(id)?;
            Ok(named(read_json(&s, doc.path.as_deref())))
        }
        "edit" => edit(host, id, client, params).map(named),
        "reveal" => {
            let from = position(&params["from"]).ok_or("`from` must be [line, character]")?;
            let to = position(&params["to"]).ok_or("`to` must be [line, character]")?;
            host.reveal(id, from, to.max_pos(from))?;
            Ok(json!({}))
        }
        "camera" => {
            let change = camera_change(params)?;
            Ok(camera_json(&host.camera(id, change)?))
        }
        "capture" => {
            let size = match params.get("size").filter(|v| !v.is_null()) {
                None => CAPTURE_DEFAULT,
                Some(s) => match s.as_f64() {
                    Some(s) if (f64::from(CAPTURE_MIN)..=f64::from(CAPTURE_MAX)).contains(&s) => {
                        s as u32
                    }
                    _ => {
                        return Err(format!(
                            "`size` is the longest side in pixels, {CAPTURE_MIN} to {CAPTURE_MAX}"
                        ));
                    }
                },
            };
            let c = host.capture(id, size)?;
            if !c.png.starts_with(b"\x89PNG\r\n\x1a\n") {
                return Err("the app's capture is not a PNG".into());
            }
            Ok(named(json!({
                "png": base64(&c.png),
                "width": c.width,
                "height": c.height,
                "backend": c.backend,
                "camera": camera_json(&c.camera),
            })))
        }
        "annotate" => {
            let (lines, markers) = marks(params)?;
            host.annotate(id, lines, markers)?;
            Ok(json!({}))
        }
        "console" => {
            let s = host.read(id)?;
            let mut out = run_json(&s.run);
            out["lines"] = Value::Array(
                s.console
                    .iter()
                    .map(|l| console_json(l, doc.path.as_deref()))
                    .collect(),
            );
            Ok(named(out))
        }
        _ => Err(format!(
            "NeoSCAD does not know \"{method}\" (it may be older than your neoscad: update the app)"
        )),
    }
}

trait MaxPos {
    fn max_pos(self, other: Self) -> Self;
}

impl MaxPos for EditorPosition {
    /// A range never ends before it starts.
    fn max_pos(self, other: EditorPosition) -> EditorPosition {
        if (self.line, self.character) < (other.line, other.character) {
            other
        } else {
            self
        }
    }
}

/// `[line, character]` as an editor position.
fn position(v: &Value) -> Option<EditorPosition> {
    let line = u32::try_from(v.get(0)?.as_u64()?).ok()?;
    let character = u32::try_from(v.get(1)?.as_u64()?).ok()?;
    (v.as_array()?.len() == 2).then_some(EditorPosition { line, character })
}

fn position_json(p: EditorPosition) -> Value {
    json!([p.line, p.character])
}

fn kind_name(k: ConsoleKind) -> &'static str {
    match k {
        ConsoleKind::Error => "error",
        ConsoleKind::Warning => "warning",
        ConsoleKind::Deprecated => "deprecated",
        ConsoleKind::Echo => "echo",
        ConsoleKind::Trace => "trace",
        ConsoleKind::Info => "info",
    }
}

/// A console line as the agent reads it (the web page's `agentLine`):
/// 1-based line, and the file only when it is not the document.
fn console_json(l: &ConsoleLine, doc: Option<&str>) -> Value {
    let mut out = json!({"kind": kind_name(l.kind), "text": l.text});
    if let Some(loc) = &l.location {
        out["line"] = json!(loc.start_line + 1);
        if doc != Some(loc.path.as_str()) {
            let name = loc.path.rsplit(['/', '\\']).next().unwrap_or(&loc.path);
            out["file"] = json!(name);
        }
    }
    out
}

fn run_json(r: &AgentRunStatus) -> Value {
    let mode = r.mode.map(|m| match m {
        RenderMode::Render => "render",
        RenderMode::Force => "render",
        RenderMode::Preview => "preview",
    });
    json!({
        "mode": mode,
        "summary": r.summary,
        "state": if r.running { "running" } else { "idle" },
    })
}

fn value_json(v: &ParameterValue) -> Value {
    match v {
        ParameterValue::Bool { value } => json!(value),
        ParameterValue::Number { value } => json!(value),
        ParameterValue::Text { value } => json!(value),
        ParameterValue::Vector { value } => json!(value),
    }
}

/// The `read` answer, in the web page's shape (`page.js`, `read`).
fn read_json(s: &AgentDocumentState, path: Option<&str>) -> Value {
    let values: serde_json::Map<String, Value> = s
        .overrides
        .iter()
        .map(|o| (o.name.clone(), value_json(&o.value)))
        .collect();
    json!({
        "version": s.version,
        "text": s.text,
        "selection": s.selection.map(|sel| json!({
            "anchor": position_json(sel.anchor),
            "head": position_json(sel.head),
        })),
        "values": values,
        "parts": s.parts,
        "run": run_json(&s.run),
        "diagnostics": s.console.iter()
            .filter(|l| matches!(l.kind, ConsoleKind::Error | ConsoleKind::Warning))
            .map(|l| console_json(l, path))
            .collect::<Vec<_>>(),
    })
}

fn edit(
    host: &dyn AgentHost,
    id: u64,
    client: Option<&str>,
    params: &Value,
) -> Result<Value, String> {
    let version = params["version"]
        .as_u64()
        .ok_or("`version` must be the document's version (editor_read gives it)")?;
    let list = params["edits"]
        .as_array()
        .ok_or("`edits` must be an array")?;
    if list.is_empty() {
        return Err("no edits".into());
    }
    if list.len() > MAX_EDITS {
        return Err(format!("at most {MAX_EDITS} edits at a time"));
    }
    let mut edits = Vec::with_capacity(list.len());
    for (i, e) in list.iter().enumerate() {
        let bad = || {
            format!(
                "edit {} must be {{from: [line, character], to, insert}}",
                i + 1
            )
        };
        let from = position(&e["from"]).ok_or_else(bad)?;
        let to = position(&e["to"]).ok_or_else(bad)?;
        let insert = e["insert"].as_str().ok_or_else(bad)?.to_string();
        if (to.line, to.character) < (from.line, from.character) {
            return Err(format!("edit {} ends before it starts", i + 1));
        }
        edits.push(AgentTextEdit { from, to, insert });
    }
    edits.sort_by_key(|e| (e.from.line, e.from.character));
    if edits
        .windows(2)
        .any(|w| (w[0].to.line, w[0].to.character) > (w[1].from.line, w[1].from.character))
    {
        return Err("the edits overlap".into());
    }
    let summary: String = params["summary"]
        .as_str()
        .unwrap_or("")
        .chars()
        .take(MAX_SUMMARY)
        .collect();
    let request = AgentEditRequest {
        version,
        edits,
        summary,
        client: client.map(str::to_string),
    };
    match host.edit(id, request)? {
        AgentEditOutcome::Applied { version } => Ok(json!({"version": version})),
        AgentEditOutcome::Stale { version: now } => Err(format!(
            "the document's text changed (it is at version {now}, not {version}): editor_read again"
        )),
        AgentEditOutcome::Declined => Err(
            "the user declined this edit (the text is unchanged); ask them what they want instead"
                .into(),
        ),
    }
}

fn vec3(v: &Value) -> Option<Vec<f64>> {
    let a: Option<Vec<f64>> = v.as_array()?.iter().map(Value::as_f64).collect();
    a.filter(|a| a.len() == 3 && a.iter().all(|x| x.is_finite()))
}

fn camera_change(params: &Value) -> Result<AgentCameraChange, String> {
    let three = |k: &str| -> Result<Option<Vec<f64>>, String> {
        match params.get(k).filter(|v| !v.is_null()) {
            None => Ok(None),
            Some(v) => vec3(v)
                .map(Some)
                .ok_or_else(|| format!("`{k}` must be [x, y, z]")),
        }
    };
    let vpd = match params.get("vpd").filter(|v| !v.is_null()) {
        None => None,
        Some(d) => match d.as_f64() {
            Some(d) if d.is_finite() && d > 0.0 => Some(d),
            _ => return Err("`vpd` must be a distance above 0".into()),
        },
    };
    let view = match params["view"].as_str() {
        None => None,
        Some(v) => {
            let v = v.to_ascii_lowercase();
            let v = if v == "iso" {
                "diagonal".to_string()
            } else {
                v
            };
            if !VIEWS.contains(&v.as_str()) {
                return Err(format!("`view` is one of {}", VIEWS.join(", ")));
            }
            Some(v)
        }
    };
    Ok(AgentCameraChange {
        view,
        fit: params["fit"].as_bool().unwrap_or(false),
        vpt: three("vpt")?,
        vpr: three("vpr")?,
        vpd,
    })
}

fn camera_json(c: &AgentCamera) -> Value {
    json!({"vpt": c.vpt, "vpr": c.vpr, "vpd": c.vpd, "vpf": c.vpf})
}

/// `#rgb` or `#rrggbb` as RGBA (0 to 1); the agents' pink when absent.
fn color(v: &Value) -> Result<Vec<f32>, String> {
    let Some(c) = v.as_str() else {
        return if v.is_null() {
            Ok(MARK_COLOR.to_vec())
        } else {
            Err(format!("color {v} must be #rrggbb"))
        };
    };
    let hex = c.strip_prefix('#').unwrap_or("");
    let digits: Option<Vec<u8>> = hex
        .chars()
        .map(|ch| ch.to_digit(16).and_then(|d| u8::try_from(d).ok()))
        .collect();
    let channel = |hi: u8, lo: u8| f32::from(hi * 16 + lo) / 255.0;
    match digits.as_deref() {
        Some(&[r, g, b]) => Ok(vec![channel(r, r), channel(g, g), channel(b, b), 1.0]),
        Some(&[r1, r2, g1, g2, b1, b2]) => {
            Ok(vec![channel(r1, r2), channel(g1, g2), channel(b1, b2), 1.0])
        }
        _ => Err(format!("color {v} must be #rrggbb")),
    }
}

/// The agent's marks as the view draws them, within the bounds.
fn marks(params: &Value) -> Result<(Vec<ViewLine>, Vec<ViewMarker>), String> {
    let empty = Vec::new();
    let markers = params["markers"].as_array().unwrap_or(&empty);
    let lines = params["lines"].as_array().unwrap_or(&empty);
    if markers.len() + lines.len() > MAX_MARKS {
        return Err(format!("at most {MAX_MARKS} markers and lines at a time"));
    }
    let mut out_markers = Vec::with_capacity(markers.len());
    for (i, m) in markers.iter().enumerate() {
        let point =
            vec3(&m["point"]).ok_or(format!("marker {}: `point` must be [x, y, z]", i + 1))?;
        let label: String = m["label"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(MAX_LABEL)
            .collect();
        out_markers.push(ViewMarker {
            point,
            label,
            color: color(&m["color"])?,
        });
    }
    let mut out_lines = Vec::with_capacity(lines.len());
    let mut points = 0;
    for (i, l) in lines.iter().enumerate() {
        let p: Option<Vec<Vec<f64>>> = l["points"]
            .as_array()
            .map(|a| a.iter().map(vec3).collect())
            .unwrap_or(None);
        let p = p.filter(|p| p.len() >= 2).ok_or(format!(
            "line {}: `points` must be two or more [x, y, z]",
            i + 1
        ))?;
        points += p.len();
        if points > MAX_POINTS {
            return Err(format!("at most {MAX_POINTS} points in all"));
        }
        out_lines.push(ViewLine {
            points: p.concat(),
            closed: l["closed"].as_bool().unwrap_or(false),
            color: color(&l["color"])?,
        });
    }
    Ok((out_lines, out_markers))
}

/// Standard base64 (RFC 4648) with padding: the capture's PNG in JSON.
pub fn base64(data: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(char::from(ABC[(n >> (18 - 6 * i)) as usize & 63]));
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;
