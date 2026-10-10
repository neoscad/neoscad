//! The agent link for the macOS and Windows apps (docs/agent-bridge.md,
//! "Desktop apps"): [`AgentLink`], which listens for `neoscad mcp` once the
//! user has allowed agents, and the two interfaces the app implements:
//! [`AgentHost`], which does what each request needs on the app's
//! documents, and [`AgentObserver`], told who is connected and what they
//! are doing. The protocol and the request handling are
//! `client::agent`; the listener is `agent_link` (the Linux app uses both
//! directly).
//!
//! The view's part is here too, on [`Viewport`]: a capture as the user
//! sees it (`capture_as_shown`), a camera change (`apply_agent_camera`)
//! and the agent's own layer of marks (`set_agent_annotations`), so each
//! of the host's view callbacks is one call.
//!
//! # Threads
//!
//! The link calls [`AgentHost`] on its own threads, never the main thread,
//! one call per request and with nothing locked: the host hops to its main
//! thread (the editor and the view live there) and may block on it,
//! including while it asks the user to approve an edit. [`AgentObserver`]
//! is called on the link's threads too; it must hand the status to the
//! main thread without waiting for it (`DispatchQueue.main.async`,
//! `DispatcherQueue.TryEnqueue`), since the main thread may be calling
//! into the link at that moment.

use std::sync::{Arc, PoisonError};

use client::agent as protocol;
pub use client::agent::{
    AgentCamera, AgentCameraChange, AgentCapture, AgentClient, AgentDocumentState,
    AgentEditOutcome, AgentEditRequest, AgentRunStatus, AgentStatus, AgentTextEdit, EditorPosition,
    EditorSelection,
};

use crate::{
    ConsoleLine, CoreError, ParameterOverride, RenderMode, ViewLine, ViewMarker, Viewport, guarded,
};

/// A place in the editor: 0-based line, UTF-16 column (CodeMirror's).
#[uniffi::remote(Record)]
pub struct EditorPosition {
    pub line: u32,
    pub character: u32,
}

/// The editor's selection; `anchor == head` is a cursor.
#[uniffi::remote(Record)]
pub struct EditorSelection {
    pub anchor: EditorPosition,
    pub head: EditorPosition,
}

/// One replacement for the editor's `agentEdit`: `from` to `to`
/// (exclusive) becomes `insert`.
#[uniffi::remote(Record)]
pub struct AgentTextEdit {
    pub from: EditorPosition,
    pub to: EditorPosition,
    pub insert: String,
}

/// An agent's edit: apply `edits` as one undoable, highlighted step
/// (`agentEdit`) if the document is still at `version`.
#[uniffi::remote(Record)]
pub struct AgentEditRequest {
    pub version: u64,
    pub edits: Vec<AgentTextEdit>,
    /// What changes ("line 12-14"), for an approval prompt.
    pub summary: String,
    /// The agent's self-reported name, for that prompt (not verified).
    pub client: Option<String>,
}

/// What became of an edit.
#[uniffi::remote(Enum)]
pub enum AgentEditOutcome {
    Applied { version: u64 },
    Stale { version: u64 },
    Declined,
}

/// The document's last run, as the console's summary shows it.
#[uniffi::remote(Record)]
pub struct AgentRunStatus {
    pub mode: Option<RenderMode>,
    pub summary: String,
    pub running: bool,
}

/// A document as the agent reads it.
#[uniffi::remote(Record)]
pub struct AgentDocumentState {
    /// Counts every change of the text, the user's and the agent's (as
    /// the web page's `revision`); never repeats for different text while
    /// the document is open.
    pub version: u64,
    pub text: String,
    pub selection: Option<EditorSelection>,
    pub overrides: Vec<ParameterOverride>,
    pub parts: bool,
    /// The `--enable` names the document runs with (the app's language
    /// settings), added to the agent server's own for its text.
    pub enable: Vec<String>,
    pub run: AgentRunStatus,
    pub console: Vec<ConsoleLine>,
}

/// The camera as `$vpt`, `$vpr`, `$vpd`, `$vpf`.
#[uniffi::remote(Record)]
pub struct AgentCamera {
    pub vpt: Vec<f64>,
    pub vpr: Vec<f64>,
    pub vpd: f64,
    pub vpf: f64,
}

/// A camera change: `view` (top, bottom, left, right, front, back,
/// diagonal), then View All (`fit`), then each of `vpt`, `vpr`, `vpd`.
#[uniffi::remote(Record)]
pub struct AgentCameraChange {
    pub view: Option<String>,
    pub fit: bool,
    pub vpt: Option<Vec<f64>>,
    pub vpr: Option<Vec<f64>>,
    pub vpd: Option<f64>,
}

/// The view as the user sees it, as a PNG.
#[uniffi::remote(Record)]
pub struct AgentCapture {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub camera: AgentCamera,
    pub backend: String,
}

/// A connected agent, for the status control.
#[uniffi::remote(Record)]
pub struct AgentClient {
    /// For [`AgentLink::disconnect`].
    pub id: u64,
    /// Self-reported ("Claude Code"): a label, not an identity.
    pub name: Option<String>,
    /// "is editing", while it does something.
    pub activity: Option<String>,
    pub document: Option<u64>,
}

/// The link's state.
#[uniffi::remote(Record)]
pub struct AgentStatus {
    pub allowed: bool,
    pub listening: bool,
    pub address: Option<String>,
    pub clients: Vec<AgentClient>,
    pub error: Option<String>,
}

/// Why the app could not do a request: a sentence the agent reads ("the
/// document was closed").
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Error)]
pub enum AgentHostError {
    Refused { message: String },
}

impl std::fmt::Display for AgentHostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentHostError::Refused { message } => f.write_str(message),
        }
    }
}

impl std::error::Error for AgentHostError {}

impl From<uniffi::UnexpectedUniFFICallbackError> for AgentHostError {
    fn from(e: uniffi::UnexpectedUniFFICallbackError) -> Self {
        AgentHostError::Refused {
            message: format!("NeoSCAD failed to answer: {}", e.reason),
        }
    }
}

/// What the app does for the agent, each call naming a document by the id
/// the app gave [`AgentLink::document_opened`]. Called on the link's
/// threads (see the module documentation).
#[uniffi::export(with_foreign)]
pub trait AgentHost: Send + Sync {
    /// The document's text, version, selection, customizer values and last
    /// run (the editor's text, unsaved changes included).
    fn read(&self, document: u64) -> Result<AgentDocumentState, AgentHostError>;

    /// On the main thread: if the document is at `edit.version`, apply the
    /// edits with the editor's `agentEdit` (asking first when the user
    /// chose "Ask before applying") and give the new version; else
    /// `Stale` with the current one; `Declined` when the user said no.
    fn edit(
        &self,
        document: u64,
        edit: AgentEditRequest,
    ) -> Result<AgentEditOutcome, AgentHostError>;

    /// Select and scroll to the range (the editor's `revealRange`).
    fn reveal(
        &self,
        document: u64,
        from: EditorPosition,
        to: EditorPosition,
    ) -> Result<(), AgentHostError>;

    /// `viewport.apply_agent_camera(change)` on the document's view.
    fn camera(
        &self,
        document: u64,
        change: AgentCameraChange,
    ) -> Result<AgentCamera, AgentHostError>;

    /// After any pending preview, `viewport.capture_as_shown(max_side)`.
    fn capture(&self, document: u64, max_side: u32) -> Result<AgentCapture, AgentHostError>;

    /// `viewport.set_agent_annotations(lines, markers)`.
    fn annotate(
        &self,
        document: u64,
        lines: Vec<ViewLine>,
        markers: Vec<ViewMarker>,
    ) -> Result<(), AgentHostError>;
}

/// Told the link's state after every change. Keep the status with the
/// highest `sequence`: statuses from different threads can arrive out of
/// order.
#[uniffi::export(with_foreign)]
pub trait AgentObserver: Send + Sync {
    fn status_changed(&self, status: AgentStatus, sequence: u64);
}

/// The foreign host as the protocol's.
struct Host(Arc<dyn AgentHost>);

fn message(e: AgentHostError) -> String {
    e.to_string()
}

impl protocol::AgentHost for Host {
    fn read(&self, document: u64) -> Result<AgentDocumentState, String> {
        self.0.read(document).map_err(message)
    }
    fn edit(&self, document: u64, edit: AgentEditRequest) -> Result<AgentEditOutcome, String> {
        self.0.edit(document, edit).map_err(message)
    }
    fn reveal(
        &self,
        document: u64,
        from: EditorPosition,
        to: EditorPosition,
    ) -> Result<(), String> {
        self.0.reveal(document, from, to).map_err(message)
    }
    fn camera(&self, document: u64, change: AgentCameraChange) -> Result<AgentCamera, String> {
        self.0.camera(document, change).map_err(message)
    }
    fn capture(&self, document: u64, max_side: u32) -> Result<AgentCapture, String> {
        self.0.capture(document, max_side).map_err(message)
    }
    fn annotate(
        &self,
        document: u64,
        lines: Vec<ViewLine>,
        markers: Vec<ViewMarker>,
    ) -> Result<(), String> {
        self.0.annotate(document, lines, markers).map_err(message)
    }
}

struct Observer(Arc<dyn AgentObserver>);

impl agent_link::AgentObserver for Observer {
    fn status_changed(&self, status: AgentStatus, sequence: u64) {
        self.0.status_changed(status, sequence);
    }
}

/// The app's agent link: nothing runs and nothing listens until
/// [`AgentLink::start`], which needs the user's consent first
/// ([`AgentLink::set_allowed`]). Releasing it stops it.
#[derive(uniffi::Object)]
pub struct AgentLink {
    inner: agent_link::AgentLink,
}

impl std::fmt::Debug for AgentLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.inner.fmt(f)
    }
}

fn platform() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    }
}

impl AgentLink {
    fn make(host: Arc<dyn AgentHost>, app_version: String, dir: Option<String>) -> AgentLink {
        let config = agent_link::LinkConfig {
            app: "NeoSCAD".into(),
            version: app_version,
            platform: platform().into(),
            rendezvous: dir.map(|d| agent_link::discovery::Rendezvous::at(d.as_ref())),
        };
        AgentLink {
            inner: agent_link::AgentLink::new(Arc::new(Host(host)), config),
        }
    }
}

#[uniffi::export]
impl AgentLink {
    /// A link for the app (its version goes in the `hello`, so a
    /// mismatched `neoscad` can say which side is old).
    #[uniffi::constructor]
    pub fn new(host: Arc<dyn AgentHost>, app_version: String) -> Arc<AgentLink> {
        Arc::new(AgentLink::make(host, app_version, None))
    }

    /// A link listening in `dir` instead of this user's place (the
    /// command line then needs `NEOSCAD_AGENT_DIR` set to it): for tests.
    #[uniffi::constructor]
    pub fn with_dir(host: Arc<dyn AgentHost>, app_version: String, dir: String) -> Arc<AgentLink> {
        Arc::new(AgentLink::make(host, app_version, Some(dir)))
    }

    pub fn set_observer(&self, observer: Option<Arc<dyn AgentObserver>>) -> Result<(), CoreError> {
        guarded(|| {
            self.inner.set_observer(
                observer.map(|o| Arc::new(Observer(o)) as Arc<dyn agent_link::AgentObserver>),
            );
            Ok(())
        })
    }

    /// The user's consent, from the app's settings: until it is true,
    /// `start` fails and every request is refused. Turning it off stops
    /// the link and tells each agent why.
    pub fn set_allowed(&self, allowed: bool) -> Result<(), CoreError> {
        guarded(|| {
            self.inner.set_allowed(allowed);
            Ok(())
        })
    }

    /// Listen for agents; the address (a socket path or pipe name), for
    /// the log. Already listening is fine.
    pub fn start(&self) -> Result<String, CoreError> {
        guarded(|| {
            self.inner
                .start()
                .map_err(|message| CoreError::Failed { message })
        })
    }

    /// Stop listening and end every connection.
    pub fn stop(&self) -> Result<(), CoreError> {
        guarded(|| {
            self.inner.stop();
            Ok(())
        })
    }

    /// End one agent's connection ("Disconnect"); it does not come back by
    /// itself until its session or the app restarts.
    pub fn disconnect(&self, client: u64) -> Result<(), CoreError> {
        guarded(|| {
            self.inner.disconnect(client);
            Ok(())
        })
    }

    pub fn status(&self) -> Result<AgentStatus, CoreError> {
        guarded(|| Ok(self.inner.status()))
    }

    /// A document window opened, or its name or file changed (Save As):
    /// `file` as the title shows it, `path` once saved. Cheap; call it
    /// whether or not the link runs.
    pub fn document_opened(
        &self,
        id: u64,
        file: String,
        path: Option<String>,
    ) -> Result<(), CoreError> {
        guarded(|| {
            self.inner.document_opened(id, &file, path.as_deref());
            Ok(())
        })
    }

    /// The document's window became the key (focused) window.
    pub fn document_focused(&self, id: u64) -> Result<(), CoreError> {
        guarded(|| {
            self.inner.document_focused(id);
            Ok(())
        })
    }

    pub fn document_closed(&self, id: u64) -> Result<(), CoreError> {
        guarded(|| {
            self.inner.document_closed(id);
            Ok(())
        })
    }
}

/// The status control's text: "Connect your AI agent", "Claude Code
/// connected", "Claude Code is editing", "2 agents connected".
#[uniffi::export]
pub fn agent_status_line(status: AgentStatus) -> String {
    protocol::status_line(&status)
}

/// The width and height of a capture whose longest side is at most
/// `max_side`, in the view's proportions and never larger than the view
/// (the web page's `captureSize`); a view without a size gives a square.
fn capture_size(view: (u32, u32), max_side: u32) -> (u32, u32) {
    let max_side = max_side.clamp(protocol::CAPTURE_MIN, protocol::CAPTURE_MAX);
    let (w, h) = (f64::from(view.0.max(1)), f64::from(view.1.max(1)));
    if view.0 == 0 || view.1 == 0 {
        return (max_side, max_side);
    }
    let scale = (f64::from(max_side) / w.max(h)).min(1.0);
    // At least 16 pixels a side, as the export image requires.
    let side = |x: f64| ((x * scale).round() as u32).max(16);
    (side(w), side(h))
}

#[uniffi::export]
impl Viewport {
    /// The view as the user sees it (camera, grid, every mark), drawn
    /// offscreen with its longest side at most `max_side` pixels, as a
    /// PNG: what an agent's `view_capture` gets (`copy_as_shown`). The
    /// window's own surface is never read.
    pub fn capture_as_shown(&self, max_side: u32) -> Result<AgentCapture, CoreError> {
        guarded(|| {
            let failed = |e: render::offscreen::Error| CoreError::Failed {
                message: e.to_string(),
            };
            // Copied under the lock, drawn outside it, so the window's
            // frames do not wait for the readback.
            let (mut copy, camera) = {
                let v = self.lock();
                let (w, h) = capture_size(v.size(), max_side);
                (v.copy_as_shown(w, h).map_err(failed)?, camera(&v))
            };
            let image = copy.read_pixels_blocking().map_err(failed)?;
            Ok(AgentCapture {
                png: render::encode_png(image.width, image.height, &image.rgba),
                width: image.width,
                height: image.height,
                camera,
                backend: format!("{:?}", self.gpu.adapter_info().backend),
            })
        })
    }

    /// Change the camera as an agent asked (`view_camera`), and give it
    /// after the change.
    pub fn apply_agent_camera(&self, change: AgentCameraChange) -> Result<AgentCamera, CoreError> {
        guarded(|| {
            let three = |v: &Option<Vec<f64>>, what: &str| {
                v.as_deref().map(|p| client::point3(p, what)).transpose()
            };
            let (vpt, vpr) = (three(&change.vpt, "vpt")?, three(&change.vpr, "vpr")?);
            let view = match change.view.as_deref() {
                None => None,
                Some(name) => Some(view_named(name).ok_or_else(|| CoreError::InvalidArgument {
                    message: format!("unknown view '{name}'"),
                })?),
            };
            let mut v = self.lock();
            if let Some(view) = view {
                v.set_view(view);
            }
            if change.fit {
                v.view_all();
            }
            if vpt.is_some() || vpr.is_some() || change.vpd.is_some() {
                v.set_file_view(vpt, vpr, change.vpd.filter(|d| *d > 0.0), None);
            }
            Ok(camera(&v))
        })
    }

    /// Draw an agent's marks over the model, replacing its earlier ones
    /// (empty lists clear them). A layer of its own: the check and measure
    /// panels' marks (`set_annotations`) stay.
    pub fn set_agent_annotations(
        &self,
        lines: Vec<ViewLine>,
        markers: Vec<ViewMarker>,
    ) -> Result<(), CoreError> {
        guarded(|| {
            let layer = crate::inspect::annotations(&lines, &markers)?;
            let merged = {
                let mut marks = self.marks.lock().unwrap_or_else(PoisonError::into_inner);
                marks.agent = layer;
                marks.merged()
            };
            self.lock().set_annotations(merged);
            Ok(())
        })
    }
}

fn camera(v: &render::viewport::Viewport) -> AgentCamera {
    let c = v.camera();
    AgentCamera {
        vpt: c.vpt().to_vec(),
        vpr: c.vpr().to_vec(),
        vpd: c.viewer_distance,
        vpf: c.fov,
    }
}

/// A standard view by the name the protocol uses ([`protocol::VIEWS`]).
fn view_named(name: &str) -> Option<render::snapshot::View> {
    use render::snapshot::View;
    Some(match name {
        "top" => View::Top,
        "bottom" => View::Bottom,
        "left" => View::Left,
        "right" => View::Right,
        "front" => View::Front,
        "back" => View::Back,
        "diagonal" | "iso" => View::Iso,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_keep_the_views_proportions_within_bounds() {
        assert_eq!(capture_size((1600, 800), 768), (768, 384));
        // Never larger than the view.
        assert_eq!(capture_size((400, 300), 768), (400, 300));
        // A view not in a window yet.
        assert_eq!(capture_size((0, 0), 512), (512, 512));
        // Out-of-range sizes are clamped to the protocol's.
        assert_eq!(capture_size((8000, 4000), 100_000), (2048, 1024));
        assert_eq!(capture_size((100, 4000), 64), (16, 64));
        for name in protocol::VIEWS {
            assert!(view_named(name).is_some(), "{name}");
        }
    }

    #[test]
    fn the_agent_has_its_own_layer_and_captures_the_view_as_shown() {
        let v = match Viewport::new("Cornfield".into()) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("skipped: {e}");
                return;
            }
        };
        let marker = |label: &str| ViewMarker {
            point: vec![1.0, 2.0, 3.0],
            label: label.into(),
            color: vec![1.0, 0.0, 0.0, 1.0],
        };
        v.set_annotations(Vec::new(), vec![marker("1"), marker("2")])
            .unwrap();
        v.set_agent_annotations(Vec::new(), vec![marker("here")])
            .unwrap();
        assert_eq!(v.lock().annotations().markers.len(), 3);
        // The panels replace only theirs, and the agent only its own.
        v.set_annotations(Vec::new(), vec![marker("3")]).unwrap();
        let labels: Vec<String> = v
            .lock()
            .annotations()
            .markers
            .iter()
            .map(|m| m.label.clone())
            .collect();
        assert_eq!(labels, ["3", "here"]);
        v.set_agent_annotations(Vec::new(), Vec::new()).unwrap();
        assert_eq!(v.lock().annotations().markers.len(), 1);

        let cam = v
            .apply_agent_camera(AgentCameraChange {
                view: Some("top".into()),
                fit: false,
                vpt: Some(vec![1.0, 2.0, 3.0]),
                vpr: None,
                vpd: Some(50.0),
            })
            .unwrap();
        assert_eq!((cam.vpt.clone(), cam.vpd), (vec![1.0, 2.0, 3.0], 50.0));
        assert!(
            v.apply_agent_camera(AgentCameraChange {
                view: Some("sideways".into()),
                fit: false,
                vpt: None,
                vpr: None,
                vpd: None,
            })
            .is_err()
        );
        // A view in no window yet captures a square.
        let c = v.capture_as_shown(128).unwrap();
        assert_eq!((c.width, c.height), (128, 128));
        assert!(c.png.starts_with(b"\x89PNG\r\n\x1a\n"));
        assert_eq!(c.camera.vpd, 50.0);
        assert!(!c.backend.is_empty());
    }
}
