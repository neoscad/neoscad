//! The application: one core for the process, a window per document, the
//! app-wide actions (New Window, Open, Examples, the colour scheme,
//! Connect AI Agent, Preferences, Check for Updates, About, Quit) and their
//! shortcuts.

mod agent;
mod console;
mod customizer;
mod editor;
mod inspect;
mod library;
mod update;
mod viewport;
mod window;

use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::{Rc, Weak};
use std::sync::Arc;
use std::time::Instant;

use adw::prelude::*;
use gtk::{gio, glib};

use client::Client;
use linux_app::language::{self, Language, Sink, Target};

use library::LibraryViewer;
pub use window::Window;

/// The application id: the D-Bus name, the desktop file's name and the
/// icon's (the macOS app's bundle id is `org.neoscad.NeoSCAD`).
pub const APP_ID: &str = "org.neoscad.NeoSCAD";

/// What every window shares.
pub struct Shared {
    /// The one core: its session caches parses and geometry across every
    /// window, as the macOS app's `CoreService` does.
    pub client: Arc<Client>,
    /// The monotonic clock the document loops take "now" from (the core
    /// has no clock: CLAUDE.md, "Rules").
    started: Instant,
    /// The open windows. The list owns them: GTK holds the widgets, but
    /// every signal handler holds its window weakly (a handler owning its
    /// window would be a cycle), so without this the window's state would
    /// be dropped as soon as it was shown. A window leaves on destroy.
    windows: RefCell<Vec<Rc<Window>>>,
    /// The read-only library files shown (one viewer per file), owned as
    /// the windows are.
    viewers: RefCell<Vec<Rc<LibraryViewer>>>,
    /// Analysed library files, shared by every editor's language server,
    /// so BOSL2 is indexed once per process.
    lsp_cache: Arc<lsp::Cache>,
    /// Where the editor bundle is, found once.
    pub editor_dir: Option<PathBuf>,
    /// The update check's settings and the newer release, if any
    /// (`update.rs`).
    updates: update::Updates,
    /// AI agents: the consent, the link to `neoscad mcp` and its status
    /// (`agent.rs`).
    agents: agent::Agents,
    /// Preferences > Language: the extensions every window runs with
    /// (`linux_app::extensions`), and where they are kept.
    extensions: RefCell<linux_app::extensions::Settings>,
    extensions_path: PathBuf,
    /// `G_MESSAGES_DEBUG` names this app: the bridge also asks the page
    /// how many markers it shows after each publication, for the log
    /// (linux/smoke.sh checks it). Off, that is one call saved per run.
    pub debug: bool,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Shared").finish_non_exhaustive()
    }
}

impl Shared {
    pub fn now_ms(&self) -> u64 {
        self.started.elapsed().as_millis() as u64
    }

    fn windows(&self) -> Vec<Rc<Window>> {
        self.windows.borrow().clone()
    }

    /// The `--enable` names of Preferences > Language.
    pub fn enable(&self) -> Vec<String> {
        self.extensions.borrow().names()
    }

    /// Preferences > Language changed: keep it, and hand the names to
    /// every window, which runs its document again with them.
    pub fn set_extensions(&self, s: linux_app::extensions::Settings) {
        if *self.extensions.borrow() == s {
            return;
        }
        *self.extensions.borrow_mut() = s;
        if let Err(e) = s.save(&self.extensions_path) {
            glib::g_warning!(
                "neoscad",
                "language: could not save {}: {e}",
                self.extensions_path.display()
            );
        }
        let names = s.names();
        for w in self.windows() {
            w.set_enable(&names);
        }
    }

    /// A window was destroyed.
    pub fn forget(&self, window: &Weak<Window>) {
        self.windows
            .borrow_mut()
            .retain(|w| !std::ptr::eq(Rc::as_ptr(w), window.as_ptr()));
    }

    /// A library viewer was destroyed.
    pub fn forget_viewer(&self, viewer: &Weak<LibraryViewer>) {
        self.viewers
            .borrow_mut()
            .retain(|v| !std::ptr::eq(Rc::as_ptr(v), viewer.as_ptr()));
    }

    /// A language server for one editor page, whose messages reach
    /// `deliver` on the main thread in the order the server gave them.
    /// The worker's answers and a run's markers come from other threads;
    /// a channel read by one main-loop future keeps them in order, which
    /// a main-context callback per message would not promise.
    pub fn language(&self, deliver: impl Fn(String) + 'static) -> Language {
        let (tx, mut rx) = futures_channel::mpsc::unbounded::<Vec<String>>();
        let sink: Sink = Arc::new(move |m| {
            let _ = tx.unbounded_send(m);
        });
        let ls = Language::start(self.client.clone(), self.lsp_cache.clone(), sink);
        glib::spawn_future_local(async move {
            use futures_util::StreamExt;
            // Ends when every sender is gone: the server stopped and its
            // last run finished.
            while let Some(batch) = rx.next().await {
                for m in batch {
                    deliver(m);
                }
            }
        });
        ls
    }

    /// A path for a new untitled document named after `base`, in the home
    /// folder (so its includes resolve there, as a file's would), unique
    /// among open windows and on disk (`Client::untitled_path`).
    pub fn untitled_path(&self, base: &str) -> String {
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        let taken: Vec<String> = self.windows().iter().map(|w| w.core_path()).collect();
        self.client
            .untitled_path(&home, base, &taken)
            .unwrap_or_else(|_| format!("{home}/{base}.scad"))
    }
}

/// Run the app; the exit code of `GApplication::run`.
pub fn run() -> glib::ExitCode {
    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::HANDLES_OPEN | gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let shared: Rc<RefCell<Option<Rc<Shared>>>> = Rc::default();

    let s = shared.clone();
    app.connect_startup(move |app| {
        let editor_dir =
            linux_app::resources::find_editor_dir(&linux_app::resources::editor_dir_candidates(
                std::env::var_os("NEOSCAD_EDITOR_DIR").map(PathBuf::from),
                std::env::current_exe().ok().as_deref(),
            ));
        editor::register_scheme(editor_dir.clone());
        let sh = Rc::new(Shared {
            client: Arc::new(Client::new(linux_app::host::config())),
            started: Instant::now(),
            windows: RefCell::default(),
            viewers: RefCell::default(),
            lsp_cache: Arc::new(lsp::Cache::new()),
            editor_dir,
            updates: update::Updates::new(),
            agents: agent::Agents::new(),
            extensions: RefCell::new(linux_app::extensions::Settings::load(
                &linux_app::extensions::settings_path(&glib::user_config_dir()),
            )),
            extensions_path: linux_app::extensions::settings_path(&glib::user_config_dir()),
            debug: std::env::var("G_MESSAGES_DEBUG")
                .is_ok_and(|v| v.split([',', ' ']).any(|d| d == "neoscad" || d == "all")),
        });
        install_actions(app, &sh);
        update::start(&sh);
        agent::start(&sh);
        *s.borrow_mut() = Some(sh);
    });

    let s = shared.clone();
    app.connect_activate(move |app| {
        if let Some(sh) = s.borrow().clone() {
            new_window(app, &sh).widget().present();
        }
    });

    let s = shared;
    app.connect_open(move |app, files, _hint| {
        if let Some(sh) = s.borrow().clone() {
            for f in files {
                if let Some(p) = f.path() {
                    open_path(app, &sh, p);
                }
            }
        }
    });

    app.run()
}

/// A new window with an empty untitled document.
fn new_window(app: &adw::Application, sh: &Rc<Shared>) -> Rc<Window> {
    let path = sh.untitled_path("Untitled");
    let w = Window::new(
        app,
        sh.clone(),
        linux_app::document::Document::untitled(path, String::new()),
    );
    sh.windows.borrow_mut().push(w.clone());
    w
}

/// The window to show a newly opened document in: the active one when it
/// holds an untouched untitled document (as GNOME Text Editor reuses an
/// empty tab), else a new one.
fn target_window(app: &adw::Application, sh: &Rc<Shared>) -> Rc<Window> {
    let active = app.active_window();
    sh.windows()
        .into_iter()
        .find(|w| active.as_ref() == Some(w.widget().upcast_ref()) && w.is_replaceable())
        .unwrap_or_else(|| new_window(app, sh))
}

/// Open the file at `path`, or show why it cannot be; the window that
/// shows it.
pub fn open_path(app: &adw::Application, sh: &Rc<Shared>, path: PathBuf) -> Option<Rc<Window>> {
    // Already open: bring its window forward.
    if let Some(w) = sh
        .windows()
        .into_iter()
        .find(|w| w.file().as_deref() == Some(path.as_path()))
    {
        w.widget().present();
        return Some(w);
    }
    let text = std::fs::read(&path)
        .map_err(|e| e.to_string())
        .and_then(linux_app::document::decode);
    match text {
        Ok(text) => {
            gtk::RecentManager::default().add_item(&gio::File::for_path(&path).uri());
            let w = target_window(app, sh);
            w.replace_document(linux_app::document::Document::from_file(path, text), true);
            w.widget().present();
            Some(w)
        }
        Err(e) => {
            let w = app
                .active_window()
                .and_then(|a| {
                    sh.windows()
                        .into_iter()
                        .find(|w| w.widget().upcast_ref::<gtk::Window>() == &a)
                })
                .unwrap_or_else(|| {
                    let w = new_window(app, sh);
                    w.widget().present();
                    w
                });
            w.toast(&format!("Could not open {}: {e}", path.display()));
            None
        }
    }
}

/// Go to a definition in another file (the editor's `open` message): a
/// file of the user's own opens as a document, a library file read-only
/// in a viewer (`language::target` has the rule), at the 0-based line
/// and UTF-16 column. Why not, for the window it was reached from to
/// say.
pub fn open_location(
    app: &adw::Application,
    sh: &Rc<Shared>,
    uri: &str,
    line: u64,
    character: u64,
) -> Result<(), String> {
    let dirs = sh.client.library_dirs();
    let writable = |p: &std::path::Path| {
        std::fs::metadata(p).is_ok_and(|m| m.is_file() && !m.permissions().readonly())
    };
    let target = language::target(uri, &dirs, writable);
    glib::g_debug!("neoscad", "definition at {line}:{character} in {target:?}");
    match target {
        Some(Target::Document(path)) => {
            // `open_path` shows its own message if the file cannot be
            // read.
            if let Some(w) = open_path(app, sh, path) {
                w.reveal_at(line, character);
            }
            Ok(())
        }
        Some(Target::Library(path)) => {
            let existing = sh
                .viewers
                .borrow()
                .iter()
                .find(|v| v.path() == path.as_path())
                .cloned();
            let v = match existing {
                Some(v) => v,
                None => {
                    let v = LibraryViewer::new(app, sh.clone(), path.clone(), &dirs)
                        .map_err(|e| format!("Could not show {}: {e}", path.display()))?;
                    sh.viewers.borrow_mut().push(v.clone());
                    v
                }
            };
            v.widget().present();
            v.reveal_at(line, character);
            Ok(())
        }
        None => Err(format!("Could not open {uri}: not a file")),
    }
}

/// File > Examples: the example as a new untitled document named after
/// it; a heavy one waits for Preview (its note says why).
fn open_example(app: &adw::Application, sh: &Rc<Shared>, id: &str) {
    let Some(e) = client::examples().into_iter().find(|e| e.id == id) else {
        return;
    };
    let Ok(text) = client::example_source(id) else {
        return;
    };
    let stem = std::path::Path::new(&e.file_name)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Example".into());
    let path = sh.untitled_path(&stem);
    let w = target_window(app, sh);
    w.set_parts(e.parts);
    w.replace_document(
        linux_app::document::Document::untitled(path, text),
        e.autorun,
    );
    if !e.note.is_empty() {
        w.toast(&e.note);
    }
    w.widget().present();
}

/// The app's actions and their accelerators.
fn install_actions(app: &adw::Application, sh: &Rc<Shared>) {
    let new = gio::SimpleAction::new("new-window", None);
    let (a, s) = (app.downgrade(), sh.clone());
    new.connect_activate(move |_, _| {
        if let Some(app) = a.upgrade() {
            new_window(&app, &s).widget().present();
        }
    });
    app.add_action(&new);

    let open = gio::SimpleAction::new("open", None);
    let (a, s) = (app.downgrade(), sh.clone());
    open.connect_activate(move |_, _| {
        if let Some(app) = a.upgrade() {
            open_dialog(&app, &s);
        }
    });
    app.add_action(&open);

    let example = gio::SimpleAction::new("example", Some(glib::VariantTy::STRING));
    let (a, s) = (app.downgrade(), sh.clone());
    example.connect_activate(move |_, p| {
        if let (Some(app), Some(id)) = (a.upgrade(), p.and_then(|p| p.str().map(str::to_string))) {
            open_example(&app, &s, &id);
        }
    });
    app.add_action(&example);

    // The colour scheme: follow the system (the default), or force one.
    // AdwStyleManager recolours every window, and the editor and the view
    // follow it (the page through prefers-color-scheme, the view by
    // switching between the Mac app's scheme pair).
    let style = gio::SimpleAction::new_stateful(
        "style",
        Some(glib::VariantTy::STRING),
        &"system".to_variant(),
    );
    style.connect_activate(|action, p| {
        let Some(v) = p.and_then(|p| p.str().map(str::to_string)) else {
            return;
        };
        adw::StyleManager::default().set_color_scheme(match v.as_str() {
            "light" => adw::ColorScheme::ForceLight,
            "dark" => adw::ColorScheme::ForceDark,
            _ => adw::ColorScheme::Default,
        });
        action.set_state(&v.to_variant());
    });
    app.add_action(&style);

    // Main menu > Connect AI Agent…: the Agents page on its own.
    let agents = gio::SimpleAction::new("agents", None);
    let (a, s) = (app.downgrade(), sh.clone());
    agents.connect_activate(move |_, _| {
        if let Some(app) = a.upgrade() {
            agent::show_dialog(&s, app.active_window().as_ref());
        }
    });
    app.add_action(&agents);

    let preferences = gio::SimpleAction::new("preferences", None);
    let (a, s) = (app.downgrade(), sh.clone());
    preferences.connect_activate(move |_, _| {
        if let Some(app) = a.upgrade() {
            update::preferences(&s, app.active_window().as_ref());
        }
    });
    app.add_action(&preferences);

    let check_updates = gio::SimpleAction::new("check-updates", None);
    let s = sh.clone();
    check_updates.connect_activate(move |_, _| update::check_now(&s));
    app.add_action(&check_updates);

    let about = gio::SimpleAction::new("about", None);
    let a = app.downgrade();
    about.connect_activate(move |_, _| {
        let Some(app) = a.upgrade() else { return };
        let dialog = adw::AboutDialog::builder()
            .application_name("NeoSCAD")
            .application_icon(APP_ID)
            .version(env!("CARGO_PKG_VERSION"))
            .website("https://neoscad.org")
            .issue_url("https://github.com/neoscad/neoscad/issues")
            .license_type(gtk::License::Gpl20)
            .comments("A reimplementation of OpenSCAD.")
            .build();
        dialog.present(app.active_window().as_ref());
    });
    app.add_action(&about);

    let quit = gio::SimpleAction::new("quit", None);
    let a = app.downgrade();
    quit.connect_activate(move |_, _| {
        if let Some(app) = a.upgrade() {
            // Each window asks about its unsaved changes as it closes.
            for w in app.windows() {
                w.close();
            }
        }
    });
    app.add_action(&quit);
}

/// File > Open.
fn open_dialog(app: &adw::Application, sh: &Rc<Shared>) {
    let dialog = gtk::FileDialog::builder()
        .title("Open")
        .modal(true)
        .filters(&scad_filters())
        .build();
    let (a, s) = (app.downgrade(), sh.clone());
    dialog.open(
        app.active_window().as_ref(),
        gio::Cancellable::NONE,
        move |r| {
            if let (Some(app), Ok(file)) = (a.upgrade(), r)
                && let Some(p) = file.path()
            {
                open_path(&app, &s, p);
            }
        },
    );
}

/// The file chooser's filters: OpenSCAD files first, then everything.
pub fn scad_filters() -> gio::ListStore {
    let store = gio::ListStore::new::<gtk::FileFilter>();
    let scad = gtk::FileFilter::new();
    scad.set_name(Some("OpenSCAD files"));
    scad.add_suffix("scad");
    store.append(&scad);
    let all = gtk::FileFilter::new();
    all.set_name(Some("All files"));
    all.add_pattern("*");
    store.append(&all);
    store
}

/// The header bar's main menu.
pub fn main_menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let item = |label: &str, action: &str, accel: Option<&str>| {
        let i = gio::MenuItem::new(Some(label), Some(action));
        if let Some(a) = accel {
            i.set_attribute_value("accel", Some(&a.to_variant()));
        }
        i
    };

    let file = gio::Menu::new();
    file.append_item(&item("New Window", "app.new-window", Some("<Control>n")));
    file.append_item(&item("Open…", "app.open", Some("<Control>o")));
    let examples = gio::Menu::new();
    for e in client::examples() {
        let i = gio::MenuItem::new(Some(&e.title), None);
        i.set_action_and_target_value(Some("app.example"), Some(&e.id.to_variant()));
        examples.append_item(&i);
    }
    file.append_submenu(Some("Examples"), &examples);
    menu.append_section(None, &file);

    let save = gio::Menu::new();
    save.append_item(&item("Save", "win.save", Some("<Control>s")));
    save.append_item(&item("Save As…", "win.save-as", Some("<Control><Shift>s")));
    menu.append_section(None, &save);

    // File > Export: every format of the core's table
    // (`client::export_formats`), and Export Again in the last one (or
    // one that fits the model's dimension).
    let export = gio::Menu::new();
    let formats = gio::Menu::new();
    for f in client::export_formats() {
        let i = gio::MenuItem::new(Some(&format!("{}…", f.title)), None);
        i.set_action_and_target_value(Some("win.export"), Some(&f.id.to_variant()));
        formats.append_item(&i);
    }
    export.append_submenu(Some("Export"), &formats);
    export.append_item(&item(
        "Export Again…",
        "win.export-again",
        Some("<Control><Shift>e"),
    ));
    menu.append_section(None, &export);

    let design = gio::Menu::new();
    design.append_item(&item("Preview", "win.preview", Some("F5")));
    design.append_item(&item("Render", "win.render", Some("F6")));
    design.append_item(&item("View All", "win.view-all", Some("<Control><Shift>v")));
    design.append_item(&item("Reset View", "win.reset-view", None));
    menu.append_section(None, &design);

    // The side panels.
    let panels = gio::Menu::new();
    panels.append_item(&item("Show Panels", "win.toggle-panels", Some("F9")));
    panels.append_item(&item("Customizer", "win.show-customizer", Some("<Alt>1")));
    panels.append_item(&item("Check", "win.show-check", Some("<Alt>2")));
    panels.append_item(&item("Measure", "win.show-measure", Some("<Alt>3")));
    panels.append_item(&item("Reset Parameters", "win.reset-parameters", None));
    menu.append_section(None, &panels);

    let style = gio::Menu::new();
    for (label, v) in [
        ("Follow System", "system"),
        ("Light", "light"),
        ("Dark", "dark"),
    ] {
        let i = gio::MenuItem::new(Some(label), None);
        i.set_action_and_target_value(Some("app.style"), Some(&v.to_variant()));
        style.append_item(&i);
    }
    let app = gio::Menu::new();
    app.append_submenu(Some("Style"), &style);
    app.append_item(&item("Connect AI Agent…", "app.agents", None));
    app.append_item(&item(
        "Preferences",
        "app.preferences",
        Some("<Control>comma"),
    ));
    app.append_item(&item("Check for Updates", "app.check-updates", None));
    app.append_item(&item("About NeoSCAD", "app.about", None));
    app.append_item(&item("Quit", "app.quit", Some("<Control>q")));
    menu.append_section(None, &app);
    menu
}

/// The window's shortcuts, as a `GtkShortcutController` in the capture
/// phase: the editor's web view would otherwise take Ctrl+S and the other
/// keys first. Editing keys (Ctrl+Z, Ctrl+A, ...) are not listed, so they
/// reach CodeMirror.
pub fn shortcuts() -> gtk::ShortcutController {
    let c = gtk::ShortcutController::new();
    c.set_propagation_phase(gtk::PropagationPhase::Capture);
    for (trigger, action) in [
        ("<Control>n", "app.new-window"),
        ("<Control>o", "app.open"),
        ("<Control>q", "app.quit"),
        ("<Control>comma", "app.preferences"),
        ("<Control>s", "win.save"),
        ("<Control><Shift>s", "win.save-as"),
        ("<Control><Shift>e", "win.export-again"),
        ("<Control>w", "window.close"),
        ("F5", "win.preview"),
        ("F6", "win.render"),
        ("<Control><Shift>v", "win.view-all"),
        // The side panels. Alt+digit, which CodeMirror leaves alone (this
        // controller is in the capture phase, ahead of the editor).
        ("F9", "win.toggle-panels"),
        ("<Alt>1", "win.show-customizer"),
        ("<Alt>2", "win.show-check"),
        ("<Alt>3", "win.show-measure"),
    ] {
        c.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string(trigger),
            Some(gtk::NamedAction::new(action)),
        ));
    }
    c
}
