//! The offer to be the default app for OpenSCAD files, GTK side
//! (docs/linux-app.md, "Default app for OpenSCAD files"): the facts gio
//! knows, the launch dialog and the Preferences row. When to ask is
//! `linux_app::default_app`.
//!
//! The desktop entry is looked up among `gio::AppInfo::all()` by id rather
//! than with `GDesktopAppInfo`, which the gio crate in the tree doesn't
//! bind on this version (it moved to a separate Unix crate); the id is the
//! same one `default_for_type` reports, so the comparison is exact.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};

use linux_app::default_app::{self as logic, DESKTOP_ID, MIME_TYPE, Settings, Status};

use super::{Shared, Window};

/// How long after the first window is shown the dialog waits, so it lands
/// on a drawn window rather than appearing before it.
const ASK_DELAY: Duration = Duration::from_secs(1);

/// The offer's state, one per process (in `Shared`).
pub struct DefaultApp {
    settings: Cell<Settings>,
    path: PathBuf,
    /// The launch dialog was considered: a second window, or a file opened
    /// into the running app, must not ask again in the same run.
    considered: Cell<bool>,
}

impl DefaultApp {
    pub fn new() -> DefaultApp {
        let path = logic::settings_path(&glib::user_config_dir());
        DefaultApp {
            settings: Cell::new(Settings::load(&path)),
            path,
            considered: Cell::new(false),
        }
    }

    fn set_dont_ask(&self) {
        let s = Settings { dont_ask: true };
        self.settings.set(s);
        if let Err(e) = s.save(&self.path) {
            glib::g_warning!(
                "neoscad",
                "default app: could not save {}: {e}",
                self.path.display()
            );
        }
    }
}

/// What gio says now: looked up each time, since the user may change the
/// default in the desktop's settings while the app runs.
struct Facts {
    sandboxed: bool,
    /// Our installed desktop entry, if any.
    entry: Option<gio::AppInfo>,
    /// The current default for `MIME_TYPE`, if any.
    default: Option<gio::AppInfo>,
}

impl Facts {
    fn look() -> Facts {
        let sandboxed = logic::sandboxed(
            std::path::Path::new("/.flatpak-info").exists(),
            std::env::var_os("FLATPAK_ID").is_some(),
            std::env::var_os("SNAP").is_some(),
        );
        let entry = gio::AppInfo::all()
            .into_iter()
            .find(|a| a.id().as_deref() == Some(DESKTOP_ID));
        let default = gio::AppInfo::default_for_type(MIME_TYPE, false);
        Facts {
            sandboxed,
            entry,
            default,
        }
    }

    fn is_default(&self) -> bool {
        logic::is_default(self.default.as_ref().and_then(|d| d.id()).as_deref())
    }

    fn status(&self) -> Status {
        Status::new(
            self.sandboxed,
            self.entry.is_some(),
            self.is_default(),
            self.default.as_ref().map(|d| d.display_name().to_string()),
        )
    }
}

/// Make our entry the default; the error's text if gio refused.
fn make_default(facts: &Facts) -> Result<(), String> {
    let Some(entry) = &facts.entry else {
        return Err("NeoSCAD's desktop entry isn't installed".into());
    };
    entry
        .set_as_default_for_type(MIME_TYPE)
        .map_err(|e| e.to_string())
}

/// Called each time a window is shown at launch or on open; the first call
/// of the process decides, once, whether to ask. Nothing else in the app
/// opens a dialog by itself at launch (updates show a banner, and agents'
/// consent waits for the user), and the dialog is skipped if one is
/// already up when it is due, so the two never stack.
pub fn after_launch(sh: &Rc<Shared>, window: &Rc<Window>) {
    if sh.default_app.considered.replace(true) {
        return;
    }
    let (weak, win) = (Rc::downgrade(sh), Rc::downgrade(window));
    glib::timeout_add_local_once(ASK_DELAY, move || {
        let (Some(sh), Some(window)) = (weak.upgrade(), win.upgrade()) else {
            return;
        };
        let facts = Facts::look();
        let suppressed = logic::suppressed(
            sh.default_app.settings.get().dont_ask,
            std::env::var_os(logic::NO_PROMPT_ENV).is_some(),
            std::env::var_os("CI").is_some(),
        );
        let ask = logic::should_ask(
            facts.sandboxed,
            facts.entry.is_some(),
            facts.is_default(),
            suppressed,
        );
        glib::g_debug!(
            "neoscad",
            "default app: sandboxed {}, entry {}, default {:?}, suppressed {suppressed}: {}",
            facts.sandboxed,
            facts.entry.is_some(),
            facts.default.as_ref().and_then(|d| d.id()),
            if ask { "asking" } else { "not asking" }
        );
        if !ask || window.widget().visible_dialog().is_some() {
            return;
        }
        ask_now(&sh, &window, facts);
    });
}

fn ask_now(sh: &Rc<Shared>, window: &Rc<Window>, facts: Facts) {
    let dialog = adw::AlertDialog::new(
        Some("Make NeoSCAD the default app for OpenSCAD files?"),
        Some(
            "Opening a .scad file will open it in NeoSCAD. You can change this \
             later in your desktop's settings.",
        ),
    );
    dialog.add_responses(&[("later", "_Not Now"), ("default", "_Make Default")]);
    dialog.set_response_appearance("default", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("default"));
    dialog.set_close_response("later");
    let dont_ask = gtk::CheckButton::with_mnemonic("_Don't ask again");
    dialog.set_extra_child(Some(&dont_ask));
    let (weak, win) = (Rc::downgrade(sh), Rc::downgrade(window));
    dialog.connect_response(None, move |_, response| {
        let Some(sh) = weak.upgrade() else { return };
        // Kept whichever button closed the dialog: "Not Now" alone asks
        // again next launch, with the tick it never does.
        if dont_ask.is_active() {
            sh.default_app.set_dont_ask();
        }
        if response != "default" {
            return;
        }
        let message = match make_default(&facts) {
            Ok(()) => "NeoSCAD now opens OpenSCAD files".to_string(),
            Err(e) => {
                glib::g_warning!("neoscad", "default app: {e}");
                format!("The default app for OpenSCAD files didn't change: {e}")
            }
        };
        if let Some(w) = win.upgrade() {
            w.toast(&message);
        }
    });
    dialog.present(Some(window.widget()));
}

/// Preferences > General > OpenSCAD Files: whether NeoSCAD is the default
/// app, with "Make Default" when it isn't and the system lets it be.
pub fn group() -> adw::PreferencesGroup {
    let row = adw::ActionRow::builder()
        .title("Default app for OpenSCAD files")
        .build();
    let button = gtk::Button::builder()
        .label("Make Default")
        .valign(gtk::Align::Center)
        .build();
    row.add_suffix(&button);
    let show = {
        let (row, button) = (row.downgrade(), button.downgrade());
        move |status: &Status| {
            if let (Some(row), Some(button)) = (row.upgrade(), button.upgrade()) {
                row.set_subtitle(&status.message());
                button.set_visible(status.offers_button());
            }
        }
    };
    show(&Facts::look().status());
    button.connect_clicked({
        let row = row.downgrade();
        move |_| {
            let facts = Facts::look();
            match make_default(&facts) {
                Ok(()) => show(&Facts::look().status()),
                Err(e) => {
                    glib::g_warning!("neoscad", "default app: {e}");
                    show(&facts.status());
                    if let Some(row) = row.upgrade() {
                        row.set_subtitle(&format!(
                            "The default app for OpenSCAD files didn't change: {e}"
                        ));
                    }
                }
            }
        }
    });
    let group = adw::PreferencesGroup::builder()
        .title("OpenSCAD Files")
        .build();
    group.add(&row);
    group
}
