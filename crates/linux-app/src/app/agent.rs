//! "Connect your AI agent", the GTK side (docs/linux-app.md, "AI agents";
//! the design is `docs/audits/agent-connection-desktop.md`, Option C and
//! its GTK spec): the link to `neoscad mcp` for the whole process, the
//! header button every window shows, its popover, and the Agents page
//! (the "Connect Your AI Agent" dialog and a page of Preferences). What
//! is not GTK is `linux_app::agent`; each window's side of a request is
//! `window/agent.rs`.
//!
//! Nothing runs for agents until the user allows them: the link is made at
//! start-up but has no thread and no socket before `start`, which follows
//! the consent switch (or a consent given in an earlier run).
//!
//! Requests arrive on the link's threads. The host (`MainLoopHost`) hands
//! each to the main loop with `MainContext::invoke`, where a thread-local
//! finds the app's windows; the link's thread waits for the answer, the
//! main loop never does. Status changes take the same way in.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;

use adw::prelude::*;
use gtk::{gio, glib};

use agent_link::setup as machine;
use agent_link::{AgentLink, AgentObserver, LinkConfig};
use client::agent::{
    AgentCamera, AgentCameraChange, AgentDocumentState, AgentEditOutcome, AgentEditRequest,
    AgentStatus, EditorPosition,
};
use client::agent_setup::{self as setup, ClientSetup, SetupAction};
use client::{ViewLine, ViewMarker};
use linux_app::agent::{
    self as logic, AgentWindows, CaptureSource, Job, MainLoopHost, Reply, Settings,
};

use super::{Shared, Window};

thread_local! {
    /// The app's shared state, for jobs the link's threads hand to the main
    /// loop: they are `Send`, the windows are not, so the jobs find them
    /// here. Set once at start-up.
    static SHARED: RefCell<Weak<Shared>> = const { RefCell::new(Weak::new()) };
}

/// The app's agent state, one per process (in `Shared`).
pub struct Agents {
    settings: RefCell<Settings>,
    path: PathBuf,
    pub link: AgentLink,
    status: RefCell<AgentStatus>,
    /// The highest status sequence shown: statuses from different link
    /// threads can arrive out of order.
    sequence: Cell<u64>,
    /// The Agents pages open now, refreshed on every status change.
    pages: RefCell<Vec<Weak<AgentPage>>>,
    /// The windows' numbers for the link (`AgentDocuments`): stable while
    /// a window is open, never reused.
    next_document: Cell<u64>,
    /// The `neoscad` the setup rows name (not in the Flatpak, whose rows
    /// run `flatpak run`), found once.
    cli: Option<PathBuf>,
    host: setup::Host,
}

impl Agents {
    pub fn new() -> Agents {
        let path = logic::settings_path(&glib::user_config_dir());
        let settings = Settings::load(&path);
        let post: linux_app::agent::Post = Box::new(|job: Job| {
            glib::MainContext::default().invoke(move || {
                let shared = SHARED.with(|s| s.borrow().upgrade());
                // Without the app (it is quitting) the job is dropped, and
                // with it the reply: the agent hears so at once.
                if let Some(sh) = shared {
                    job(&AppWindows(sh));
                }
            });
        });
        let link = AgentLink::new(
            Arc::new(MainLoopHost::new(post)),
            LinkConfig {
                app: "NeoSCAD".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                platform: "linux".into(),
                // This user's place, or NEOSCAD_AGENT_DIR's (the tests').
                rendezvous: None,
            },
        );
        let host = machine::host();
        let home = machine::home();
        let cli = logic::find_cli(
            std::env::current_exe().ok().as_deref(),
            &std::env::var("PATH").unwrap_or_default(),
            &home,
            machine::is_executable,
        );
        Agents {
            settings: RefCell::new(settings),
            path,
            link,
            status: RefCell::default(),
            sequence: Cell::new(0),
            pages: RefCell::default(),
            next_document: Cell::new(1),
            cli,
            host,
        }
    }

    pub fn settings(&self) -> Settings {
        self.settings.borrow().clone()
    }

    fn change(&self, f: impl FnOnce(&mut Settings)) {
        f(&mut self.settings.borrow_mut());
        if let Err(e) = self.settings.borrow().save(&self.path) {
            glib::g_warning!(
                "neoscad",
                "agent: could not save {}: {e}",
                self.path.display()
            );
        }
    }

    /// A number for a new window's document.
    pub fn new_document(&self) -> u64 {
        let id = self.next_document.get();
        self.next_document.set(id + 1);
        id
    }

    pub fn status(&self) -> AgentStatus {
        self.status.borrow().clone()
    }

    /// The command clients are given: `flatpak run …` in the Flatpak, the
    /// app's `neoscad` by its absolute path elsewhere, or a bare `neoscad`
    /// when none was found (the page then says how to get it).
    fn server(&self) -> setup::ServerCommand {
        setup::ServerCommand::for_host(self.host, &self.cli_text())
    }

    fn cli_text(&self) -> String {
        self.cli
            .as_ref()
            .map_or_else(|| "neoscad".into(), |p| p.to_string_lossy().into_owned())
    }

    /// Clients can be set up: the Flatpak always has its CLI; elsewhere
    /// it must have been found.
    fn has_cli(&self) -> bool {
        self.host == setup::Host::LinuxFlatpak || self.cli.is_some()
    }
}

/// Wire the link to this app: called once at start-up, with the shared
/// state in place. Starts listening when the user allowed agents in an
/// earlier run.
pub fn start(sh: &Rc<Shared>) {
    SHARED.with(|s| *s.borrow_mut() = Rc::downgrade(sh));
    sh.agents.link.set_observer(Some(Arc::new(Observer)));
    let s = sh.agents.settings();
    glib::g_debug!(
        "neoscad",
        "agent: {} ({:?}, cli {:?})",
        if s.allowed { "allowed" } else { "not allowed" },
        sh.agents.host,
        sh.agents.cli
    );
    if s.allowed {
        listen(sh);
    }
    install_css();
}

fn listen(sh: &Shared) {
    sh.agents.link.set_allowed(true);
    match sh.agents.link.start() {
        Ok(address) => glib::g_debug!("neoscad", "agent: listening at {address}"),
        Err(e) => glib::g_warning!("neoscad", "agent: could not listen: {e}"),
    }
}

/// The consent switch moved: listen, or stop at once (every connection
/// ends, and each agent is told why).
fn set_allowed(sh: &Rc<Shared>, on: bool) {
    if sh.agents.settings().allowed == on {
        return;
    }
    sh.agents.change(|s| s.set_allowed(on));
    if on {
        listen(sh);
    } else {
        sh.agents.link.set_allowed(false);
        glib::g_debug!("neoscad", "agent: turned off");
    }
    refresh(sh);
}

/// Told by the link after each change, on its threads.
struct Observer;

impl AgentObserver for Observer {
    fn status_changed(&self, status: AgentStatus, sequence: u64) {
        glib::MainContext::default().invoke(move || {
            if let Some(sh) = SHARED.with(|s| s.borrow().upgrade()) {
                status_changed(&sh, status, sequence);
            }
        });
    }
}

fn status_changed(sh: &Rc<Shared>, status: AgentStatus, sequence: u64) {
    if sequence < sh.agents.sequence.get() {
        return;
    }
    sh.agents.sequence.set(sequence);
    let old = sh.agents.status.replace(status.clone());
    if old == status {
        return;
    }
    for name in logic::newly_connected(&old, &status) {
        glib::g_debug!("neoscad", "agent: {name} connected");
        tell(sh, &format!("{name} connected"));
    }
    if old.clients.len() != status.clients.len() || old.listening != status.listening {
        glib::g_debug!(
            "neoscad",
            "agent: {} connected, listening {}",
            status.clients.len(),
            status.listening
        );
    }
    refresh(sh);
}

/// Show the current status and settings in every window and open page.
fn refresh(sh: &Rc<Shared>) {
    let status = sh.agents.status();
    let settings = sh.agents.settings();
    for w in sh.windows() {
        w.agent_button.show(&status, &settings);
    }
    sh.agents.pages.borrow_mut().retain(|p| match p.upgrade() {
        Some(p) => {
            p.show(sh, &status, &settings);
            true
        }
        None => false,
    });
}

/// A toast in the active window (else the first).
fn tell(sh: &Shared, message: &str) {
    let windows = sh.windows();
    if let Some(w) = windows
        .iter()
        .find(|w| w.widget().is_active())
        .or(windows.first())
    {
        w.toast(message);
    }
}

/// The animation of the status dot while an agent works; GTK leaves it
/// still when the user turned animations off.
fn install_css() {
    let Some(display) = gtk::gdk::Display::default() else {
        return;
    };
    let css = gtk::CssProvider::new();
    css.load_from_string(
        "@keyframes neoscad-agent-pulse { from { opacity: 1; } 50% { opacity: 0.3; } to { opacity: 1; } }\n\
         .agent-dot.working { animation: neoscad-agent-pulse 1.2s ease-in-out infinite; }\n\
         .agent-copy { padding: 6px 12px; }",
    );
    gtk::style_context_add_provider_for_display(
        &display,
        &css,
        gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

// --- The windows, for the link's jobs ---------------------------------------

/// The app's windows as the agent host sees them (on the main loop).
struct AppWindows(Rc<Shared>);

impl AppWindows {
    /// The window showing `document`; `None` drops the caller's reply,
    /// which tells the agent the document was closed.
    fn window(&self, document: u64) -> Option<Rc<Window>> {
        self.0
            .windows()
            .into_iter()
            .find(|w| w.agent_id() == document)
    }
}

impl AgentWindows for AppWindows {
    fn read(&self, document: u64, reply: Reply<AgentDocumentState>) {
        if let Some(w) = self.window(document) {
            w.agent_read(reply);
        }
    }

    fn edit(&self, document: u64, edit: AgentEditRequest, reply: Reply<AgentEditOutcome>) {
        if let Some(w) = self.window(document) {
            let ask = self.0.agents.settings().ask_first;
            w.agent_edit(edit, ask, reply);
        }
    }

    fn reveal(&self, document: u64, from: EditorPosition, to: EditorPosition, reply: Reply<()>) {
        if let Some(w) = self.window(document) {
            w.agent_reveal(from, to, reply);
        }
    }

    fn camera(&self, document: u64, change: AgentCameraChange, reply: Reply<AgentCamera>) {
        if let Some(w) = self.window(document) {
            w.agent_camera(&change, reply);
        }
    }

    fn capture(&self, document: u64, max_side: u32, reply: Reply<CaptureSource>) {
        if let Some(w) = self.window(document) {
            w.agent_capture(max_side, reply);
        }
    }

    fn annotate(
        &self,
        document: u64,
        lines: Vec<ViewLine>,
        markers: Vec<ViewMarker>,
        reply: Reply<()>,
    ) {
        if let Some(w) = self.window(document) {
            w.agent_annotate(&lines, &markers, reply);
        }
    }
}

// --- The header button --------------------------------------------------------

/// The header bar's agent button: a sparkle while none is connected; the
/// agent's name and a green dot, pulsing while it works, when one is. It
/// opens a popover with the status, Disconnect and Set Up.
pub struct AgentButton {
    pub button: gtk::MenuButton,
    dot: gtk::DrawingArea,
    label: gtk::Label,
    content: gtk::Box,
}

impl std::fmt::Debug for AgentButton {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentButton").finish_non_exhaustive()
    }
}

impl AgentButton {
    pub fn new(sh: &Rc<Shared>) -> AgentButton {
        let dot = gtk::DrawingArea::builder()
            .content_width(8)
            .content_height(8)
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        dot.add_css_class("agent-dot");
        dot.set_draw_func(|_, cr, w, h| {
            // GNOME's green (libadwaita's success colour).
            cr.set_source_rgb(
                0x33 as f64 / 255.0,
                0xd1 as f64 / 255.0,
                0x7a as f64 / 255.0,
            );
            let r = f64::from(w.min(h)) / 2.0;
            cr.arc(
                f64::from(w) / 2.0,
                f64::from(h) / 2.0,
                r,
                0.0,
                std::f64::consts::TAU,
            );
            let _ = cr.fill();
        });
        let label = gtk::Label::builder().visible(false).build();
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        row.append(&sparkle());
        row.append(&dot);
        row.append(&label);
        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(12)
            .margin_top(12)
            .margin_bottom(12)
            .margin_start(12)
            .margin_end(12)
            .width_request(280)
            .build();
        let popover = gtk::Popover::builder().child(&content).build();
        let button = gtk::MenuButton::builder()
            .child(&row)
            .popover(&popover)
            .tooltip_text("Connect your AI agent")
            .build();
        button.update_property(&[gtk::accessible::Property::Label("AI agents")]);
        let b = AgentButton {
            button,
            dot,
            label,
            content,
        };
        // The popover's rows are made when it opens, from the status then,
        // and again on each change while it is open.
        let weak = Rc::downgrade(sh);
        let content = b.content.clone();
        popover.connect_show(move |_| {
            if let Some(sh) = weak.upgrade() {
                fill_popover(&sh, &content, &sh.agents.status(), &sh.agents.settings());
            }
        });
        b.show(&sh.agents.status(), &sh.agents.settings());
        b
    }

    /// Show `status`.
    pub fn show(&self, status: &AgentStatus, settings: &Settings) {
        let look = logic::button_look(status);
        self.button.set_visible(settings.shows_button());
        self.button.set_tooltip_text(Some(&look.tooltip));
        self.label.set_text(look.label.as_deref().unwrap_or(""));
        self.label.set_visible(look.label.is_some());
        self.dot.set_visible(look.connected);
        if look.working {
            self.dot.add_css_class("working");
        } else {
            self.dot.remove_css_class("working");
        }
        if self.button.is_active()
            && let Some(sh) = SHARED.with(|s| s.borrow().upgrade())
        {
            fill_popover(&sh, &self.content, status, settings);
        }
    }
}

/// The sparkle mark (the web page's ✦), drawn in the text colour so it
/// follows the style, and needing no icon file or font.
fn sparkle() -> gtk::DrawingArea {
    let area = gtk::DrawingArea::builder()
        .content_width(16)
        .content_height(16)
        .valign(gtk::Align::Center)
        .build();
    area.set_draw_func(|area, cr, w, h| {
        let c = area.color();
        cr.set_source_rgba(
            f64::from(c.red()),
            f64::from(c.green()),
            f64::from(c.blue()),
            f64::from(c.alpha()),
        );
        let s = f64::from(w.min(h)) / 16.0;
        // Four-pointed stars whose sides curve in towards the centre: each
        // side bends round a point a fifth of the way out between its two
        // tips, which keeps the arms full enough to read at 16 pixels.
        let star = |cx: f64, cy: f64, r: f64| {
            let k = 0.2 * r;
            cr.move_to(cx * s, (cy - r) * s);
            for ((px, py), (qx, qy)) in [
                ((cx + r, cy), (cx + k, cy - k)),
                ((cx, cy + r), (cx + k, cy + k)),
                ((cx - r, cy), (cx - k, cy + k)),
                ((cx, cy - r), (cx - k, cy - k)),
            ] {
                cr.curve_to(qx * s, qy * s, qx * s, qy * s, px * s, py * s);
            }
            cr.close_path();
        };
        star(6.5, 9.5, 6.5);
        star(13.0, 3.0, 3.0);
        let _ = cr.fill();
    });
    area
}

fn heading(text: &str) -> gtk::Label {
    let l = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .build();
    l.add_css_class("heading");
    l
}

fn body(text: &str) -> gtk::Label {
    let l = gtk::Label::builder()
        .label(text)
        .xalign(0.0)
        .wrap(true)
        .max_width_chars(36)
        .build();
    l.add_css_class("dim-label");
    l
}

/// The popover: what is going on, and the way to change it.
fn fill_popover(sh: &Rc<Shared>, content: &gtk::Box, status: &AgentStatus, settings: &Settings) {
    while let Some(c) = content.first_child() {
        content.remove(&c);
    }
    if !settings.allowed {
        content.append(&heading("Connect your AI agent"));
        content.append(&body(
            "Let Claude Code, Cursor, VS Code and other AI agents read and edit the \
             models open here, and see the 3D view.",
        ));
    } else if status.clients.is_empty() {
        content.append(&heading("No agent connected"));
        content.append(&body(
            "An agent set up with NeoSCAD connects by itself when you ask it about your model.",
        ));
        if let Some(e) = &status.error {
            content.append(&body(&format!("NeoSCAD could not listen for agents: {e}")));
        }
    } else {
        content.append(&heading(&client::agent::status_line(status)));
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build();
        list.add_css_class("boxed-list");
        for c in &status.clients {
            list.append(&client_row(sh, c));
        }
        content.append(&list);
    }
    let set_up = gtk::Button::with_label(if settings.allowed {
        "Set Up Agents…"
    } else {
        "Set Up…"
    });
    if !settings.allowed {
        set_up.add_css_class("suggested-action");
    }
    let weak = Rc::downgrade(sh);
    let c = content.clone();
    set_up.connect_clicked(move |_| {
        if let Some(sh) = weak.upgrade() {
            if let Some(p) = c
                .ancestor(gtk::Popover::static_type())
                .and_downcast::<gtk::Popover>()
            {
                p.popdown();
            }
            let parent = c.root().and_downcast::<gtk::Window>();
            show_dialog(&sh, parent.as_ref());
        }
    });
    content.append(&set_up);
}

/// A connected agent: its name (its own claim, so no "verified" look),
/// what it is doing, and Disconnect.
fn client_row(sh: &Rc<Shared>, c: &client::agent::AgentClient) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(&logic::client_name(c)))
        .subtitle(c.activity.as_deref().unwrap_or("connected"))
        .build();
    let disconnect = gtk::Button::builder()
        .label("Disconnect")
        .valign(gtk::Align::Center)
        .build();
    let (weak, id) = (Rc::downgrade(sh), c.id);
    disconnect.connect_clicked(move |_| {
        if let Some(sh) = weak.upgrade() {
            glib::g_debug!("neoscad", "agent: disconnect {id}");
            sh.agents.link.disconnect(id);
        }
    });
    row.add_suffix(&disconnect);
    row
}

// --- The Agents page -----------------------------------------------------------

/// Main menu > Connect AI Agent…, and the popover's Set Up: the Agents
/// page in a dialog of its own.
pub fn show_dialog(sh: &Rc<Shared>, parent: Option<&gtk::Window>) {
    let dialog = adw::PreferencesDialog::new();
    dialog.set_title("Connect Your AI Agent");
    dialog.set_search_enabled(false);
    dialog.add(&page(sh));
    dialog.present(parent);
}

/// The Agents page: consent, who is connected, the chosen client's setup,
/// and how to work with the agent once it is set up. Shown
/// on its own (`show_dialog`) and in Preferences.
pub fn page(sh: &Rc<Shared>) -> adw::PreferencesPage {
    let p = Rc::new(AgentPage::new(sh));
    sh.agents.pages.borrow_mut().push(Rc::downgrade(&p));
    p.show(sh, &sh.agents.status(), &sh.agents.settings());
    // The page owns its state: dropped with the page's widget.
    let page = p.page.clone();
    let keep = RefCell::new(Some(p));
    page.connect_destroy(move |_| {
        keep.borrow_mut().take();
    });
    page
}

pub struct AgentPage {
    page: adw::PreferencesPage,
    allow: adw::SwitchRow,
    ask: adw::SwitchRow,
    connected: adw::PreferencesGroup,
    connected_rows: RefCell<Vec<gtk::Widget>>,
}

impl std::fmt::Debug for AgentPage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentPage").finish_non_exhaustive()
    }
}

impl AgentPage {
    fn new(sh: &Rc<Shared>) -> AgentPage {
        let page = adw::PreferencesPage::builder()
            .title("Agents")
            .icon_name("avatar-default-symbolic")
            .name("agents")
            .build();

        let access = adw::PreferencesGroup::builder()
            .title("AI Agents")
            .description(
                "Agents on this computer that use NeoSCAD (Claude Code, Cursor, VS Code \
                 and others) can read and edit the models open here and see the 3D view. \
                 What they read goes to the agent's AI service.",
            )
            .build();
        let allow = adw::SwitchRow::builder()
            .title("Allow AI agents to work on open documents")
            .build();
        let ask = adw::SwitchRow::builder()
            .title("Ask before applying an agent's edits")
            .subtitle("Either way, an agent's edit is highlighted, and one Undo takes it back")
            .build();
        access.add(&allow);
        access.add(&ask);
        page.add(&access);

        let connected = adw::PreferencesGroup::builder()
            .title("Connected Agents")
            .build();
        page.add(&connected);

        let (set_up, usage) = client_groups(sh);
        page.add(&set_up);
        page.add(&usage);

        let p = AgentPage {
            page,
            allow,
            ask,
            connected,
            connected_rows: RefCell::default(),
        };
        // `show` sets the switches too: each handler acts only on a value
        // that differs from the settings, so only the user's flips count.
        let weak = Rc::downgrade(sh);
        p.allow.connect_active_notify({
            let weak = weak.clone();
            move |row| {
                let Some(sh) = weak.upgrade() else { return };
                if row.is_active() != sh.agents.settings().allowed {
                    set_allowed(&sh, row.is_active());
                }
            }
        });
        p.ask.connect_active_notify(move |row| {
            let Some(sh) = weak.upgrade() else { return };
            if row.is_active() != sh.agents.settings().ask_first {
                sh.agents.change(|s| s.ask_first = row.is_active());
                refresh(&sh);
            }
        });
        p
    }

    fn show(&self, sh: &Rc<Shared>, status: &AgentStatus, settings: &Settings) {
        self.allow.set_active(settings.allowed);
        self.ask.set_active(settings.ask_first);
        self.ask.set_sensitive(settings.allowed);
        for r in self.connected_rows.borrow_mut().drain(..) {
            self.connected.remove(&r);
        }
        self.connected.set_visible(settings.allowed);
        let mut rows: Vec<gtk::Widget> = Vec::new();
        if status.clients.is_empty() {
            let (title, subtitle) = match &status.error {
                Some(e) => ("NeoSCAD could not listen for agents".to_string(), e.clone()),
                None => (
                    "No agent connected".to_string(),
                    "Set one up below. It connects by itself when you ask it about your \
                     model, for example: “Make the teeth smaller and show me the result.”"
                        .to_string(),
                ),
            };
            let row = adw::ActionRow::builder()
                .title(title)
                .subtitle(glib::markup_escape_text(&subtitle))
                .build();
            rows.push(row.upcast());
        } else {
            for c in &status.clients {
                rows.push(client_row(sh, c).upcast());
            }
        }
        for r in &rows {
            self.connected.add(r);
        }
        *self.connected_rows.borrow_mut() = rows;
    }
}

/// "Set Up an Agent" and "Using NeoSCAD with your agent".
///
/// The clients are a row of linked toggle buttons (GNOME's segmented
/// control; `AdwToggleGroup` needs libadwaita 1.7, and the app targets
/// 1.5), and only the chosen client's setup shows below them: four
/// expandable rows at once made the page a wall of buttons and JSON, most
/// of it for clients the user does not have. The page opens on the client
/// picked last (kept in agents.json), else Claude Code. The usage section
/// under it is the core's text for that client (`client::agent_setup::
/// usage`, shared with the macOS and Windows apps), so it follows the pick.
fn client_groups(sh: &Rc<Shared>) -> (adw::PreferencesGroup, adw::PreferencesGroup) {
    let a = &sh.agents;
    let description = if a.host == setup::Host::LinuxFlatpak {
        "NeoSCAD runs in a Flatpak sandbox, so agents start its command-line tool with \
         flatpak run. It reaches your home folder only."
            .to_string()
    } else if let Some(cli) = &a.cli {
        format!("Agents run NeoSCAD's command-line tool, {}.", cli.display())
    } else {
        "NeoSCAD's command-line tool (neoscad) was not found. Install it from \
         neoscad.org/download (install.sh, .deb or .rpm), then open this again."
            .to_string()
    };
    let group = adw::PreferencesGroup::builder()
        .title("Set Up an Agent")
        .build();
    // Which command the setups name is a footnote under the setup, as on
    // macOS, not the group's description: above the selector, its length
    // (a long path wraps) would move the selector up and down between
    // machines.
    let note = gtk::Label::builder()
        .label(&description)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .margin_top(12)
        .build();
    note.add_css_class("dim-label");
    note.add_css_class("caption");
    // Claude Desktop has no Linux build: `setups` leaves it out here.
    let rows = setup::setups(a.host, &a.server(), &machine::home(), None);
    let offers: Vec<setup::Client> = rows.iter().map(|r| r.client).collect();
    let chosen = logic::chosen_client(a.settings().setup_client, &offers);
    glib::g_debug!(
        "neoscad",
        "agent: setup page opens on {}",
        client_key(chosen)
    );

    // A widget that is not a row goes below the group's list, in the order
    // added: the selector, then the chosen client's row in a list of its
    // own, so the two read as one block under the group's title, then the
    // note.
    let picker = gtk::Box::builder()
        .orientation(gtk::Orientation::Horizontal)
        .homogeneous(true)
        .build();
    picker.add_css_class("linked");
    picker.update_property(&[gtk::accessible::Property::Label("Agent")]);
    let stack = gtk::Stack::builder()
        .vhomogeneous(false)
        .margin_top(12)
        .build();
    let mut first: Option<gtk::ToggleButton> = None;
    let mut buttons = Vec::new();
    for r in rows {
        let client = r.client;
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .build();
        list.add_css_class("boxed-list");
        list.append(&setup_row(sh, r));
        stack.add_named(&list, Some(client_key(client)));
        let button = gtk::ToggleButton::builder()
            .label(client.short_label())
            .name(format!("agent-client-{}", client_key(client)))
            .build();
        button.set_group(first.as_ref());
        first.get_or_insert_with(|| button.clone());
        picker.append(&button);
        buttons.push((client, button));
    }
    group.add(&picker);
    group.add(&stack);
    group.add(&note);

    let usage = adw::PreferencesGroup::builder()
        .title("Using NeoSCAD with your agent")
        .build();
    let learn = gtk::LinkButton::with_label(logic::LEARN_MORE_URL, "Learn more");
    learn.set_valign(gtk::Align::Center);
    usage.set_header_suffix(Some(&learn));
    let usage_rows: Rc<RefCell<Vec<gtk::Widget>>> = Rc::default();

    // The chosen client before the handlers, so opening the page does not
    // count as a pick (only the user's click is kept).
    stack.set_visible_child_name(client_key(chosen));
    fill_usage(sh, &usage, &usage_rows, chosen);
    for (client, button) in &buttons {
        button.set_active(*client == chosen);
        let (weak, stack, usage, usage_rows, client) = (
            Rc::downgrade(sh),
            stack.clone(),
            usage.downgrade(),
            usage_rows.clone(),
            *client,
        );
        button.connect_toggled(move |b| {
            // The group's other button turning off is not a pick.
            let (true, Some(sh), Some(usage)) = (b.is_active(), weak.upgrade(), usage.upgrade())
            else {
                return;
            };
            glib::g_debug!("neoscad", "agent: setup for {}", client_key(client));
            stack.set_visible_child_name(client_key(client));
            fill_usage(&sh, &usage, &usage_rows, client);
            sh.agents.change(|s| s.setup_client = Some(client));
        });
    }
    (group, usage)
}

/// The client's name in the stack and the buttons' widget names (GTK's
/// inspector finds them by it): the core's own spelling, as agents.json
/// keeps it.
fn client_key(c: setup::Client) -> &'static str {
    match c {
        setup::Client::ClaudeCode => "claude-code",
        setup::Client::ClaudeDesktop => "claude-desktop",
        setup::Client::Cursor => "cursor",
        setup::Client::VsCode => "vs-code",
        setup::Client::Other => "other",
    }
}

/// "Using NeoSCAD with your agent" for `client`: a row per item, its body
/// as the subtitle, all showing at once so the section can be scanned.
/// "Things to ask" opens onto its example requests, each with a copy
/// button, since pasting one into the agent is the quickest first try.
fn fill_usage(
    sh: &Rc<Shared>,
    group: &adw::PreferencesGroup,
    rows: &RefCell<Vec<gtk::Widget>>,
    client: setup::Client,
) {
    for r in rows.borrow_mut().drain(..) {
        group.remove(&r);
    }
    let mut added: Vec<gtk::Widget> = Vec::new();
    for item in logic::usage_rows(sh.agents.host, client) {
        let icon = gtk::Image::from_icon_name(item.icon);
        let title = glib::markup_escape_text(&item.title);
        let body = glib::markup_escape_text(&item.body);
        if item.examples.is_empty() {
            let row = adw::ActionRow::builder()
                .title(title)
                .subtitle(body)
                .build();
            row.add_prefix(&icon);
            added.push(row.upcast());
            continue;
        }
        let row = adw::ExpanderRow::builder()
            .title(title)
            .subtitle(body)
            .expanded(true)
            .build();
        row.add_prefix(&icon);
        for e in item.examples {
            let example = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&format!("“{e}”")))
                .build();
            let copy = gtk::Button::builder()
                .icon_name("edit-copy-symbolic")
                .tooltip_text("Copy")
                .valign(gtk::Align::Center)
                .build();
            copy.add_css_class("flat");
            let weak = Rc::downgrade(sh);
            copy.connect_clicked(move |b| {
                b.clipboard().set_text(&e);
                if let Some(sh) = weak.upgrade() {
                    tell(&sh, "Copied");
                }
            });
            example.add_suffix(&copy);
            row.add_row(&example);
        }
        added.push(row.upcast());
    }
    for r in &added {
        group.add(r);
    }
    *rows.borrow_mut() = added;
}

fn setup_row(sh: &Rc<Shared>, r: ClientSetup) -> adw::ExpanderRow {
    let row = adw::ExpanderRow::builder()
        .title(&r.label)
        .subtitle(glib::markup_escape_text(&r.note))
        .build();
    let label = match &r.action {
        SetupAction::RunClaude { .. } => Some("Add"),
        SetupAction::OpenUrl { .. } => Some(match r.client {
            setup::Client::Cursor => "Open Cursor",
            setup::Client::VsCode => "Open VS Code",
            _ => "Open",
        }),
        // Claude Desktop does not run on Linux; `setups` leaves it out.
        SetupAction::MergeConfig { .. } | SetupAction::CopyOnly => None,
    };
    if let Some(label) = label {
        let button = gtk::Button::builder()
            .label(label)
            .valign(gtk::Align::Center)
            .sensitive(sh.agents.has_cli())
            .build();
        let (weak, row2, action) = (Rc::downgrade(sh), row.downgrade(), r.action.clone());
        button.connect_clicked(move |b| {
            let (Some(sh), Some(row)) = (weak.upgrade(), row2.upgrade()) else {
                return;
            };
            let action = action.clone();
            let b = b.clone();
            with_consent(&sh, &row, move |sh, row| match &action {
                SetupAction::RunClaude { .. } => add_to_claude_code(sh, row, &b, false),
                SetupAction::OpenUrl { url } => open_link(sh, row, url),
                _ => {}
            });
        });
        row.add_suffix(&button);
    }
    // Nothing to click (Other, and Claude Code in the Flatpak, which runs
    // nothing on the host): the command or JSON is the row's whole
    // content, so show it, rather than a closed row that seems to do
    // nothing now that it is the only row under the selector.
    if r.action == SetupAction::CopyOnly {
        row.set_expanded(true);
    }
    // The command or JSON, to copy: always there, for any client and for
    // when the one-click way fails.
    let text = gtk::Label::builder()
        .label(&r.copy_text)
        .xalign(0.0)
        .wrap(true)
        .wrap_mode(gtk::pango::WrapMode::WordChar)
        .selectable(true)
        .hexpand(true)
        .build();
    text.add_css_class("monospace");
    let copy = gtk::Button::builder()
        .icon_name("edit-copy-symbolic")
        .tooltip_text("Copy")
        .valign(gtk::Align::Center)
        .build();
    copy.add_css_class("flat");
    let (weak, copied) = (Rc::downgrade(sh), r.copy_text.clone());
    copy.connect_clicked(move |b| {
        b.clipboard().set_text(&copied);
        if let Some(sh) = weak.upgrade() {
            tell(&sh, "Copied");
        }
    });
    let line = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    line.add_css_class("agent-copy");
    line.append(&text);
    line.append(&copy);
    row.add_row(&line);
    row
}

/// Setting up a client while agents are not allowed would leave it unable
/// to reach the open models, which is the point of setting it up here: ask
/// once, with the consent's own words. Either answer goes on with the
/// setup (the client's file tools work without the app).
fn with_consent(
    sh: &Rc<Shared>,
    row: &adw::ExpanderRow,
    then: impl FnOnce(&Rc<Shared>, &adw::ExpanderRow) + 'static,
) {
    if sh.agents.settings().allowed {
        then(sh, row);
        return;
    }
    let dialog = adw::AlertDialog::new(
        Some("Let AI agents work on your open models?"),
        Some(
            "Agents on this computer that use NeoSCAD (Claude Code, Cursor, VS Code and \
             others) will be able to read and edit the models open in NeoSCAD and see the \
             3D view. What they read goes to the agent's AI service. You can turn this off \
             in Preferences.",
        ),
    );
    dialog.add_responses(&[("later", "_Not Now"), ("allow", "_Allow")]);
    dialog.set_response_appearance("allow", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("allow"));
    dialog.set_close_response("later");
    let (weak, r) = (Rc::downgrade(sh), row.downgrade());
    let then = RefCell::new(Some(then));
    dialog.connect_response(None, move |_, response| {
        let (Some(sh), Some(row)) = (weak.upgrade(), r.upgrade()) else {
            return;
        };
        if response == "allow" {
            set_allowed(&sh, true);
        }
        if let Some(then) = then.borrow_mut().take() {
            then(&sh, &row);
        }
    });
    dialog.present(Some(row));
}

/// The Claude Code row's Add: find `claude`, run `claude mcp add`, and
/// say what happened; off the main loop, as `claude` takes a moment.
fn add_to_claude_code(
    sh: &Rc<Shared>,
    row: &adw::ExpanderRow,
    button: &gtk::Button,
    replace: bool,
) {
    button.set_sensitive(false);
    row.set_subtitle("Adding to Claude Code…");
    let server = sh.agents.server();
    let (weak, row, button) = (Rc::downgrade(sh), row.downgrade(), button.downgrade());
    glib::spawn_future_local(async move {
        let outcome = gio::spawn_blocking(move || {
            machine::find_claude().map(|claude| {
                machine::add_to_claude_code(std::path::Path::new(&claude), &server, replace)
            })
        })
        .await
        .ok()
        .flatten();
        let (Some(sh), Some(row), Some(button)) = (weak.upgrade(), row.upgrade(), button.upgrade())
        else {
            return;
        };
        button.set_sensitive(true);
        match outcome {
            None => {
                row.set_subtitle("Claude Code was not found. Run this command in a terminal:");
                row.set_expanded(true);
            }
            Some(machine::ClaudeCodeOutcome::Added { .. }) => {
                glib::g_debug!("neoscad", "agent: added to Claude Code");
                row.set_subtitle(
                    "Added for all your projects. Start a new Claude Code session to use it.",
                );
                tell(&sh, "Added NeoSCAD to Claude Code");
            }
            Some(machine::ClaudeCodeOutcome::AlreadyExists { .. }) => {
                row.set_subtitle("Claude Code already has a server named neoscad.");
                ask_replace(&sh, &row, &button);
            }
            Some(machine::ClaudeCodeOutcome::Failed { output }) => {
                let first = output.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
                row.set_subtitle(&glib::markup_escape_text(&format!(
                    "Claude Code could not add it ({first}). Run this command in a terminal:"
                )));
                row.set_expanded(true);
            }
        }
    });
}

fn ask_replace(sh: &Rc<Shared>, row: &adw::ExpanderRow, button: &gtk::Button) {
    let dialog = adw::AlertDialog::new(
        Some("Replace NeoSCAD in Claude Code?"),
        Some(
            "Claude Code already has a server named “neoscad”, which may run another \
             copy of NeoSCAD. Replace it with this one, for all your projects?",
        ),
    );
    dialog.add_responses(&[("cancel", "_Cancel"), ("replace", "_Replace")]);
    dialog.set_response_appearance("replace", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("replace"));
    dialog.set_close_response("cancel");
    let (weak, r, b) = (Rc::downgrade(sh), row.downgrade(), button.downgrade());
    dialog.connect_response(None, move |_, response| {
        if let (Some(sh), Some(row), Some(button), "replace") =
            (weak.upgrade(), r.upgrade(), b.upgrade(), response)
        {
            add_to_claude_code(&sh, &row, &button, true);
        }
    });
    dialog.present(Some(row));
}

/// Cursor's or VS Code's install link, through the OpenURI portal in the
/// Flatpak: the client asks the user, and owns its config.
fn open_link(sh: &Rc<Shared>, row: &adw::ExpanderRow, url: &str) {
    let parent = row.root().and_downcast::<gtk::Window>();
    let (weak, row) = (Rc::downgrade(sh), row.downgrade());
    gtk::UriLauncher::new(url).launch(parent.as_ref(), gio::Cancellable::NONE, move |r| {
        let (Some(sh), Some(row)) = (weak.upgrade(), row.upgrade()) else {
            return;
        };
        match r {
            Ok(()) => {
                row.set_subtitle("Finish in the app that opened: it asks you to install NeoSCAD.")
            }
            Err(e) => {
                glib::g_warning!("neoscad", "agent: could not open the install link: {e}");
                row.set_subtitle(
                    "It did not open. Is it installed? Add this to its MCP settings instead:",
                );
                row.set_expanded(true);
                tell(&sh, "Could not open the link");
            }
        }
    });
}
