//! A library file, read-only, in a window of its own: where going to a
//! definition in BOSL2 or MCAD lands (`language::Target::Library`), as
//! the macOS app's `LibraryViewer` shows one beside the window it was
//! reached from. Editing a library in place from a jump would change
//! every model that uses it, so the editor is read-only.
//!
//! The viewer has an editor page and a language server of its own, so
//! hover and go to definition work inside library code too; its server
//! is never supplied a run, so it shows no markers for code the user
//! cannot change and never evaluates the file. One viewer per file:
//! jumping to a file already shown brings it forward and moves the
//! cursor.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};

use adw::prelude::*;
use gtk::glib;
use serde_json::{Value, json};
use webkit6::prelude::WebViewExt;

use linux_app::bridge::{self, Incoming};
use linux_app::language::{self, Language};

use super::Shared;
use super::editor;

pub struct LibraryViewer {
    win: adw::ApplicationWindow,
    web: webkit6::WebView,
    path: PathBuf,
    text: String,
    shared: Rc<Shared>,
    st: RefCell<State>,
}

struct State {
    language: Language,
    /// The page has loaded the text (a reveal waits for it).
    loaded: bool,
    reveal: Option<(u64, u64)>,
}

impl std::fmt::Debug for LibraryViewer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LibraryViewer")
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

impl LibraryViewer {
    /// A viewer of `path`, its text read through the core (the bundled
    /// MCAD exists only in memory).
    pub fn new(
        app: &adw::Application,
        shared: Rc<Shared>,
        path: PathBuf,
        library_dirs: &[String],
    ) -> Result<Rc<LibraryViewer>, String> {
        if shared.editor_dir.is_none() {
            return Err("the editor bundle was not found".into());
        }
        let text = shared
            .client
            .read_file(&path.to_string_lossy())
            .map_err(|e| e.to_string())?;
        let title = adw::WindowTitle::new(
            &language::display_path(&path, library_dirs),
            "Read-only library file",
        );
        let header = adw::HeaderBar::builder().title_widget(&title).build();
        let viewer = Rc::new_cyclic(|me: &Weak<LibraryViewer>| {
            let m = me.clone();
            let web = editor::web_view(move |msg| {
                if let Some(v) = m.upgrade() {
                    v.on_message(&msg);
                }
            });
            let toolbar = adw::ToolbarView::new();
            toolbar.add_top_bar(&header);
            toolbar.set_content(Some(&web));
            let win = adw::ApplicationWindow::builder()
                .application(app)
                .default_width(760)
                .default_height(800)
                .content(&toolbar)
                .title(path.to_string_lossy().as_ref())
                .build();
            let language = start_language(&shared, me.clone());
            LibraryViewer {
                win,
                web,
                path,
                text,
                shared,
                st: RefCell::new(State {
                    language,
                    loaded: false,
                    reveal: None,
                }),
            }
        });
        let (me, shared) = (Rc::downgrade(&viewer), viewer.shared.clone());
        viewer.win.connect_destroy(move |_| {
            if let Some(v) = me.upgrade() {
                v.st.borrow().language.stop();
            }
            shared.forget_viewer(&me);
        });
        let me = Rc::downgrade(&viewer);
        viewer.web.connect_web_process_terminated(move |web, _| {
            if let Some(v) = me.upgrade() {
                // The new page initializes a new client: it needs a new
                // server, since a server answers `initialize` once.
                let mut st = v.st.borrow_mut();
                st.language.stop();
                st.language = start_language(&v.shared, Rc::downgrade(&v));
                st.loaded = false;
            }
            web.load_uri(linux_app::resources::PAGE_URL);
        });
        Ok(viewer)
    }

    pub fn widget(&self) -> &adw::ApplicationWindow {
        &self.win
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Put the cursor at a 0-based line and UTF-16 column, now or once
    /// the page has the text.
    pub fn reveal_at(self: &Rc<Self>, line: u64, character: u64) {
        let loaded = {
            let mut st = self.st.borrow_mut();
            st.reveal = Some((line, character));
            st.loaded
        };
        if loaded {
            self.flush_reveal();
        }
    }

    fn flush_reveal(self: &Rc<Self>) {
        let Some((line, character)) = self.st.borrow_mut().reveal.take() else {
            return;
        };
        editor::call(
            &self.web,
            "reveal",
            &[json!(line), json!(character)],
            |_| {},
        );
    }

    fn on_message(self: &Rc<Self>, m: &Value) {
        let Some(msg) = bridge::parse(m) else { return };
        match msg {
            Incoming::Ready => {
                let uri = language::document_uri(&self.path.to_string_lossy());
                let me = Rc::downgrade(self);
                editor::call(
                    &self.web,
                    "load",
                    &[json!(self.text), json!(uri), json!(true)],
                    move |_| {
                        if let Some(v) = me.upgrade() {
                            glib::g_debug!("neoscad", "library viewer: {}", v.path.display());
                            v.st.borrow_mut().loaded = true;
                            v.flush_reveal();
                        }
                    },
                );
            }
            Incoming::Lsp(message) => {
                glib::g_debug!(
                    "neoscad",
                    "lsp (library): page -> server {}",
                    language::describe(&message)
                );
                let ls = self.st.borrow().language.clone();
                ls.send(message);
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
                    glib::g_warning!("neoscad", "{e}");
                }
            }
            Incoming::Log { message } => glib::g_warning!("neoscad", "library editor: {message}"),
            // Read-only: no changes; the app's keys (F5, F6) have no
            // model to run here.
            Incoming::Changes(_) | Incoming::Command(_) | Incoming::Unknown(_) => {}
        }
    }
}

/// The viewer's server, delivering to its page whether or not it has
/// said `ready` (the page's client sends `initialize` before that).
fn start_language(shared: &Rc<Shared>, me: Weak<LibraryViewer>) -> Language {
    shared.language(move |m| {
        if let Some(v) = me.upgrade() {
            editor::call(&v.web, "lspReceive", &[json!(m)], |_| {});
        }
    })
}
