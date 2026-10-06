//! "Connect your AI agent", without GTK (docs/linux-app.md, "AI agents";
//! docs/agent-bridge.md, "Desktop apps"): the consent and its settings
//! file, which `neoscad` the setup rows name, what the header button
//! shows, and the app's [`AgentHost`], which carries each request from the
//! link's threads to the GTK main loop and waits there for the answer.
//!
//! The link itself (`agent_link::AgentLink`) and the protocol
//! (`client::agent`) are shared with the macOS and Windows apps; the
//! setup rows are `client::agent_setup`'s and the work that needs the
//! machine (running `claude`) is `agent_link::setup`'s. This module is
//! only what is particular to this app, kept apart from the widgets so it
//! is tested on every platform.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use client::agent::{
    AgentCamera, AgentCameraChange, AgentCapture, AgentClient, AgentDocumentState,
    AgentEditOutcome, AgentEditRequest, AgentHost, AgentStatus, AgentTextEdit, CAPTURE_MAX,
    CAPTURE_MIN, EditorPosition, VIEWS,
};
use client::agent_setup::{Client, Host, UsageTopic};
use client::{ViewLine, ViewMarker};
use render::viewport::{AnnotationLine, AnnotationMarker, Annotations, Viewport};
use serde_json::{Value, json};

// --- The consent ------------------------------------------------------------

/// The user's choices about agents, kept between runs of the app.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    /// "Allow AI agents to work on open documents": off until the user
    /// turns it on (the owner's decision 2). Until then the app has no
    /// socket and no thread for agents.
    pub allowed: bool,
    /// "Ask before applying an agent's edits": each edit waits for Apply,
    /// as the web page's switch (off by default there too).
    pub ask_first: bool,
    /// The user turned agents off after having them on: the header button
    /// then hides, and the main menu's item is the way back. Someone who
    /// never tried them keeps the button, which is how they find them.
    pub turned_off: bool,
    /// The client last picked in the Agents page's selector, so the page
    /// opens on the client the user has rather than on Claude Code each
    /// time; `None` until they pick one ([`chosen_client`]).
    pub setup_client: Option<Client>,
}

impl Settings {
    pub fn from_json(bytes: &[u8]) -> Settings {
        let v: Value = serde_json::from_slice(bytes).unwrap_or(Value::Null);
        Settings {
            allowed: v["allowed"].as_bool().unwrap_or(false),
            ask_first: v["askFirst"].as_bool().unwrap_or(false),
            turned_off: v["turnedOff"].as_bool().unwrap_or(false),
            // A name this build does not know (a newer app's client, a
            // typo) is no choice, rather than a reason to drop the
            // consent read above.
            setup_client: serde_json::from_value(v["setupClient"].clone()).ok(),
        }
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(&json!({
            "allowed": self.allowed,
            "askFirst": self.ask_first,
            "turnedOff": self.turned_off,
            "setupClient": self.setup_client,
        }))
        .unwrap_or_default()
    }

    /// The file at `path`, or the defaults (not allowed) when it is
    /// missing or unreadable: a damaged file never grants access.
    pub fn load(path: &Path) -> Settings {
        std::fs::read(path)
            .map(|b| Settings::from_json(&b))
            .unwrap_or_default()
    }

    /// Written to a temporary file and renamed over `path`, so a crash
    /// mid-write leaves the old choice rather than half a file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let dir = path.parent().unwrap_or(Path::new("."));
        std::fs::create_dir_all(dir)?;
        let tmp = dir.join(format!("agents.{}.tmp", std::process::id()));
        std::fs::write(&tmp, self.to_json())?;
        std::fs::rename(&tmp, path).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }

    /// The consent switch moved.
    pub fn set_allowed(&mut self, on: bool) {
        if self.allowed && !on {
            self.turned_off = true;
        }
        if on {
            self.turned_off = false;
        }
        self.allowed = on;
    }

    /// Whether the header bar shows the agent button.
    pub fn shows_button(&self) -> bool {
        self.allowed || !self.turned_off
    }
}

/// `$XDG_CONFIG_HOME/neoscad/agents.json` (in the Flatpak, the sandbox's
/// own config directory), beside the update settings.
pub fn settings_path(config_dir: &Path) -> PathBuf {
    config_dir.join("neoscad").join("agents.json")
}

// --- The Agents page's client selector and usage section -------------------

/// The client whose setup the page shows, from those it `offers` (the
/// host's setups, in their order): the one the user picked last, else
/// Claude Code, which most users of the page have, else the first. A kept
/// client the host does not offer (Claude Desktop, picked on another
/// platform's settings or by hand) falls back rather than showing nothing.
pub fn chosen_client(kept: Option<Client>, offers: &[Client]) -> Client {
    [kept, Some(Client::ClaudeCode)]
        .into_iter()
        .flatten()
        .find(|c| offers.contains(c))
        .or_else(|| offers.first().copied())
        .unwrap_or(Client::ClaudeCode)
}

/// One row of "Using NeoSCAD with your agent": an item of the shared text
/// (`client::agent_setup::usage`) with the icon the page gives it, and
/// the example requests for the row that has them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UsageRow {
    pub icon: &'static str,
    pub title: String,
    pub body: String,
    /// Shown under "Things to ask" only; empty elsewhere.
    pub examples: Vec<String>,
}

/// The rows for `client` on `host`, in the shared text's order. The text
/// is the core's, word for word, so the three apps cannot drift apart;
/// only the icons are this app's. They are Adwaita's standard symbolic
/// names, which the GNOME runtime and Ubuntu's icon theme both carry.
pub fn usage_rows(host: Host, client: Client) -> Vec<UsageRow> {
    let usage = client::agent_setup::usage(host, client);
    usage
        .items
        .into_iter()
        .map(|item| UsageRow {
            icon: usage_icon(item.topic),
            examples: if item.topic == UsageTopic::WhatToAsk {
                usage.examples.clone()
            } else {
                Vec::new()
            },
            title: item.title,
            body: item.body,
        })
        .collect()
}

fn usage_icon(topic: UsageTopic) -> &'static str {
    match topic {
        UsageTopic::KeepOpen => "window-new-symbolic",
        UsageTopic::WhatToAsk => "dialog-question-symbolic",
        UsageTopic::Edits => "edit-undo-symbolic",
        UsageTopic::Seeing => "view-reveal-symbolic",
        UsageTopic::Control => "emblem-ok-symbolic",
        UsageTopic::Export => "document-save-as-symbolic",
    }
}

/// The page's "Learn more": the website's guide to agents, which covers
/// every client and the command line in more depth than the page can.
pub const LEARN_MORE_URL: &str = "https://neoscad.org/agents.html";

// --- Which `neoscad` clients run --------------------------------------------

/// The command-line tool the setup rows name, absolute, so a client
/// started with another `PATH` still finds it (docs/mcp.md, "Setup from
/// the apps"): the one beside this program first (a build's
/// `target/debug`, or a package that installs both in `/usr/bin`), then
/// `PATH`, then where `install.sh` and the packages put it. `None` when
/// there is none, and the dialog then says how to get it. Not used in the
/// Flatpak, whose rows run `flatpak run --command=neoscad`.
pub fn find_cli(
    exe: Option<&Path>,
    path_env: &str,
    home: &str,
    is_executable: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(dir) = exe.and_then(Path::parent) {
        candidates.push(dir.join("neoscad"));
    }
    candidates.extend(
        path_env
            .split(':')
            .filter(|d| d.starts_with('/'))
            .map(|d| Path::new(d).join("neoscad")),
    );
    if !home.is_empty() {
        candidates.push(Path::new(home).join(".local/bin/neoscad"));
    }
    candidates.push(PathBuf::from("/usr/local/bin/neoscad"));
    candidates.push(PathBuf::from("/usr/bin/neoscad"));
    candidates.into_iter().find(|p| is_executable(p))
}

// --- What the header button shows -------------------------------------------

/// The header button's look for a status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ButtonLook {
    /// Beside the mark: the client's name ("Claude Code"), "2 agents", or
    /// nothing while none is connected (the mark alone, as GNOME's header
    /// bars keep their buttons small).
    pub label: Option<String>,
    /// `client::agent::status_line`: "Connect your AI agent", "Claude Code
    /// is editing".
    pub tooltip: String,
    /// A client is connected: the status dot shows.
    pub connected: bool,
    /// A client is doing something now: the dot pulses.
    pub working: bool,
}

pub fn button_look(status: &AgentStatus) -> ButtonLook {
    let tooltip = client::agent::status_line(status);
    let label = match status.clients.as_slice() {
        [] => None,
        [c] => Some(client_name(c)),
        cs => Some(format!("{} agents", cs.len())),
    };
    ButtonLook {
        label,
        tooltip,
        connected: !status.clients.is_empty(),
        working: status.clients.iter().any(|c| c.activity.is_some()),
    }
}

/// A client's self-reported name, or "An agent". Shown as a label: the
/// name is the client's own claim, not an identity.
pub fn client_name(c: &AgentClient) -> String {
    c.name.clone().unwrap_or_else(|| "An agent".to_string())
}

/// The clients in `new` that were not in `old`, by name, once each has
/// said who it is: for "Claude Code connected" toasts. `neoscad mcp`
/// connects as it starts, before its client's `initialize` names it, so a
/// toast at that moment could only say "An agent connected"; the name
/// arriving is the moment to tell the user.
pub fn newly_connected(old: &AgentStatus, new: &AgentStatus) -> Vec<String> {
    new.clients
        .iter()
        .filter(|c| c.name.is_some())
        .filter(|c| !old.clients.iter().any(|o| o.id == c.id && o.name.is_some()))
        .map(client_name)
        .collect()
}

/// The approval bar's sentence: "Claude Code wants to change line 12-14."
pub fn approval_text(edit: &AgentEditRequest) -> String {
    let who = edit.client.as_deref().unwrap_or("An agent");
    if edit.summary.is_empty() {
        format!("{who} wants to change the text.")
    } else {
        format!("{who} wants to change {}.", edit.summary)
    }
}

// --- Edits ------------------------------------------------------------------

/// Whether an agent's edit may go to the editor now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditCheck {
    /// Yes: the document is at the version the agent read, and the editor
    /// is in step with the window's copy at `editor_version`.
    Apply { editor_version: u64 },
    /// No: the text moved on (the user typed), or the editor is being
    /// loaded or resynchronised and is about to.
    Stale { version: u64 },
}

/// `revision` is the document's ([`crate::document::Document::revision`]),
/// `editor_version` the editor version the window's copy matches (`None`
/// while a load or resync is on its way).
pub fn edit_check(revision: u64, requested: u64, editor_version: Option<u64>) -> EditCheck {
    match editor_version {
        Some(editor_version) if requested == revision => EditCheck::Apply { editor_version },
        _ => EditCheck::Stale { version: revision },
    }
}

/// How long the editor highlights an agent's change (`agentEdit`'s `ms`,
/// its own default).
pub const HIGHLIGHT_MS: u64 = 8000;

/// The arguments of the editor's `agentEdit(edits, ms, expectVersion)`:
/// the edits as `{from: [line, character], to, insert}`, and the editor
/// version the window checked them against. The editor refuses them
/// (`stale: true`) when it has moved past that version: a keystroke the
/// window has not heard of yet, still on its way from the web process,
/// would otherwise put the agent's positions in the wrong place.
pub fn agent_edit_args(editor_version: u64, edits: &[AgentTextEdit]) -> [Value; 3] {
    let edits: Vec<Value> = edits
        .iter()
        .map(|e| {
            json!({
                "from": [e.from.line, e.from.character],
                "to": [e.to.line, e.to.character],
                "insert": e.insert,
            })
        })
        .collect();
    [
        Value::Array(edits),
        json!(HIGHLIGHT_MS),
        json!(editor_version),
    ]
}

/// What the editor's answer to `agentEdit` ([`agent_edit_args`]) means.
/// `revision` is the document's when the edit was sent: the edit's own
/// `changes` message makes it one more, unless it changed nothing (no
/// edits, or the same text put back), which CodeMirror does not report.
pub fn edit_answer(revision: u64, editor_version: u64, answer: &Value) -> AgentEditOutcome {
    if answer["stale"].as_bool() == Some(true) {
        return AgentEditOutcome::Stale { version: revision };
    }
    match answer["version"].as_u64() {
        Some(v) if v == editor_version => AgentEditOutcome::Applied { version: revision },
        Some(_) => AgentEditOutcome::Applied {
            version: revision + 1,
        },
        // No answer: the page went away under the edit.
        None => AgentEditOutcome::Stale { version: revision },
    }
}

// --- The view ---------------------------------------------------------------

/// The width and height of a capture whose longest side is at most
/// `max_side`, in the view's proportions and never larger than the view
/// (the web page's `captureSize`); a view without a size gives a square.
/// As `crates/ffi/src/agent.rs`'s, for the same reasons.
pub fn capture_size(view: (u32, u32), max_side: u32) -> (u32, u32) {
    let max_side = max_side.clamp(CAPTURE_MIN, CAPTURE_MAX);
    if view.0 == 0 || view.1 == 0 {
        return (max_side, max_side);
    }
    let (w, h) = (f64::from(view.0), f64::from(view.1));
    let scale = (f64::from(max_side) / w.max(h)).min(1.0);
    // At least 16 pixels a side, as the export image requires.
    let side = |x: f64| ((x * scale).round() as u32).max(16);
    (side(w), side(h))
}

/// The camera as the protocol gives it.
pub fn camera_of(v: &Viewport) -> AgentCamera {
    let c = v.camera();
    AgentCamera {
        vpt: c.vpt().to_vec(),
        vpr: c.vpr().to_vec(),
        vpd: c.viewer_distance,
        vpf: c.fov,
    }
}

/// A standard view by the protocol's name ([`VIEWS`], and `iso`).
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

/// Move the camera as an agent asked (`view_camera`): the standard view,
/// View All, then each of `vpt`, `vpr` and `vpd` given. Checked before
/// anything moves, so a bad argument leaves the view alone.
pub fn apply_camera(v: &mut Viewport, change: &AgentCameraChange) -> Result<AgentCamera, String> {
    let three = |p: &Option<Vec<f64>>, what: &str| {
        p.as_deref()
            .map(|p| client::point3(p, what).map_err(|e| e.to_string()))
            .transpose()
    };
    let (vpt, vpr) = (three(&change.vpt, "vpt")?, three(&change.vpr, "vpr")?);
    let view = match change.view.as_deref() {
        None => None,
        Some(name) => Some(
            view_named(name)
                .ok_or_else(|| format!("unknown view '{name}' (one of {})", VIEWS.join(", ")))?,
        ),
    };
    if let Some(view) = view {
        v.set_view(view);
    }
    if change.fit {
        v.view_all();
    }
    if vpt.is_some() || vpr.is_some() || change.vpd.is_some() {
        v.set_file_view(vpt, vpr, change.vpd.filter(|d| *d > 0.0), None);
    }
    Ok(camera_of(v))
}

fn rgba(c: &[f32]) -> [f32; 4] {
    match c {
        [r, g, b, a, ..] => [*r, *g, *b, *a],
        [r, g, b] => [*r, *g, *b, 1.0],
        _ => [1.0, 0.0, 1.0, 1.0],
    }
}

/// Lines and markers as the renderer draws them: the side panels' marks
/// (`crate::inspect::annotations`) and an agent's. A trailing partial
/// point of a line is dropped, and so is a marker whose point is not three
/// finite numbers (a marker at the origin instead would be a lie).
pub fn marks(lines: &[ViewLine], markers: &[ViewMarker]) -> Annotations {
    Annotations {
        lines: lines
            .iter()
            .map(|l| AnnotationLine {
                points: l.points.as_chunks::<3>().0.to_vec(),
                closed: l.closed,
                color: rgba(&l.color),
            })
            .collect(),
        markers: markers
            .iter()
            .filter_map(|m| {
                Some(AnnotationMarker {
                    point: client::point3(&m.point, "a marker").ok()?,
                    label: m.label.clone(),
                    color: rgba(&m.color),
                })
            })
            .collect(),
    }
}

/// The panels' marks with the agent's over them: two layers, so neither
/// replaces the other.
pub fn merge_marks(panels: Annotations, agent: &Annotations) -> Annotations {
    let mut all = panels;
    all.lines.extend(agent.lines.iter().cloned());
    all.markers.extend(agent.markers.iter().cloned());
    all
}

/// The chip over the view while an agent's marks show ("2 agent marks",
/// with a button that clears them, as the macOS app's `AgentMarksChip`
/// is), or `None` when there are none and the chip hides. It counts what
/// the view draws, so a marker dropped for a bad point is not counted:
/// a chip claiming a mark the user cannot find would send them looking.
pub fn marks_chip(agent: &Annotations) -> Option<String> {
    match agent.lines.len() + agent.markers.len() {
        0 => None,
        1 => Some("1 agent mark".into()),
        n => Some(format!("{n} agent marks")),
    }
}

// --- The host: the link's threads to the main loop ---------------------------

/// How long a request waits for the main loop, a little under the command
/// line's own limits (`crates/cli/src/mcp/tools/browser.rs`: 15 s, 150 s
/// for an edit, 90 s for a capture), so the agent hears this app's reason
/// rather than a bare timeout.
pub const QUICK_WAIT: Duration = Duration::from_secs(14);
/// An edit may wait for the user's Apply (the web page's 140 s).
pub const EDIT_WAIT: Duration = Duration::from_secs(145);
pub const CAPTURE_WAIT: Duration = Duration::from_secs(85);

/// One answer, sent from the main loop to the link's thread waiting for
/// it. Dropping it unanswered (the window closed, the job never ran)
/// tells the waiting thread so at once.
#[derive(Debug)]
pub struct Reply<T>(mpsc::SyncSender<Result<T, String>>);

impl<T> Reply<T> {
    pub fn send(self, r: Result<T, String>) {
        // The thread may have stopped waiting (a timeout): nothing to do.
        let _ = self.0.send(r);
    }
}

/// What to draw for a capture: an offscreen copy of the window's view as
/// shown (`Viewport::copy_as_shown`), which the link's thread draws and
/// reads back, so the main loop never waits for the GPU.
#[derive(Debug)]
pub struct CaptureSource {
    pub copy: Viewport,
    pub camera: AgentCamera,
    pub backend: String,
}

/// The windows' side of each request, called on the main thread by the
/// GTK app (`src/app/agent.rs`). Each answers through its [`Reply`], at
/// once or later (after the editor answers, or the user approves).
pub trait AgentWindows {
    fn read(&self, document: u64, reply: Reply<AgentDocumentState>);
    fn edit(&self, document: u64, edit: AgentEditRequest, reply: Reply<AgentEditOutcome>);
    fn reveal(&self, document: u64, from: EditorPosition, to: EditorPosition, reply: Reply<()>);
    fn camera(&self, document: u64, change: AgentCameraChange, reply: Reply<AgentCamera>);
    fn capture(&self, document: u64, max_side: u32, reply: Reply<CaptureSource>);
    fn annotate(
        &self,
        document: u64,
        lines: Vec<ViewLine>,
        markers: Vec<ViewMarker>,
        reply: Reply<()>,
    );
}

/// Work for the main loop.
pub type Job = Box<dyn FnOnce(&dyn AgentWindows) + Send>;

/// Hands a job to the main loop (`glib::MainContext::invoke` in the app).
/// It must not run the job on the calling thread: that is a link thread.
pub type Post = Box<dyn Fn(Job) + Send + Sync>;

/// The app's [`AgentHost`]: each request becomes a job on the main loop,
/// and the link's thread waits for its answer. The main loop never waits
/// for anything here.
pub struct MainLoopHost {
    post: Post,
}

impl std::fmt::Debug for MainLoopHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MainLoopHost").finish_non_exhaustive()
    }
}

impl MainLoopHost {
    pub fn new(post: Post) -> MainLoopHost {
        MainLoopHost { post }
    }

    fn ask<T: Send + 'static>(
        &self,
        limit: Duration,
        f: impl FnOnce(&dyn AgentWindows, Reply<T>) + Send + 'static,
    ) -> Result<T, String> {
        let (tx, rx) = mpsc::sync_channel(1);
        (self.post)(Box::new(move |w| f(w, Reply(tx))));
        match rx.recv_timeout(limit) {
            Ok(r) => r,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(format!(
                "NeoSCAD did not answer within {} s (it may be busy, or waiting for the user)",
                limit.as_secs()
            )),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err("the document was closed".to_string()),
        }
    }
}

impl AgentHost for MainLoopHost {
    fn read(&self, document: u64) -> Result<AgentDocumentState, String> {
        self.ask(QUICK_WAIT, move |w, r| w.read(document, r))
    }

    fn edit(&self, document: u64, edit: AgentEditRequest) -> Result<AgentEditOutcome, String> {
        self.ask(EDIT_WAIT, move |w, r| w.edit(document, edit, r))
    }

    fn reveal(
        &self,
        document: u64,
        from: EditorPosition,
        to: EditorPosition,
    ) -> Result<(), String> {
        self.ask(QUICK_WAIT, move |w, r| w.reveal(document, from, to, r))
    }

    fn camera(&self, document: u64, change: AgentCameraChange) -> Result<AgentCamera, String> {
        self.ask(QUICK_WAIT, move |w, r| w.camera(document, change, r))
    }

    fn capture(&self, document: u64, max_side: u32) -> Result<AgentCapture, String> {
        let mut source = self.ask(CAPTURE_WAIT, move |w, r| w.capture(document, max_side, r))?;
        // Drawn here, on the link's thread: the readback waits for the GPU,
        // which the main loop must not.
        let image = source
            .copy
            .read_pixels_blocking()
            .map_err(|e| format!("could not draw the view: {e}"))?;
        Ok(AgentCapture {
            png: render::encode_png(image.width, image.height, &image.rgba),
            width: image.width,
            height: image.height,
            camera: source.camera,
            backend: source.backend,
        })
    }

    fn annotate(
        &self,
        document: u64,
        lines: Vec<ViewLine>,
        markers: Vec<ViewMarker>,
    ) -> Result<(), String> {
        self.ask(QUICK_WAIT, move |w, r| {
            w.annotate(document, lines, markers, r)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use client::agent::AgentRunStatus;
    use std::sync::{Arc, Mutex};

    #[test]
    fn consent_is_off_until_given_and_kept() {
        let dir = std::env::temp_dir().join(format!("neoscad-agents-{}", std::process::id()));
        let path = settings_path(&dir);
        let mut s = Settings::load(&path);
        assert_eq!(s, Settings::default());
        assert!(!s.allowed && s.shows_button());
        s.set_allowed(true);
        s.ask_first = true;
        s.save(&path).unwrap();
        let back = Settings::load(&path);
        assert!(back.allowed && back.ask_first && !back.turned_off);
        // Turned off after being on: the button hides, the choice is kept.
        let mut s = back;
        s.set_allowed(false);
        assert!(s.turned_off && !s.shows_button());
        s.set_allowed(true);
        assert!(s.shows_button() && !s.turned_off);
        // A damaged file grants nothing.
        std::fs::write(&path, "{\"allowed\": tru").unwrap();
        assert!(!Settings::load(&path).allowed);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_selector_opens_on_claude_code_then_on_the_last_pick() {
        let linux: Vec<Client> = client::agent_setup::setups(
            Host::Linux,
            &client::agent_setup::ServerCommand::for_host(Host::Linux, "/usr/bin/neoscad"),
            "/home/u",
            None,
        )
        .iter()
        .map(|r| r.client)
        .collect();
        // Claude Desktop has no Linux build, so the selector never offers it.
        assert_eq!(
            linux,
            [
                Client::ClaudeCode,
                Client::Cursor,
                Client::VsCode,
                Client::Other
            ]
        );
        assert_eq!(chosen_client(None, &linux), Client::ClaudeCode);
        assert_eq!(chosen_client(Some(Client::VsCode), &linux), Client::VsCode);
        assert_eq!(
            chosen_client(Some(Client::ClaudeDesktop), &linux),
            Client::ClaudeCode
        );
        assert_eq!(
            chosen_client(Some(Client::ClaudeDesktop), &[Client::Other]),
            Client::Other
        );

        // The pick is kept in agents.json beside the consent, and a name
        // this build does not know leaves the consent intact.
        let dir = std::env::temp_dir().join(format!("neoscad-agents-pick-{}", std::process::id()));
        let path = settings_path(&dir);
        assert_eq!(Settings::load(&path).setup_client, None);
        let s = Settings {
            allowed: true,
            setup_client: Some(Client::Cursor),
            ..Settings::default()
        };
        s.save(&path).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("\"setupClient\": \"cursor\"")
        );
        assert_eq!(Settings::load(&path), s);
        let s = Settings::from_json(br#"{"allowed": true, "setupClient": "zed"}"#);
        assert!(s.allowed);
        assert_eq!(s.setup_client, None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_usage_rows_are_the_shared_text_with_examples_under_things_to_ask() {
        for host in [Host::Linux, Host::LinuxFlatpak] {
            for c in [
                Client::ClaudeCode,
                Client::Cursor,
                Client::VsCode,
                Client::Other,
            ] {
                let shared = client::agent_setup::usage(host, c);
                let rows = usage_rows(host, c);
                assert_eq!(rows.len(), 6);
                for (row, item) in rows.iter().zip(&shared.items) {
                    assert_eq!((&row.title, &row.body), (&item.title, &item.body));
                    assert!(row.icon.ends_with("-symbolic"));
                    if item.topic == UsageTopic::WhatToAsk {
                        assert_eq!(row.examples, shared.examples);
                    } else {
                        assert!(row.examples.is_empty(), "{}", row.title);
                    }
                }
                let icons: std::collections::HashSet<_> = rows.iter().map(|r| r.icon).collect();
                assert_eq!(icons.len(), rows.len());
                // What this app has: Ctrl keys, no autosave, the marks
                // chip, the header button and Preferences > Agents.
                let all: String = rows.iter().map(|r| r.body.as_str()).collect();
                assert!(!all.contains('⌘') && all.contains("chip over the view"));
                assert!(all.contains("Ctrl+Z") && all.contains("only when you save"));
                assert!(all.contains("header bar") && all.contains("Preferences > Agents"));
            }
        }
    }

    #[test]
    fn the_cli_beside_the_app_comes_first_then_path() {
        let exists =
            |set: &'static [&'static str]| move |p: &Path| set.iter().any(|s| Path::new(s) == p);
        let exe = Path::new("/opt/neoscad/bin/neoscad-gtk");
        assert_eq!(
            find_cli(
                Some(exe),
                "/usr/bin:/bin",
                "/home/u",
                exists(&["/opt/neoscad/bin/neoscad", "/usr/bin/neoscad"])
            ),
            Some(PathBuf::from("/opt/neoscad/bin/neoscad"))
        );
        assert_eq!(
            find_cli(
                Some(exe),
                "relative:/usr/bin",
                "/home/u",
                exists(&["/usr/bin/neoscad"])
            ),
            Some(PathBuf::from("/usr/bin/neoscad"))
        );
        assert_eq!(
            find_cli(None, "", "/home/u", exists(&["/home/u/.local/bin/neoscad"])),
            Some(PathBuf::from("/home/u/.local/bin/neoscad"))
        );
        assert_eq!(
            find_cli(Some(exe), "/usr/bin", "/home/u", exists(&[])),
            None
        );
    }

    fn client(id: u64, name: Option<&str>, activity: Option<&str>) -> AgentClient {
        AgentClient {
            id,
            name: name.map(str::to_string),
            activity: activity.map(str::to_string),
            document: None,
        }
    }

    #[test]
    fn the_button_names_the_agent_and_pulses_while_it_works() {
        let mut s = AgentStatus {
            allowed: true,
            listening: true,
            ..AgentStatus::default()
        };
        let idle = button_look(&s);
        assert_eq!(idle.label, None);
        assert_eq!(idle.tooltip, "Connect your AI agent");
        assert!(!idle.connected && !idle.working);

        // Connected, but not named yet (before the client's `initialize`):
        // no toast until the name comes.
        let before = s.clone();
        s.clients.push(client(3, None, None));
        assert!(newly_connected(&before, &s).is_empty());
        let unnamed = s.clone();
        s.clients[0].name = Some("Claude Code".into());
        assert_eq!(newly_connected(&unnamed, &s), ["Claude Code"]);
        assert_eq!(newly_connected(&s, &s), Vec::<String>::new());
        let one = button_look(&s);
        assert_eq!(one.label.as_deref(), Some("Claude Code"));
        assert!(one.connected && !one.working);

        s.clients[0].activity = Some("is editing".into());
        let busy = button_look(&s);
        assert_eq!(busy.tooltip, "Claude Code is editing");
        assert!(busy.working);

        s.clients.push(client(4, None, None));
        assert_eq!(button_look(&s).label.as_deref(), Some("2 agents"));
    }

    fn request(version: u64, summary: &str) -> AgentEditRequest {
        AgentEditRequest {
            version,
            edits: vec![AgentTextEdit {
                from: EditorPosition {
                    line: 0,
                    character: 5,
                },
                to: EditorPosition {
                    line: 0,
                    character: 7,
                },
                insert: "12".into(),
            }],
            summary: summary.into(),
            client: Some("Claude Code".into()),
        }
    }

    fn linux_args() -> Value {
        Value::Array(agent_edit_args(31, &request(7, "").edits).to_vec())
    }

    #[test]
    fn an_edit_goes_through_only_on_the_version_read_and_an_editor_in_step() {
        assert_eq!(
            edit_check(7, 7, Some(31)),
            EditCheck::Apply { editor_version: 31 }
        );
        // The user typed since the agent read version 6.
        assert_eq!(edit_check(7, 6, Some(31)), EditCheck::Stale { version: 7 });
        // A load or a resync is on its way: the text is about to change.
        assert_eq!(edit_check(7, 7, None), EditCheck::Stale { version: 7 });

        assert_eq!(
            linux_args(),
            json!([[{"from": [0, 5], "to": [0, 7], "insert": "12"}], 8000, 31])
        );
        // The page saw a keystroke the window had not yet.
        assert_eq!(
            edit_answer(7, 31, &json!({"version": 32, "stale": true, "marks": 0})),
            AgentEditOutcome::Stale { version: 7 }
        );
        assert_eq!(
            edit_answer(7, 31, &json!({"version": 32, "marks": 1})),
            AgentEditOutcome::Applied { version: 8 }
        );
        // Nothing changed (the same text put back): no `changes` follows.
        assert_eq!(
            edit_answer(7, 31, &json!({"version": 31, "marks": 0})),
            AgentEditOutcome::Applied { version: 7 }
        );
        assert_eq!(
            edit_answer(7, 31, &Value::Null),
            AgentEditOutcome::Stale { version: 7 }
        );
        assert_eq!(
            approval_text(&request(7, "line 3-4")),
            "Claude Code wants to change line 3-4."
        );
    }

    #[test]
    fn captures_keep_the_views_proportions_within_bounds() {
        assert_eq!(capture_size((1600, 800), 768), (768, 384));
        assert_eq!(capture_size((400, 300), 768), (400, 300));
        assert_eq!(capture_size((0, 0), 512), (512, 512));
        assert_eq!(capture_size((8000, 4000), 100_000), (2048, 1024));
        for name in VIEWS {
            assert!(view_named(name).is_some(), "{name}");
        }
    }

    #[test]
    fn the_agents_marks_are_a_layer_over_the_panels() {
        let line = ViewLine {
            points: vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 9.0],
            closed: false,
            color: vec![1.0, 0.0, 0.0],
        };
        let good = ViewMarker {
            point: vec![1.0, 2.0, 3.0],
            label: "here".into(),
            color: vec![0.0, 1.0, 0.0, 0.5],
        };
        let bad = ViewMarker {
            point: vec![f64::NAN, 0.0, 0.0],
            ..good.clone()
        };
        let agent = marks(&[line], &[good, bad]);
        assert_eq!(
            agent.lines[0].points.len(),
            2,
            "the partial point is dropped"
        );
        assert_eq!(agent.lines[0].color, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(agent.markers.len(), 1);
        let panels = marks(
            &[],
            &[ViewMarker {
                point: vec![0.0; 3],
                label: "check".into(),
                color: vec![1.0; 3],
            }],
        );
        let all = merge_marks(panels, &agent);
        assert_eq!(all.markers.len(), 2);
        assert_eq!(all.markers[0].label, "check");
        assert_eq!(all.lines.len(), 1);
    }

    #[test]
    fn the_marks_chip_counts_what_the_view_draws() {
        assert_eq!(marks_chip(&Annotations::default()), None);
        let marker = |p: f64| ViewMarker {
            point: vec![p, 0.0, 0.0],
            label: String::new(),
            color: vec![1.0; 3],
        };
        assert_eq!(
            marks_chip(&marks(&[], &[marker(1.0)])).as_deref(),
            Some("1 agent mark")
        );
        let line = ViewLine {
            points: vec![0.0; 6],
            closed: false,
            color: vec![1.0; 3],
        };
        // The bad marker is not drawn, so not counted.
        let a = marks(&[line], &[marker(1.0), marker(f64::NAN)]);
        assert_eq!(marks_chip(&a).as_deref(), Some("2 agent marks"));
    }

    /// Windows that answer from a table, as the main loop would; the
    /// `post` runs each job on a thread of its own, standing in for the
    /// main loop (the host must not depend on running it in place).
    #[derive(Default)]
    struct Fake {
        text: Mutex<String>,
        version: Mutex<u64>,
        log: Mutex<Vec<String>>,
    }

    impl AgentWindows for Fake {
        fn read(&self, document: u64, reply: Reply<AgentDocumentState>) {
            if document != 1 {
                return; // dropped: the document is gone
            }
            reply.send(Ok(AgentDocumentState {
                version: *self.version.lock().unwrap(),
                text: self.text.lock().unwrap().clone(),
                selection: None,
                overrides: Vec::new(),
                parts: false,
                run: AgentRunStatus {
                    mode: None,
                    summary: String::new(),
                    running: false,
                },
                console: Vec::new(),
            }));
        }

        fn edit(&self, _: u64, edit: AgentEditRequest, reply: Reply<AgentEditOutcome>) {
            let mut v = self.version.lock().unwrap();
            if let EditCheck::Stale { version } = edit_check(*v, edit.version, Some(1)) {
                return reply.send(Ok(AgentEditOutcome::Stale { version }));
            }
            let mut t = self.text.lock().unwrap();
            let e = &edit.edits[0];
            t.replace_range(
                e.from.character as usize..e.to.character as usize,
                &e.insert,
            );
            *v += 1;
            reply.send(Ok(AgentEditOutcome::Applied { version: *v }));
        }

        fn reveal(&self, _: u64, from: EditorPosition, _: EditorPosition, reply: Reply<()>) {
            self.log
                .lock()
                .unwrap()
                .push(format!("reveal {}", from.line));
            reply.send(Ok(()));
        }

        fn camera(&self, _: u64, _: AgentCameraChange, _: Reply<AgentCamera>) {
            // Never answers in time (a stuck main loop).
            std::thread::sleep(Duration::from_millis(300));
        }

        fn capture(&self, _: u64, _: u32, reply: Reply<CaptureSource>) {
            reply.send(Err("NeoSCAD has no 3D view here".into()));
        }

        fn annotate(&self, _: u64, l: Vec<ViewLine>, m: Vec<ViewMarker>, reply: Reply<()>) {
            self.log
                .lock()
                .unwrap()
                .push(format!("marks {} {}", l.len(), m.len()));
            reply.send(Ok(()));
        }
    }

    fn host(fake: Arc<Fake>) -> MainLoopHost {
        MainLoopHost::new(Box::new(move |job: Job| {
            let fake = fake.clone();
            std::thread::spawn(move || job(&*fake));
        }))
    }

    #[test]
    fn requests_reach_the_windows_and_their_answers_come_back() {
        let fake = Arc::new(Fake::default());
        *fake.text.lock().unwrap() = "cube(10);".into();
        *fake.version.lock().unwrap() = 4;
        let h = host(fake.clone());
        let s = h.read(1).unwrap();
        assert_eq!((s.version, s.text.as_str()), (4, "cube(10);"));
        assert_eq!(
            h.edit(1, request(4, "line 1")).unwrap(),
            AgentEditOutcome::Applied { version: 5 }
        );
        assert_eq!(fake.text.lock().unwrap().as_str(), "cube(12);");
        assert_eq!(
            h.edit(1, request(4, "line 1")).unwrap(),
            AgentEditOutcome::Stale { version: 5 }
        );
        h.reveal(
            1,
            EditorPosition {
                line: 2,
                character: 0,
            },
            EditorPosition {
                line: 2,
                character: 0,
            },
        )
        .unwrap();
        h.annotate(1, Vec::new(), Vec::new()).unwrap();
        assert_eq!(*fake.log.lock().unwrap(), ["reveal 2", "marks 0 0"]);
        assert_eq!(
            h.capture(1, 512).unwrap_err(),
            "NeoSCAD has no 3D view here"
        );
        // A closed document drops its reply: the agent hears so at once.
        assert_eq!(h.read(9).unwrap_err(), "the document was closed");
    }

    #[test]
    fn a_main_loop_that_does_not_answer_times_out() {
        let fake = Arc::new(Fake::default());
        let h = host(fake);
        let start = std::time::Instant::now();
        let r = h.ask(Duration::from_millis(50), |w, r| {
            w.camera(
                1,
                AgentCameraChange {
                    view: None,
                    fit: true,
                    vpt: None,
                    vpr: None,
                    vpd: None,
                },
                r,
            )
        });
        assert!(r.unwrap_err().contains("did not answer within"));
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
