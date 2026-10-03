//! A document window: the header bar, the editor, the 3D view and the
//! console, and the document loop that ties them (`client::DocumentLoop`:
//! the host keeps one GLib timer and passes "now"; the loop decides when
//! to run, what to send the core, and whether a result is current).
//!
//! Each window's editor has a language server of its own
//! (`linux_app::language`), whose markers come from the window's runs.
//!
//! The side panels (customizer, check, measure) share an
//! `AdwOverlaySplitView` at the window's end, toggled from the header bar
//! (F9) or opened on one panel (Alt+1, Alt+2, Alt+3). The files the last
//! run read are watched (`GFileMonitor` per directory, `linux_app::watch`)
//! and a change runs the document again. The document's own file is
//! watched too: another program's change to it is taken in or reported
//! (`disk`).

mod disk;

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};
use serde_json::{Value, json};
use webkit6::prelude::WebViewExt;

use client::{
    CheckReport, CoreError, DocumentLoop, ExportFormatInfo, ExportKind, Measurement, OverlayState,
    ParameterGroup, PrinterSettings, RenderMode, RunOptions, SourceRange,
};
use linux_app::bridge::{self, ChangeOutcome, EditorBridge, Incoming};
use linux_app::document::{self, Document};
use linux_app::language::{self, Language};
use linux_app::run::{self, FileView};
use linux_app::view::ViewCanvas;
use linux_app::watch::WatchSet;
use linux_app::{customizer, inspect};

use super::Shared;
use super::console::Console;
use super::customizer::{self as customizer_panel, Customizer};
use super::editor;
use super::inspect::{CheckPanel, MeasurePanel, Request as InspectRequest};
use super::viewport::ViewWidget;

/// The colour scheme pair of the macOS app (`ViewportController.swift`):
/// OpenSCAD's default for a light style, a dark scheme of its own for dark.
const LIGHT_SCHEME: &str = "Cornfield";
const DARK_SCHEME: &str = "Tomorrow Night";

pub struct Window {
    win: adw::ApplicationWindow,
    title: adw::WindowTitle,
    toasts: adw::ToastOverlay,
    /// "NeoSCAD x.y.z is available", under the header bar while the app
    /// knows of a newer release (`super::update`).
    banner: adw::Banner,
    /// "The file changed on disk", under it while the window's copy and
    /// the file disagree (`disk`).
    notice: disk::DiskNotice,
    web: Option<webkit6::WebView>,
    view: ViewWidget,
    console: Console,
    spinner: gtk::Spinner,
    /// The side panels and which one is shown.
    split: adw::OverlaySplitView,
    panels: gtk::Stack,
    customizer: Rc<Customizer>,
    check: Rc<CheckPanel>,
    measure: Rc<MeasurePanel>,
    shared: Rc<Shared>,
    st: RefCell<State>,
}

impl std::fmt::Debug for Window {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Window").finish_non_exhaustive()
    }
}

struct State {
    doc: Document,
    bridge: EditorBridge,
    lp: DocumentLoop,
    /// The loop's one timer, while a run is due.
    timer: Option<glib::SourceId>,
    /// The file's `$vp*` last applied: the view moves only when the file
    /// asks for a different one, not back after every keystroke.
    file_view: Option<FileView>,
    /// The editor's language server (none without the editor bundle).
    language: Option<Language>,
    /// A position to show once the editor has the text (a definition
    /// this window was opened for).
    reveal: Option<(u64, u64)>,

    /// The customizer's parameters of the latest run's text, the sets
    /// beside the document and the set shown (until a value is edited).
    groups: Vec<ParameterGroup>,
    set_names: Vec<String>,
    shown_set: Option<String>,
    /// What the check and measure panels draw in the view.
    overlay: OverlayState,
    printer: PrinterSettings,
    check_stop: Option<Arc<AtomicBool>>,
    measurement: Option<Measurement>,
    measure_stop: Option<Arc<AtomicBool>>,
    picking: bool,
    /// The files the last run read, their directories' monitors, and the
    /// pause that gathers a burst of changes (an editor's save is several
    /// events) into one run.
    watch: WatchSet,
    monitors: Vec<gio::FileMonitor>,
    watch_timer: Option<glib::SourceId>,
    /// The last run's files (watched with the document's own), and the
    /// pause before the document's file is read after it changed.
    deps: Vec<PathBuf>,
    disk_timer: Option<glib::SourceId>,
    /// The export running, File > Export's last format, and the last
    /// run's dimension (Export Again suggests a format that fits it).
    export_stop: Option<Arc<AtomicBool>>,
    last_export: String,
    last_dimensions: Option<u32>,
}

/// A window action's handler.
type Action = fn(&Rc<Window>);

fn scheme_for(dark: bool) -> render::ColorScheme {
    render::scheme::find(if dark { DARK_SCHEME } else { LIGHT_SCHEME })
        .unwrap_or_else(render::ColorScheme::cornfield)
}

impl Window {
    pub fn new(app: &adw::Application, shared: Rc<Shared>, doc: Document) -> Rc<Window> {
        let style = adw::StyleManager::default();
        let canvas = linux_app::host::gpu()
            .and_then(|gpu| ViewCanvas::new(gpu, scheme_for(style.is_dark())));
        let view = ViewWidget::new(canvas);

        let title = adw::WindowTitle::new("", "");
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        let open = gtk::Button::builder()
            .label("Open")
            .action_name("app.open")
            .tooltip_text("Open a file (Ctrl+O)")
            .build();
        header.pack_start(&open);
        header.pack_start(
            &gtk::Button::builder()
                .icon_name("list-add-symbolic")
                .action_name("app.new-window")
                .tooltip_text("New window (Ctrl+N)")
                .build(),
        );
        header.pack_end(
            &gtk::MenuButton::builder()
                .icon_name("open-menu-symbolic")
                .menu_model(&super::main_menu())
                .primary(true)
                .tooltip_text("Main menu")
                .build(),
        );
        let panel_toggle = gtk::ToggleButton::builder()
            .icon_name("sidebar-show-right-symbolic")
            .tooltip_text("Customizer, check and measure (F9)")
            .build();
        header.pack_end(&panel_toggle);
        let render = gtk::Button::builder()
            .label("Render")
            .action_name("win.render")
            .tooltip_text("Render the full geometry (F6)")
            .build();
        header.pack_end(&render);
        header.pack_end(
            &gtk::Button::builder()
                .icon_name("media-playback-start-symbolic")
                .action_name("win.preview")
                .tooltip_text("Preview (F5)")
                .build(),
        );
        let spinner = gtk::Spinner::new();
        header.pack_end(&spinner);

        let shared2 = shared.clone();
        Rc::new_cyclic(|me: &Weak<Window>| {
            let web = shared2.editor_dir.as_ref().map(|_| {
                let me = me.clone();
                editor::web_view(move |m| {
                    if let Some(w) = me.upgrade() {
                        w.on_message(&m);
                    }
                })
            });
            let editor_pane: gtk::Widget = match &web {
                Some(w) => w.clone().upcast(),
                None => adw::StatusPage::builder()
                    .icon_name("text-editor-symbolic")
                    .title("No editor")
                    .description(
                        "The editor bundle was not found. Build it with scripts/apple/build-editor.sh, \
                         or point NEOSCAD_EDITOR_DIR at it.",
                    )
                    .build()
                    .upcast(),
            };
            let me2 = me.clone();
            let console = Console::new(move |r| {
                if let Some(w) = me2.upgrade() {
                    w.reveal(r);
                }
            });
            let right = gtk::Paned::builder()
                .orientation(gtk::Orientation::Vertical)
                .start_child(&view.root)
                .end_child(&console.root)
                .resize_end_child(false)
                .shrink_end_child(false)
                .position(520)
                .build();
            let split = gtk::Paned::builder()
                .orientation(gtk::Orientation::Horizontal)
                .start_child(&editor_pane)
                .end_child(&right)
                .shrink_start_child(false)
                .shrink_end_child(false)
                .position(560)
                .build();
            let (side, panels, customizer, check, measure) = side_panels(me);
            let split_view = adw::OverlaySplitView::builder()
                .content(&split)
                .sidebar(&side)
                .sidebar_position(gtk::PackType::End)
                .show_sidebar(false)
                .min_sidebar_width(300.0)
                .max_sidebar_width(420.0)
                .sidebar_width_fraction(0.3)
                .build();
            split_view
                .bind_property("show-sidebar", &panel_toggle, "active")
                .bidirectional()
                .sync_create()
                .build();
            let toasts = adw::ToastOverlay::new();
            toasts.set_child(Some(&split_view));
            let banner = adw::Banner::builder()
                .button_label("Details")
                .revealed(false)
                .build();
            let notice = disk::DiskNotice::new();
            let toolbar = adw::ToolbarView::new();
            toolbar.add_top_bar(&header);
            toolbar.add_top_bar(&banner);
            toolbar.add_top_bar(&notice.revealer);
            toolbar.set_content(Some(&toasts));
            let win = adw::ApplicationWindow::builder()
                .application(app)
                .default_width(1280)
                .default_height(800)
                .content(&toolbar)
                .build();
            win.add_controller(super::shortcuts());
            let path = doc.core_path();
            let mut lp = DocumentLoop::new(client::DEFAULT_PREVIEW_DELAY_MS);
            lp.set_path(&path);
            let language = web.as_ref().map(|_| start_language(&shared2, me.clone()));
            Window {
                win,
                title,
                toasts,
                banner,
                notice,
                web,
                view,
                console,
                spinner,
                split: split_view,
                panels,
                customizer,
                check,
                measure,
                shared: shared2,
                st: RefCell::new(State {
                    doc,
                    bridge: EditorBridge::default(),
                    lp,
                    timer: None,
                    file_view: None,
                    language,
                    reveal: None,
                    groups: Vec::new(),
                    set_names: Vec::new(),
                    shown_set: None,
                    overlay: OverlayState::default(),
                    printer: PrinterSettings::default(),
                    check_stop: None,
                    measurement: None,
                    measure_stop: None,
                    picking: false,
                    watch: WatchSet::default(),
                    monitors: Vec::new(),
                    watch_timer: None,
                    deps: Vec::new(),
                    disk_timer: None,
                    export_stop: None,
                    last_export: "binstl".into(),
                    last_dimensions: None,
                }),
            }
        })
        .setup()
    }

    fn setup(self: Rc<Self>) -> Rc<Self> {
        let actions: [(&str, Action); 14] = [
            ("save", |w| w.save(None)),
            ("save-as", |w| w.save_as(None)),
            ("export-again", Window::export_again),
            ("preview", |w| w.run(RenderMode::Preview)),
            ("render", |w| w.run(RenderMode::Render)),
            ("view-all", |w| w.camera(|v| v.view_all())),
            ("reset-view", |w| w.camera(|v| v.reset_view())),
            ("toggle-panels", |w| {
                w.split.set_show_sidebar(!w.split.shows_sidebar());
            }),
            ("show-customizer", |w| w.show_panel("customizer")),
            ("show-check", |w| w.show_panel("check")),
            ("show-measure", |w| w.show_panel("measure")),
            ("check", |w| w.run_check()),
            ("measure", |w| w.run_measure()),
            ("reset-parameters", |w| {
                w.customize(customizer_panel::Request::Reset);
            }),
        ];
        for (name, f) in actions {
            let a = gio::SimpleAction::new(name, None);
            let me = Rc::downgrade(&self);
            a.connect_activate(move |_, _| {
                if let Some(w) = me.upgrade() {
                    f(&w);
                }
            });
            self.win.add_action(&a);
        }
        // File > Export's formats, by the core's id.
        let export = gio::SimpleAction::new("export", Some(glib::VariantTy::STRING));
        let me = Rc::downgrade(&self);
        export.connect_activate(move |_, p| {
            if let (Some(w), Some(id)) = (me.upgrade(), p.and_then(|p| p.str().map(str::to_string)))
            {
                w.export(&id);
            }
        });
        self.win.add_action(&export);
        let (me, shared) = (Rc::downgrade(&self), self.shared.clone());
        self.win.connect_destroy(move |_| shared.forget(&me));
        let me = Rc::downgrade(&self);
        self.banner.connect_button_clicked(move |_| {
            if let Some(w) = me.upgrade() {
                super::update::show_details(&w.shared, w.widget().upcast_ref());
            }
        });
        self.show_update(self.shared.updates.notice().as_ref());
        self.connect_disk_notice();
        // The document's own file is watched from the start, not only
        // after its first run (which an example may never get).
        self.disk_reset();
        // The web content process ended (a crash, or the system reclaimed
        // it). The document's copy of the text is complete, so reload the
        // page: `ready` shows the text again; only the undo history is
        // lost (as `EditorController.webViewWebContentProcessDidTerminate`).
        if let Some(web) = &self.web {
            let me = Rc::downgrade(&self);
            web.connect_web_process_terminated(move |web, reason| {
                glib::g_warning!(
                    "neoscad",
                    "the editor's web process ended ({reason:?}); reloading"
                );
                if let Some(w) = me.upgrade() {
                    let mut st = w.st.borrow_mut();
                    st.bridge.page_lost();
                    // The new page's client initializes again, and a
                    // server answers `initialize` once: start a new one.
                    if let Some(old) = st.language.take() {
                        old.stop();
                    }
                    st.language = Some(start_language(&w.shared, Rc::downgrade(&w)));
                }
                web.load_uri(linux_app::resources::PAGE_URL);
            });
        }
        let me = Rc::downgrade(&self);
        self.win.connect_close_request(move |_| match me.upgrade() {
            Some(w) => w.close_request(),
            None => glib::Propagation::Proceed,
        });
        // The style changed: the view takes the scheme pair's other half,
        // and the model is built again in it (its face colours come from
        // the scheme it was built in).
        let me = Rc::downgrade(&self);
        let style = adw::StyleManager::default();
        let handler = style.connect_dark_notify(move |sm| {
            if let Some(w) = me.upgrade() {
                if let Some(c) = w.view.canvas.borrow_mut().as_mut() {
                    c.viewport.set_scheme(scheme_for(sm.is_dark()));
                }
                let last = w.st.borrow().lp.last_mode();
                if let Some(m) = last {
                    w.run(m);
                }
            }
        });
        // The style manager outlives every window: a closed window's
        // handler would otherwise stay connected for the app's life.
        let handler = std::cell::Cell::new(Some(handler));
        self.win.connect_destroy(move |_| {
            if let Some(h) = handler.take() {
                style.disconnect(h);
            }
        });
        self.update_titles();
        self
    }

    pub fn widget(&self) -> &adw::ApplicationWindow {
        &self.win
    }

    pub fn core_path(&self) -> String {
        self.st.borrow().doc.core_path()
    }

    pub fn file(&self) -> Option<PathBuf> {
        self.st.borrow().doc.file().map(Path::to_path_buf)
    }

    pub fn is_replaceable(&self) -> bool {
        self.st.borrow().doc.is_replaceable()
    }

    pub fn set_parts(&self, on: bool) {
        self.st.borrow_mut().lp.set_parts(on);
    }

    pub fn toast(&self, message: &str) {
        self.toasts.add_toast(adw::Toast::new(message));
    }

    /// Show the banner for a newer release, or hide it.
    pub fn show_update(&self, notice: Option<&linux_app::update::Notice>) {
        if let Some(n) = notice {
            self.banner.set_title(&n.title());
        }
        self.banner.set_revealed(notice.is_some());
    }

    fn update_titles(&self) {
        let t = self.st.borrow().doc.titles();
        self.title.set_title(&t.title);
        self.title.set_subtitle(&t.subtitle);
        self.win.set_title(Some(&t.window));
    }

    /// Show `doc` in this window (a file opened, an example), and run it
    /// if `run`.
    pub fn replace_document(self: &Rc<Self>, doc: Document, run: bool) {
        let old = {
            let mut st = self.st.borrow_mut();
            st.doc = doc;
            st.file_view = None;
            let path = st.doc.core_path();
            let old = st.lp.set_path(&path);
            st.lp.text_replaced();
            // The panels described the old document: its edited values,
            // findings, solids and picks mean nothing for this one.
            st.lp.parameters_read(&[]);
            st.groups.clear();
            st.set_names.clear();
            st.shown_set = None;
            st.overlay = OverlayState::default();
            st.measurement = None;
            // The old document's files mean nothing for this one.
            st.deps.clear();
            old
        };
        self.disk_reset();
        self.show_parameters();
        self.update_overlay();
        if let Some(old) = old {
            let _ = self.shared.client.close(&old);
        }
        self.update_titles();
        self.load_editor();
        if run {
            self.schedule();
        }
    }

    // --- The editor ---------------------------------------------------------

    fn call(
        self: &Rc<Self>,
        function: &str,
        args: &[Value],
        done: impl FnOnce(&Rc<Window>, Value) + 'static,
    ) {
        let Some(web) = &self.web else { return };
        let me = Rc::downgrade(self);
        editor::call(web, function, args, move |v| {
            if let Some(w) = me.upgrade() {
                done(&w, v);
            }
        });
    }

    /// Show the document's text in the editor (which clears its undo
    /// history, as reading a file does), under the URI its language
    /// server knows it by.
    fn load_editor(self: &Rc<Self>) {
        let (text, uri) = {
            let mut st = self.st.borrow_mut();
            if !st.bridge.is_ready() {
                return; // `ready` loads the current text
            }
            st.bridge.load_sent();
            let uri = st
                .language
                .as_ref()
                .map(|_| language::document_uri(&st.doc.core_path()));
            (st.doc.text.text(), uri)
        };
        self.call(
            "load",
            &[json!(text), json!(uri), json!(false)],
            |w, state| {
                w.st.borrow_mut().bridge.history(&state);
                w.flush_reveal();
            },
        );
    }

    /// Put the cursor at a 0-based line and UTF-16 column (a definition
    /// this window was opened or brought forward for), now or once the
    /// editor has the text: a window just opened has not loaded it yet.
    pub fn reveal_at(self: &Rc<Self>, line: u64, character: u64) {
        self.st.borrow_mut().reveal = Some((line, character));
        self.flush_reveal();
    }

    fn flush_reveal(self: &Rc<Self>) {
        let at = {
            let mut st = self.st.borrow_mut();
            if st.bridge.version().is_none() {
                return; // the load's answer reveals it
            }
            st.reveal.take()
        };
        if let Some((line, character)) = at {
            self.call("reveal", &[json!(line), json!(character)], |_, _| {});
        }
    }

    /// A message from the language server for the page. Not gated on
    /// `ready`: the page's client sends `initialize` before the editor
    /// says ready, and dropping that answer makes its first request time
    /// out (the Windows app's bug, fixed in its `EditorHost`).
    fn deliver(self: &Rc<Self>, message: String) {
        let publication =
            self.shared.debug && message.contains("\"textDocument/publishDiagnostics\"");
        glib::g_debug!(
            "neoscad",
            "lsp: server -> page {}",
            language::describe(&message)
        );
        self.call("lspReceive", &[json!(message)], move |w, _| {
            // For the log (and linux/smoke.sh): what the page shows after
            // a publication, which proves the markers arrived, not only
            // that they were sent.
            if publication {
                w.call("state", &[], |_, s| {
                    glib::g_debug!(
                        "neoscad",
                        "editor: {} markers, language server {}",
                        s.get("diagnostics").and_then(Value::as_u64).unwrap_or(0),
                        if s.get("lsp") == Some(&json!(true)) {
                            "connected"
                        } else {
                            "not connected"
                        }
                    );
                });
            }
        });
    }

    fn on_message(self: &Rc<Self>, m: &Value) {
        let Some(msg) = bridge::parse(m) else { return };
        // Every message but the per-keystroke `changes`, for following
        // the bridge with G_MESSAGES_DEBUG=neoscad (`lsp` messages are
        // logged with what they carry, below).
        if !matches!(msg, Incoming::Changes(_) | Incoming::Lsp(_)) {
            glib::g_debug!(
                "neoscad",
                "editor: {}",
                m.get("type").and_then(Value::as_str).unwrap_or("")
            );
        }
        match msg {
            Incoming::Ready => {
                self.st.borrow_mut().bridge.page_ready();
                self.load_editor();
            }
            Incoming::Changes(c) => self.changes(c),
            Incoming::Command(name) => match name.as_str() {
                "preview" => self.run(RenderMode::Preview),
                "render" => self.run(RenderMode::Render),
                _ => {}
            },
            Incoming::Lsp(message) => {
                glib::g_debug!(
                    "neoscad",
                    "lsp: page -> server {}",
                    language::describe(&message)
                );
                let ls = self.st.borrow().language.clone();
                match ls {
                    Some(ls) => ls.send(message),
                    None => {
                        if let Some(reply) = language::not_found_reply(&message) {
                            self.deliver(reply);
                        }
                    }
                }
            }
            Incoming::Open {
                uri,
                line,
                character,
            } => {
                let app = self
                    .win
                    .application()
                    .and_then(|a| a.downcast::<adw::Application>().ok());
                if let Some(app) = app
                    && let Err(e) = super::open_location(&app, &self.shared, &uri, line, character)
                {
                    self.toast(&e);
                }
            }
            Incoming::Log { message } => glib::g_warning!("neoscad", "editor: {message}"),
            Incoming::Unknown(t) => glib::g_warning!("neoscad", "editor: unknown message {t}"),
        }
    }

    /// One editor transaction: into the document's copy, then the core's
    /// buffer, then a preview after the pause.
    fn changes(self: &Rc<Self>, c: bridge::Changes) {
        let outcome = {
            let mut st = self.st.borrow_mut();
            let State { bridge, doc, .. } = &mut *st;
            bridge.changes(c, &mut doc.text)
        };
        match outcome {
            ChangeOutcome::Ignored => {}
            ChangeOutcome::Applied { edits, kind } => {
                {
                    let mut st = self.st.borrow_mut();
                    st.doc.record(kind);
                    if st.lp.in_sync() {
                        let path = st.doc.core_path();
                        let want = st.doc.text.byte_length();
                        // The byte count is a cheap check that both copies
                        // agree; if not, the next run sends the whole text.
                        match self.shared.client.edit(&path, edits) {
                            Ok(info) if info.length == Some(want) => {}
                            _ => st.lp.text_replaced(),
                        }
                    }
                }
                self.check_reload_landed();
                self.update_titles();
                self.schedule();
            }
            ChangeOutcome::Resync => {
                self.call("text", &[], |w, answer| {
                    let text = w.st.borrow_mut().bridge.resynced(&answer);
                    if let Some(text) = text {
                        let mut st = w.st.borrow_mut();
                        st.doc.record_resync(text);
                        st.lp.text_replaced();
                        drop(st);
                        w.check_reload_landed();
                        w.update_titles();
                        w.schedule();
                    }
                });
            }
        }
    }

    fn reveal(self: &Rc<Self>, r: &SourceRange) {
        let args = [r.start_line, r.start_character, r.end_line, r.end_character].map(|n| json!(n));
        self.call("revealRange", &args, |_, _| {});
    }

    // --- The document loop --------------------------------------------------

    /// A preview after the pause (typing, a file replaced).
    fn schedule(self: &Rc<Self>) {
        let now = self.shared.now_ms();
        self.st.borrow_mut().lp.schedule(now);
        self.arm_timer();
    }

    /// (Re)start the one timer for the loop's next due run.
    fn arm_timer(self: &Rc<Self>) {
        let now = self.shared.now_ms();
        let mut st = self.st.borrow_mut();
        if let Some(t) = st.timer.take() {
            t.remove();
        }
        if let Some(due) = st.lp.next_due_ms() {
            let me = Rc::downgrade(self);
            st.timer = Some(glib::timeout_add_local_once(
                Duration::from_millis(due.saturating_sub(now)),
                move || {
                    if let Some(w) = me.upgrade() {
                        // This source is done: forget it without removing.
                        w.st.borrow_mut().timer = None;
                        w.tick();
                    }
                },
            ));
        }
    }

    fn tick(self: &Rc<Self>) {
        let now = self.shared.now_ms();
        let due = self.st.borrow_mut().lp.due(now);
        match due {
            Some(mode) => self.run(mode),
            None => self.arm_timer(),
        }
    }

    /// Run the document now in `mode`.
    fn run(self: &Rc<Self>, mode: RenderMode) {
        let client = self.shared.client.clone();
        let plan = {
            let mut st = self.st.borrow_mut();
            if let Some(t) = st.timer.take() {
                t.remove();
            }
            let path = st.doc.core_path();
            let Some(plan) = st.lp.begin_run(mode, &path) else {
                return;
            };
            if let Some(old) = &plan.close {
                // Saved under a new name: the old buffer would shadow the
                // file still on disk at the old path.
                let _ = client.close(old);
            }
            if plan.send_text {
                if let Err(e) = client.update(&path, st.doc.text.text()) {
                    drop(st);
                    self.console.set_summary(&e.to_string());
                    return;
                }
                st.lp.text_sent();
            }
            plan
        };
        let ls = self.st.borrow().language.clone();
        // The page's client sends its changes half a second after typing
        // stops; this run's markers are published for the version that
        // carries its text, so have the client send it now.
        if ls.is_some() && self.st.borrow().bridge.is_ready() {
            self.call("lspSync", &[], |_, _| {});
        }
        let (camera, scheme) = match self.view.canvas.borrow().as_ref() {
            Some(c) => (c.run_camera(), c.viewport.scheme().clone()),
            None => (
                eval::Camera {
                    vpt: [0.0; 3],
                    vpr: [55.0, 0.0, 25.0],
                    vpd: 140.0,
                    vpf: 22.5,
                    auto: false,
                    locked: false,
                },
                render::ColorScheme::cornfield(),
            ),
        };
        self.spinner.start();
        self.console.set_summary(match mode {
            RenderMode::Preview => "Previewing…",
            _ => "Rendering…",
        });
        let gpu = linux_app::host::gpu().ok();
        let me = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let out = gio::spawn_blocking(move || {
                run::run_document(&client, &plan, camera, &scheme, gpu.as_deref(), ls.as_ref())
            })
            .await;
            if let Some(w) = me.upgrade() {
                match out {
                    Ok(r) => w.finished(r),
                    Err(_) => w
                        .console
                        .set_summary("The run panicked (a bug in NeoSCAD)."),
                }
            }
        });
    }

    fn finished(self: &Rc<Self>, r: Result<run::RunOutput, CoreError>) {
        let out = match r {
            Ok(out) => out,
            // A newer run took over; it reports instead.
            Err(CoreError::Cancelled) => return,
            Err(e) => {
                self.spinner.stop();
                self.console.set_summary(&e.to_string());
                return;
            }
        };
        let (current, path) = {
            let st = self.st.borrow();
            (st.lp.is_current(out.generation), st.doc.core_path())
        };
        if !current {
            return;
        }
        self.spinner.stop();
        glib::g_debug!(
            "neoscad",
            "run {} ({:?}): {} ({} console lines)",
            out.generation,
            out.mode,
            out.summary,
            out.console.len()
        );
        {
            let mut st = self.st.borrow_mut();
            let mut canvas = self.view.canvas.borrow_mut();
            if let Some(c) = canvas.as_mut() {
                if let Some(model) = out.model {
                    c.viewport.set_model(model, out.generation);
                }
                if out.file_view.is_some() && out.file_view != st.file_view {
                    let (t, r, d, f) = out.file_view.unwrap_or_default();
                    c.viewport.set_file_view(t, r, d, f);
                }
            }
            if out.file_view.is_some() {
                st.file_view = out.file_view;
            }
        }
        self.view.queue();
        self.console.set_summary(&out.summary);
        self.console.set_lines(&out.console, &path);
        {
            let mut st = self.st.borrow_mut();
            st.last_dimensions = out
                .render
                .geometry
                .as_ref()
                .map(|g| u32::from(g.dimensions));
            // For the log (linux/smoke.sh): a customizer edit runs the
            // document with its values and leaves the text alone.
            glib::g_debug!(
                "neoscad",
                "document: {} customizer values, text {}",
                st.lp.overrides().len(),
                if st.doc.is_dirty() {
                    "edited"
                } else {
                    "unchanged"
                }
            );
        }
        self.watch_files(out.files);
        self.refresh_parameters();
    }

    fn camera(&self, f: impl FnOnce(&mut render::viewport::Viewport)) {
        if let Some(c) = self.view.canvas.borrow_mut().as_mut() {
            f(&mut c.viewport);
        }
    }

    // --- The side panels ----------------------------------------------------

    /// Show the side panel `name` and put the keyboard in it: its first
    /// control (the customizer's first parameter, the check's and the
    /// measure's button). Once the panel is on screen: a widget that is
    /// not yet mapped does not take the focus.
    fn show_panel(self: &Rc<Self>, name: &str) {
        self.split.set_show_sidebar(true);
        self.panels.set_visible_child_name(name);
        let (me, name) = (Rc::downgrade(self), name.to_string());
        glib::idle_add_local_once(move || {
            let Some(w) = me.upgrade() else { return };
            let focused = match name.as_str() {
                "customizer" => w.customizer.focus_first(),
                "check" => w.check.focus_run(),
                _ => w.measure.focus_run(),
            };
            glib::g_debug!("neoscad", "panels: {name} shown (focus {focused})");
        });
    }

    /// What a detached request (check, measure, export) runs with: the
    /// customizer's values and the parts toggle, as the document's runs.
    fn run_options(&self, st: &State) -> RunOptions {
        RunOptions {
            overrides: st.lp.overrides(),
            parts: st.lp.parts(),
            enable: Vec::new(),
        }
    }

    /// Set (or with `None` drop) one edited value, and run after the
    /// pause if it changed. `value` is `None` for a parameter the text no
    /// longer has.
    fn set_parameter(self: &Rc<Self>, name: &str, value: Option<Option<client::ParameterValue>>) {
        let Some(v) = value else { return };
        let now = self.shared.now_ms();
        let changed = {
            let mut st = self.st.borrow_mut();
            let changed = st.lp.set_parameter(name, v.clone(), now);
            if changed {
                glib::g_debug!("neoscad", "{}", customizer::describe_edit(name, v.as_ref()));
                st.shown_set = None;
            }
            changed
        };
        if changed {
            self.arm_timer();
        }
    }

    /// A customizer control's request.
    fn customize(self: &Rc<Self>, r: customizer_panel::Request) {
        use customizer_panel::Request as R;
        match r {
            R::Edit(name, edit) => {
                let value = {
                    let st = self.st.borrow();
                    customizer::apply_edit(&st.groups, &st.lp.overrides(), &name, edit)
                };
                self.set_parameter(&name, value);
            }
            R::Revert(name) => self.set_parameter(&name, Some(None)),
            R::Reset => {
                let now = self.shared.now_ms();
                let changed = {
                    let mut st = self.st.borrow_mut();
                    st.shown_set = None;
                    st.lp.reset_parameters(now)
                };
                if changed {
                    glib::g_debug!("neoscad", "customizer: every value back to the text's");
                    self.arm_timer();
                }
            }
            R::ApplySet(name) => self.apply_set(name),
            R::SaveSet => self.save_set(),
        }
        self.show_parameters();
    }

    /// Put the customizer in step with the window's state.
    fn show_parameters(&self) {
        let st = self.st.borrow();
        let overrides = st.lp.overrides();
        self.customizer.show(&st.groups, &overrides);
        self.customizer.show_sets(
            st.doc.file().is_some(),
            &st.set_names,
            st.shown_set.as_deref(),
            !st.groups.is_empty(),
        );
    }

    /// Read the parameters of the text the last run read (off the main
    /// thread: a long file takes a moment to parse), dropping edited
    /// values of parameters that are gone, and the sets beside the file.
    fn refresh_parameters(self: &Rc<Self>) {
        let client = self.shared.client.clone();
        let path = self.core_path();
        let me = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let p = path.clone();
            let r = gio::spawn_blocking(move || client.parameters(&p)).await;
            let (Some(w), Ok(Ok(groups))) = (me.upgrade(), r) else {
                return;
            };
            let file = {
                let mut st = w.st.borrow_mut();
                if st.doc.core_path() != path {
                    return; // another document since
                }
                st.lp.parameters_read(&groups);
                st.groups = groups;
                st.doc.file().map(Path::to_path_buf)
            };
            let names =
                customizer::set_names(&w.shared.client, file.as_deref()).unwrap_or_else(|e| {
                    glib::g_warning!("neoscad", "parameter sets: {e}");
                    Vec::new()
                });
            w.st.borrow_mut().set_names = names;
            w.show_parameters();
        });
    }

    /// Apply the parameter set `name` (OpenSCAD's `-p file -P name`).
    fn apply_set(self: &Rc<Self>, name: String) {
        let Some(file) = self.file() else { return };
        let path = match self.sync_core() {
            Ok(p) => p,
            Err(e) => return self.toast(&e.to_string()),
        };
        match customizer::apply_set(&self.shared.client, &path, &file, &name) {
            Ok(values) => {
                let now = self.shared.now_ms();
                {
                    let mut st = self.st.borrow_mut();
                    let groups = st.groups.clone();
                    st.lp.parameter_set_applied(&name, values, &groups, now);
                    glib::g_debug!("neoscad", "customizer: set {name} applied");
                    st.shown_set = Some(name);
                }
                self.arm_timer();
            }
            Err(e) => self.toast(&e),
        }
    }

    /// Ask for a name and save the edited values as that set.
    fn save_set(self: &Rc<Self>) {
        let Some(file) = self.file() else { return };
        let json = customizer::sets_file(&file);
        let suggested = {
            let st = self.st.borrow();
            customizer::suggested_set_name(st.shown_set.as_deref(), &st.set_names)
        };
        let me = Rc::downgrade(self);
        customizer_panel::ask_set_name(
            &self.win,
            &file_name(&json.to_string_lossy()),
            &suggested,
            move |name| {
                let Some(w) = me.upgrade() else { return };
                let path = match w.sync_core() {
                    Ok(p) => p,
                    Err(e) => return w.toast(&e.to_string()),
                };
                let values = w.st.borrow().lp.overrides();
                let client = w.shared.client.clone();
                match customizer::save_set(&client, &path, &file, &name, &values) {
                    Ok(_) => {
                        let names = customizer::set_names(&client, Some(&file)).unwrap_or_default();
                        {
                            let mut st = w.st.borrow_mut();
                            st.set_names = names;
                            st.shown_set = Some(name.clone());
                        }
                        w.show_parameters();
                        w.toast(&format!("Saved the parameter set “{name}”"));
                    }
                    Err(e) => w.toast(&e),
                }
            },
        );
    }

    /// A check or measure panel's request.
    fn inspect(self: &Rc<Self>, r: InspectRequest) {
        match r {
            InspectRequest::Check => self.run_check(),
            InspectRequest::Printer(id) => {
                self.st.borrow_mut().printer = if id == client::CUSTOM_PRINTER {
                    PrinterSettings::default()
                } else {
                    PrinterSettings::default().apply_preset(&id)
                };
            }
            InspectRequest::SelectFinding(id) => self.select_finding(id),
            InspectRequest::Measure => self.run_measure(),
            InspectRequest::Picking(on) => self.set_picking(on),
            InspectRequest::ClearPicks => {
                let picking = {
                    let mut st = self.st.borrow_mut();
                    st.overlay.picks.clear();
                    st.picking
                };
                self.measure.show_picks(&[], picking);
                self.update_overlay();
            }
        }
    }

    /// `check` on the text as it is now, with the customizer's values; a
    /// newer check stops this one.
    fn run_check(self: &Rc<Self>) {
        let path = match self.sync_core() {
            Ok(p) => p,
            Err(e) => return self.check.show_error(&e.to_string()),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let (options, printer) = {
            let mut st = self.st.borrow_mut();
            if let Some(old) = st.check_stop.replace(stop.clone()) {
                old.store(true, Ordering::Relaxed);
            }
            (self.run_options(&st), st.printer.clone())
        };
        self.check.set_running(true);
        let client = self.shared.client.clone();
        let me = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let s = stop.clone();
            let r = gio::spawn_blocking(move || {
                inspect::check(&client, &path, &options, &printer, Some(s))
            })
            .await;
            let Some(w) = me.upgrade() else { return };
            match r {
                // A newer check took over; it reports.
                Ok(Err(CoreError::Cancelled)) => {}
                Ok(Ok(report)) => w.check_done(report),
                Ok(Err(e)) => w.check.show_error(&e.to_string()),
                Err(_) => w.check.show_error("The check panicked (a bug in NeoSCAD)."),
            }
            let mut st = w.st.borrow_mut();
            if st
                .check_stop
                .as_ref()
                .is_some_and(|x| Arc::ptr_eq(x, &stop))
            {
                st.check_stop = None;
            }
        });
    }

    fn check_done(self: &Rc<Self>, report: CheckReport) {
        glib::g_debug!(
            "neoscad",
            "check: {} findings ({})",
            report.findings.len(),
            client::check_summary(&report)
        );
        let selected = {
            let mut st = self.st.borrow_mut();
            let selected = st
                .overlay
                .selected
                .filter(|s| report.findings.iter().any(|f| f.id == *s));
            st.overlay.findings = report.findings.clone();
            st.overlay.selected = selected;
            selected
        };
        self.check.show_report(&report, selected);
        self.update_overlay();
    }

    /// A finding activated: marked in the view (or unmarked, activated
    /// again), and the view turned to it.
    fn select_finding(self: &Rc<Self>, id: u32) {
        let (selected, point) = {
            let mut st = self.st.borrow_mut();
            let selected = inspect::toggle_selection(&st.overlay.findings, st.overlay.selected, id);
            st.overlay.selected = selected;
            let point = selected
                .and_then(|s| st.overlay.findings.iter().find(|f| f.id == s))
                .and_then(|f| client::point3(&f.point, "a finding").ok());
            (selected, point)
        };
        glib::g_debug!(
            "neoscad",
            "check: finding {id} {}",
            if selected.is_some() {
                "selected"
            } else {
                "cleared"
            }
        );
        if let Some(p) = point {
            self.camera(|v| v.look_at(p));
        }
        self.check.select(selected);
        self.update_overlay();
    }

    /// `measure` on the text as it is now; a newer measurement stops this
    /// one, and the picked points (on the old solids) are forgotten.
    fn run_measure(self: &Rc<Self>) {
        let path = match self.sync_core() {
            Ok(p) => p,
            Err(e) => return self.measure.show_error(&e.to_string()),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let options = {
            let mut st = self.st.borrow_mut();
            if let Some(old) = st.measure_stop.replace(stop.clone()) {
                old.store(true, Ordering::Relaxed);
            }
            self.run_options(&st)
        };
        self.measure.set_running(true);
        let client = self.shared.client.clone();
        let me = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let s = stop.clone();
            let r =
                gio::spawn_blocking(move || inspect::measure(&client, &path, &options, Some(s)))
                    .await;
            let Some(w) = me.upgrade() else { return };
            match r {
                Ok(Err(CoreError::Cancelled)) => {}
                Ok(Ok((report, measurement))) => {
                    let solid = report.model.is_some() && measurement.is_some();
                    glib::g_debug!(
                        "neoscad",
                        "measure: {}",
                        inspect::measure_text(&report).replace('\n', "; ")
                    );
                    let picking = {
                        let mut st = w.st.borrow_mut();
                        st.measurement = measurement;
                        st.overlay.picks.clear();
                        st.picking && solid
                    };
                    w.measure.show_report(&report, solid);
                    w.measure.show_picks(&[], picking);
                    w.update_overlay();
                }
                Ok(Err(e)) => w.measure.show_error(&e.to_string()),
                Err(_) => w
                    .measure
                    .show_error("The measurement panicked (a bug in NeoSCAD)."),
            }
            let mut st = w.st.borrow_mut();
            if st
                .measure_stop
                .as_ref()
                .is_some_and(|x| Arc::ptr_eq(x, &stop))
            {
                st.measure_stop = None;
            }
        });
    }

    /// Picking on: a click in the view picks the surface point under it.
    fn set_picking(self: &Rc<Self>, on: bool) {
        self.st.borrow_mut().picking = on;
        let me = Rc::downgrade(self);
        self.view.set_click_handler(on.then(|| {
            Box::new(move |x, y| {
                if let Some(w) = me.upgrade() {
                    w.pick(x, y);
                }
            }) as Box<dyn Fn(f64, f64)>
        }));
        let picks = self.st.borrow().overlay.picks.clone();
        self.measure.show_picks(&picks, on);
    }

    /// The surface point under `(x, y)` (points from the view's top
    /// left), if the ray meets the measured solid.
    fn pick(self: &Rc<Self>, x: f64, y: f64) {
        let ray = self
            .view
            .canvas
            .borrow()
            .as_ref()
            .and_then(|c| c.viewport.ray_at(x, y));
        let Some((origin, direction)) = ray else {
            return;
        };
        let picks = {
            let mut st = self.st.borrow_mut();
            let hit = st
                .measurement
                .as_ref()
                .and_then(|m| m.pick(&origin, &direction).ok().flatten());
            let Some(hit) = hit else { return };
            inspect::add_pick(&mut st.overlay.picks, hit);
            st.overlay.picks.clone()
        };
        glib::g_debug!(
            "neoscad",
            "measure: {}",
            inspect::picks_text(&picks).replace('\n', "; ")
        );
        self.measure.show_picks(&picks, true);
        self.update_overlay();
    }

    /// Draw the panels' state in the view.
    fn update_overlay(&self) {
        let a = inspect::annotations(&self.st.borrow().overlay);
        glib::g_debug!(
            "neoscad",
            "overlay: {} markers, {} lines",
            a.markers.len(),
            a.lines.len()
        );
        self.camera(|v| v.set_annotations(a));
        self.view.queue();
    }

    // --- Watching the files a run read --------------------------------------

    /// Watch `files` (the last run's) and the document's own file from now
    /// on. The monitors are made again only when the directories change.
    fn watch_files(self: &Rc<Self>, files: Vec<PathBuf>) {
        let files = self.watched_paths(files);
        let n = files.len();
        let dirs = {
            let mut st = self.st.borrow_mut();
            if !st.watch.set(files) {
                return;
            }
            for m in st.monitors.drain(..) {
                m.cancel();
            }
            st.watch.directories()
        };
        let mut monitors = Vec::new();
        for dir in &dirs {
            match gio::File::for_path(dir)
                .monitor_directory(gio::FileMonitorFlags::WATCH_MOVES, gio::Cancellable::NONE)
            {
                Ok(m) => {
                    let me = Rc::downgrade(self);
                    m.connect_changed(move |_, file, other, event| {
                        if let Some(w) = me.upgrade() {
                            w.file_event(file, other, event);
                        }
                    });
                    monitors.push(m);
                }
                Err(e) => glib::g_warning!("neoscad", "cannot watch {}: {e}", dir.display()),
            }
        }
        glib::g_debug!("neoscad", "watch: {n} files in {} directories", dirs.len());
        self.st.borrow_mut().monitors = monitors;
    }

    /// A directory monitor's event: a watched file written, replaced
    /// (renamed over: `other` is the new name) or removed runs the
    /// document again, once for a burst of events.
    fn file_event(
        self: &Rc<Self>,
        file: &gio::File,
        other: Option<&gio::File>,
        event: gio::FileMonitorEvent,
    ) {
        use gio::FileMonitorEvent as E;
        // CHANGED comes many times while a file is written; the hint
        // after the last write is the one to act on.
        if !matches!(
            event,
            E::ChangesDoneHint | E::Created | E::Deleted | E::Renamed | E::MovedIn | E::MovedOut
        ) {
            return;
        }
        let (ours, theirs) = {
            let st = self.st.borrow();
            let doc = st.doc.file();
            let hits: Vec<PathBuf> = [Some(file), other]
                .into_iter()
                .flatten()
                .filter_map(|f| f.path())
                .filter(|p| st.watch.concerns(p))
                .collect();
            (
                hits.iter().any(|p| Some(p.as_path()) == doc),
                hits.into_iter().find(|p| Some(p.as_path()) != doc),
            )
        };
        if ours {
            // The document's own file: another program's write, or this
            // window's save coming back (which the check recognises).
            self.disk_changed();
        }
        let Some(path) = theirs else { return };
        let mut st = self.st.borrow_mut();
        glib::g_debug!("neoscad", "watch: {} changed ({event:?})", path.display());
        if st.watch_timer.is_some() {
            return;
        }
        let me = Rc::downgrade(self);
        st.watch_timer = Some(glib::timeout_add_local_once(
            Duration::from_millis(100),
            move || {
                let Some(w) = me.upgrade() else { return };
                let now = w.shared.now_ms();
                let mode = {
                    let mut st = w.st.borrow_mut();
                    st.watch_timer = None;
                    st.lp.files_changed(now)
                };
                match mode {
                    Some(m) => w.run(m),
                    None => w.arm_timer(),
                }
            },
        ));
    }

    // --- Files --------------------------------------------------------------

    /// Save, then `then(saved)`.
    fn save(self: &Rc<Self>, then: Option<Box<dyn FnOnce(bool)>>) {
        match self.file() {
            Some(path) => self.save_to_file(path, then),
            None => self.save_as(then),
        }
    }

    fn save_as(self: &Rc<Self>, then: Option<Box<dyn FnOnce(bool)>>) {
        let name = self.st.borrow().doc.name();
        let dialog = gtk::FileDialog::builder()
            .title("Save As")
            .modal(true)
            .initial_name(name)
            .filters(&super::scad_filters())
            .build();
        let me = Rc::downgrade(self);
        dialog.save(Some(&self.win), gio::Cancellable::NONE, move |r| {
            let Some(w) = me.upgrade() else { return };
            let ok = match r.ok().and_then(|f| f.path()) {
                Some(mut p) => {
                    if p.extension().is_none() {
                        p.set_extension("scad");
                    }
                    w.write_to(p)
                }
                None => false,
            };
            if let Some(t) = then {
                t(ok);
            }
        });
    }

    fn write_to(self: &Rc<Self>, path: PathBuf) -> bool {
        let bytes = self.st.borrow().doc.text.bytes().to_vec();
        if let Err(e) = run::write_atomic(&path, &bytes) {
            self.toast(&format!("Could not save {}: {e}", path.display()));
            return false;
        }
        let (old, uri) = {
            let mut st = self.st.borrow_mut();
            st.doc.saved_to(path.clone());
            let old = st.lp.set_path(&path.to_string_lossy());
            let uri = (old.is_some() && st.language.is_some() && st.bridge.is_ready())
                .then(|| language::document_uri(&st.doc.core_path()));
            (old, uri)
        };
        if let Some(old) = old {
            let _ = self.shared.client.close(&old);
        }
        // The file is the window's text now: no notice stands, and under
        // a new name the new file is the one watched.
        self.disk_reset();
        // Saved under a new name: the page's client closes the old URI
        // and opens the new one, so the server's markers and the run's
        // diagnostics agree on the path again.
        if let Some(uri) = uri {
            self.call("setURI", &[json!(uri)], |_, _| {});
        }
        let uri = gio::File::for_path(&path).uri();
        gtk::RecentManager::default().add_item(&uri);
        self.update_titles();
        true
    }

    /// The core's buffer holds the editor's text before a detached request
    /// (an export) reads it.
    fn sync_core(&self) -> Result<String, CoreError> {
        let mut st = self.st.borrow_mut();
        let path = st.doc.core_path();
        if !st.lp.in_sync() {
            self.shared.client.update(&path, st.doc.text.text())?;
            st.lp.text_sent();
        }
        Ok(path)
    }

    /// File > Export in the format `id` (the core's table): a save
    /// dialog, then the export with a progress toast.
    fn export(self: &Rc<Self>, id: &str) {
        let Some(format) = client::export_format_info(id) else {
            return;
        };
        self.st.borrow_mut().last_export = id.to_string();
        if format.kind == ExportKind::ViewImage {
            return self.export_png();
        }
        let name = document::export_name(&self.st.borrow().doc.name(), &format.extension);
        let dialog = gtk::FileDialog::builder()
            .title(format!("Export {}", format.title))
            .modal(true)
            .initial_name(name)
            .build();
        // Beside the model, as its other files are; without a portal the
        // chooser would otherwise start in the working directory.
        if let Some(dir) = self.file().as_deref().and_then(Path::parent) {
            dialog.set_initial_folder(Some(&gio::File::for_path(dir)));
        }
        let me = Rc::downgrade(self);
        dialog.save(Some(&self.win), gio::Cancellable::NONE, move |r| {
            if let (Some(w), Some(out)) = (me.upgrade(), r.ok().and_then(|f| f.path())) {
                w.start_export(format, out);
            }
        });
    }

    /// Export again in the last format, or one that fits the model's
    /// dimension (a 2D model to SVG, a 3D one to STL).
    fn export_again(self: &Rc<Self>) {
        let id = {
            let st = self.st.borrow();
            client::suggest_export_format(&st.last_export, st.last_dimensions)
        };
        self.export(&id);
    }

    /// Export to `out` off the main thread, detached from the document's
    /// runs (typing does not cancel it), with a toast that says the stage
    /// and has a Cancel button. A newer export cancels this one. Success
    /// is a toast; a failure is an alert with the core's reason, never
    /// silence (docs/audits/agent-surface.md, finding 5).
    fn start_export(self: &Rc<Self>, format: ExportFormatInfo, out: PathBuf) {
        let path = match self.sync_core() {
            Ok(p) => p,
            Err(e) => return self.toast(&e.to_string()),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let options = {
            let mut st = self.st.borrow_mut();
            if let Some(old) = st.export_stop.replace(stop.clone()) {
                old.store(true, Ordering::Relaxed);
            }
            self.run_options(&st)
        };
        let name = file_name(&out.to_string_lossy());
        let toast = adw::Toast::builder()
            .title(format!("Exporting {name}…"))
            .timeout(0)
            .button_label("Cancel")
            .priority(adw::ToastPriority::High)
            .build();
        let s = stop.clone();
        toast.connect_button_clicked(move |_| s.store(true, Ordering::Relaxed));
        self.toasts.add_toast(toast.clone());
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<session::Stage>();
        let progress: session::Progress = Arc::new(move |stage| {
            let _ = tx.unbounded_send(stage);
        });
        let (t, n) = (toast.clone(), name.clone());
        glib::spawn_future_local(async move {
            use futures_util::StreamExt;
            while let Some(stage) = rx.next().await {
                t.set_title(&format!("Exporting {n}: {}…", run::stage_label(stage)));
            }
        });
        glib::g_debug!("neoscad", "export: {} to {name}", format.id);
        let client = self.shared.client.clone();
        let me = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let o = out.clone();
            let s = stop.clone();
            let r = gio::spawn_blocking(move || match format.kind {
                ExportKind::Snapshot => {
                    match run::snapshot_file(&client, &path, &o, (1024, 1024), &options, Some(s)) {
                        Ok(n) => Some(Ok(format!(
                            "Exported {} ({n} bytes)",
                            file_name(&o.to_string_lossy())
                        ))),
                        Err(CoreError::Cancelled) => None,
                        Err(e) => Some(Err(e.to_string())),
                    }
                }
                _ => run::export_message(
                    &o,
                    &run::export_file(
                        &client,
                        &path,
                        &o.to_string_lossy(),
                        &format.id,
                        &options,
                        Some(s),
                        Some(progress),
                    ),
                ),
            })
            .await
            .unwrap_or_else(|_| Some(Err("The export panicked (a bug in NeoSCAD).".into())));
            toast.dismiss();
            let Some(w) = me.upgrade() else { return };
            {
                let mut st = w.st.borrow_mut();
                if st
                    .export_stop
                    .as_ref()
                    .is_some_and(|x| Arc::ptr_eq(x, &stop))
                {
                    st.export_stop = None;
                }
            }
            match r {
                Some(Ok(message)) => {
                    glib::g_debug!("neoscad", "export: {message}");
                    w.toast(&message);
                }
                Some(Err(why)) => {
                    glib::g_debug!("neoscad", "export: {name} failed: {why}");
                    let alert = adw::AlertDialog::new(
                        Some(&format!("“{name}” Was Not Exported")),
                        Some(&why),
                    );
                    alert.add_response("close", "_Close");
                    alert.present(Some(&w.win));
                }
                None => w.toast(&format!("Export of {name} cancelled")),
            }
        });
    }

    /// The view as a PNG, at the view's size, without the grid (as the
    /// macOS app's File > Export > PNG image of the view).
    fn export_png(self: &Rc<Self>) {
        let Some(format) = client::export_format_info("view-image") else {
            return;
        };
        let name = document::export_name(&self.st.borrow().doc.name(), &format.extension);
        let dialog = gtk::FileDialog::builder()
            .title(format!("Export {}", format.title))
            .modal(true)
            .initial_name(name)
            .build();
        let me = Rc::downgrade(self);
        dialog.save(Some(&self.win), gio::Cancellable::NONE, move |r| {
            let (Some(w), Some(out)) = (me.upgrade(), r.ok().and_then(|f| f.path())) else {
                return;
            };
            let copy = w.view.canvas.borrow().as_ref().map(|c| {
                let (width, height) = c.viewport.size();
                c.viewport
                    .copy_for_image(width.max(64), height.max(64))
                    .map_err(|e| e.to_string())
            });
            let Some(Ok(mut copy)) = copy else {
                return w.toast("Export failed: there is no 3D view.");
            };
            let me = Rc::downgrade(&w);
            glib::spawn_future_local(async move {
                let o = out.clone();
                let r = gio::spawn_blocking(move || {
                    let image = copy.read_pixels_blocking().map_err(|e| e.to_string())?;
                    let png = render::encode_png(image.width, image.height, &image.rgba);
                    run::write_atomic(&o, &png).map_err(|e| e.to_string())
                })
                .await;
                let Some(w) = me.upgrade() else { return };
                w.toast(&match r {
                    Ok(Ok(n)) => {
                        format!("Exported {} ({n} bytes)", file_name(&out.to_string_lossy()))
                    }
                    Ok(Err(e)) => format!("Export failed: {e}"),
                    Err(_) => "Export failed (a bug in NeoSCAD).".into(),
                });
            });
        });
    }

    /// Closing: ask about unsaved changes (GNOME's "Save changes?"
    /// dialog), then let the core forget the document.
    fn close_request(self: &Rc<Self>) -> glib::Propagation {
        let (dirty, name) = {
            let st = self.st.borrow();
            (st.doc.is_dirty(), st.doc.name())
        };
        if !dirty {
            self.closed();
            return glib::Propagation::Proceed;
        }
        let dialog = adw::AlertDialog::new(
            Some(&format!("Save Changes to “{name}”?")),
            Some("Unsaved changes will be lost."),
        );
        dialog.add_responses(&[
            ("cancel", "_Cancel"),
            ("discard", "_Discard"),
            ("save", "_Save"),
        ]);
        dialog.set_response_appearance("discard", adw::ResponseAppearance::Destructive);
        dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("save"));
        dialog.set_close_response("cancel");
        let me = Rc::downgrade(self);
        dialog.choose(Some(&self.win), gio::Cancellable::NONE, move |response| {
            let Some(w) = me.upgrade() else { return };
            match response.as_str() {
                "discard" => {
                    w.closed();
                    w.win.destroy();
                }
                "save" => {
                    let me = Rc::downgrade(&w);
                    w.save(Some(Box::new(move |ok| {
                        if let (true, Some(w)) = (ok, me.upgrade()) {
                            w.closed();
                            w.win.destroy();
                        }
                    })));
                }
                _ => {}
            }
        });
        glib::Propagation::Stop
    }

    fn closed(&self) {
        let path = {
            let mut st = self.st.borrow_mut();
            if let Some(t) = st.timer.take() {
                t.remove();
            }
            if let Some(t) = st.watch_timer.take() {
                t.remove();
            }
            if let Some(t) = st.disk_timer.take() {
                t.remove();
            }
            for m in st.monitors.drain(..) {
                m.cancel();
            }
            st.watch.clear();
            for stop in [
                st.check_stop.take(),
                st.measure_stop.take(),
                st.export_stop.take(),
            ]
            .into_iter()
            .flatten()
            {
                stop.store(true, Ordering::Relaxed);
            }
            st.lp.close();
            if let Some(ls) = st.language.take() {
                ls.stop();
            }
            st.doc.core_path()
        };
        let _ = self.shared.client.close(&path);
    }
}

/// The window's language server, delivering to its page.
fn start_language(shared: &Rc<Shared>, me: Weak<Window>) -> Language {
    shared.language(move |m| {
        if let Some(w) = me.upgrade() {
            w.deliver(m);
        }
    })
}

fn file_name(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.to_string())
}

/// The side panels: a view switcher over the customizer, check and
/// measure pages, each panel asking `me` for what it needs.
#[allow(clippy::type_complexity)]
fn side_panels(
    me: &Weak<Window>,
) -> (
    adw::ToolbarView,
    gtk::Stack,
    Rc<Customizer>,
    Rc<CheckPanel>,
    Rc<MeasurePanel>,
) {
    let m = me.clone();
    let customizer = Customizer::new(move |r| {
        if let Some(w) = m.upgrade() {
            w.customize(r);
        }
    });
    let ask = |me: &Weak<Window>| {
        let m = me.clone();
        move |r| {
            if let Some(w) = m.upgrade() {
                w.inspect(r);
            }
        }
    };
    let check = CheckPanel::new(ask(me));
    let measure = MeasurePanel::new(ask(me));
    // A stack switcher (linked buttons with titles), not libadwaita's view
    // switcher: that one always draws an icon beside each title, and in a
    // 300-pixel sidebar it cuts "Customizer" short.
    let stack = gtk::Stack::builder()
        .transition_type(gtk::StackTransitionType::Crossfade)
        .vexpand(true)
        .build();
    stack.add_titled(&customizer.root, Some("customizer"), "Customizer");
    stack.add_titled(&check.root, Some("check"), "Check");
    stack.add_titled(&measure.root, Some("measure"), "Measure");
    let switcher = gtk::StackSwitcher::builder()
        .stack(&stack)
        .hexpand(true)
        .halign(gtk::Align::Center)
        .build();
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    bar.add_css_class("toolbar");
    bar.append(&switcher);
    let side = adw::ToolbarView::new();
    side.add_top_bar(&bar);
    side.set_content(Some(&stack));
    (side, stack, customizer, check, measure)
}
