//! A document window: the header bar, the editor, the 3D view and the
//! console, and the document loop that ties them (`client::DocumentLoop`:
//! the host keeps one GLib timer and passes "now"; the loop decides when
//! to run, what to send the core, and whether a result is current).
//!
//! Each window's editor has a language server of its own
//! (`linux_app::language`), whose markers come from the window's runs.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};
use serde_json::{Value, json};
use webkit6::prelude::WebViewExt;

use client::{CoreError, DocumentLoop, RenderMode, SourceRange};
use linux_app::bridge::{self, ChangeOutcome, EditorBridge, Incoming};
use linux_app::document::{self, Document};
use linux_app::language::{self, Language};
use linux_app::run::{self, FileView};
use linux_app::view::ViewCanvas;

use super::Shared;
use super::console::Console;
use super::editor;
use super::viewport::ViewWidget;

/// The colour scheme pair of the macOS app (`ViewportController.swift`):
/// OpenSCAD's default for a light style, a dark scheme of its own for dark.
const LIGHT_SCHEME: &str = "Cornfield";
const DARK_SCHEME: &str = "Tomorrow Night";

pub struct Window {
    win: adw::ApplicationWindow,
    title: adw::WindowTitle,
    toasts: adw::ToastOverlay,
    web: Option<webkit6::WebView>,
    view: ViewWidget,
    console: Console,
    spinner: gtk::Spinner,
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
            let toasts = adw::ToastOverlay::new();
            toasts.set_child(Some(&split));
            let toolbar = adw::ToolbarView::new();
            toolbar.add_top_bar(&header);
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
                web,
                view,
                console,
                spinner,
                shared: shared2,
                st: RefCell::new(State {
                    doc,
                    bridge: EditorBridge::default(),
                    lp,
                    timer: None,
                    file_view: None,
                    language,
                    reveal: None,
                }),
            }
        })
        .setup()
    }

    fn setup(self: Rc<Self>) -> Rc<Self> {
        let actions: [(&str, Action); 8] = [
            ("save", |w| w.save(None)),
            ("save-as", |w| w.save_as(None)),
            ("export-stl", Window::export_stl),
            ("export-png", Window::export_png),
            ("preview", |w| w.run(RenderMode::Preview)),
            ("render", |w| w.run(RenderMode::Render)),
            ("view-all", |w| w.camera(|v| v.view_all())),
            ("reset-view", |w| w.camera(|v| v.reset_view())),
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
        let (me, shared) = (Rc::downgrade(&self), self.shared.clone());
        self.win.connect_destroy(move |_| shared.forget(&me));
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
            old
        };
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
    }

    fn camera(&self, f: impl FnOnce(&mut render::viewport::Viewport)) {
        if let Some(c) = self.view.canvas.borrow_mut().as_mut() {
            f(&mut c.viewport);
        }
    }

    // --- Files --------------------------------------------------------------

    /// Save, then `then(saved)`.
    fn save(self: &Rc<Self>, then: Option<Box<dyn FnOnce(bool)>>) {
        match self.file() {
            Some(path) => {
                let ok = self.write_to(path);
                if let Some(t) = then {
                    t(ok);
                }
            }
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

    fn export_stl(self: &Rc<Self>) {
        let Some(format) = client::export_format_info("binstl") else {
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
            let path = match w.sync_core() {
                Ok(p) => p,
                Err(e) => return w.toast(&e.to_string()),
            };
            let options = {
                let st = w.st.borrow();
                client::RunOptions {
                    overrides: st.lp.overrides(),
                    parts: st.lp.parts(),
                    enable: Vec::new(),
                }
            };
            let client = w.shared.client.clone();
            let id = format.id.clone();
            let me = Rc::downgrade(&w);
            glib::spawn_future_local(async move {
                let output = out.to_string_lossy().into_owned();
                let o = output.clone();
                let r = gio::spawn_blocking(move || {
                    run::export_file(&client, &path, &o, &id, &options)
                })
                .await;
                let Some(w) = me.upgrade() else { return };
                let message = match r {
                    Ok(Ok(r)) => match client::export_failure_reason(&r) {
                        None => format!("Exported {} ({} bytes)", file_name(&output), r.bytes),
                        Some(why) => format!("Export failed: {why}"),
                    },
                    Ok(Err(e)) => format!("Export failed: {e}"),
                    Err(_) => "Export failed (a bug in NeoSCAD).".into(),
                };
                w.toast(&message);
            });
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
