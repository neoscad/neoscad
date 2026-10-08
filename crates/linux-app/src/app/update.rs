//! The update check's GTK side (docs/linux-app.md, "Updates"): when to
//! check, the fetch, the banner and its details, and the preferences.
//! What a feed means and what the notice says are `linux_app::update`;
//! whether the feed is genuine is `client::update::check`.
//!
//! An automatic check runs ten seconds after start-up and then on an
//! hourly tick, each time only when a day has passed since the last one
//! (`Settings::due`), so a long session still hears about a release. It is
//! silent whatever happens: offline, a feed between releases or a bad
//! signature is not the user's problem, and is only logged
//! (`G_MESSAGES_DEBUG=neoscad`). Main menu > Check for Updates does the
//! same at once, and says what it found, including a failure.
//!
//! The request is a plain GET of two static files (`docs/privacy.md`):
//! libsoup's session has no cookie jar unless one is added, and the
//! User-Agent is the bare word `neoscad`.

use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use adw::prelude::*;
use gtk::glib::translate::IntoGlib;
use gtk::{gio, glib};
use soup::prelude::*;

use linux_app::update::{self, Install, Notice, Outcome, Settings};

use super::Shared;

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The first automatic check waits this long after start-up, so it never
/// competes with opening the first window.
const FIRST_CHECK_SECS: u32 = 10;
/// How often to ask whether a day has passed.
const TICK_SECS: u32 = 60 * 60;
/// A request that takes longer than this is abandoned.
const TIMEOUT_SECS: u32 = 10;

/// The app's update state, one per process (in `Shared`).
pub struct Updates {
    settings: RefCell<Settings>,
    path: PathBuf,
    install: Install,
    /// The newer release the banners show, if any.
    notice: RefCell<Option<Notice>>,
    /// A check is running: a second one (the menu during an automatic
    /// one) waits for it rather than racing it for the serial.
    checking: Cell<bool>,
}

impl Updates {
    pub fn new() -> Updates {
        let path = update::settings_path(&glib::user_config_dir());
        Updates {
            settings: RefCell::new(Settings::load(&path)),
            path,
            install: Install::detect(Path::new("/.flatpak-info").exists()),
            notice: RefCell::default(),
            checking: Cell::new(false),
        }
    }

    pub fn notice(&self) -> Option<Notice> {
        self.notice.borrow().clone()
    }

    fn change(&self, f: impl FnOnce(&mut Settings)) {
        f(&mut self.settings.borrow_mut());
        if let Err(e) = self.settings.borrow().save(&self.path) {
            glib::g_warning!(
                "neoscad",
                "update: could not save {}: {e}",
                self.path.display()
            );
        }
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// Automatic checks are off for this process: `NEOSCAD_NO_UPDATE_CHECK`,
/// or CI (`CI`), where the smoke test runs the app.
fn automatic_disabled() -> bool {
    std::env::var_os(update::NO_CHECK_ENV).is_some() || std::env::var_os("CI").is_some()
}

/// Schedule the automatic checks; called once, at start-up.
pub fn start(sh: &Rc<Shared>) {
    glib::g_debug!(
        "neoscad",
        "update: {:?} install, {:?}",
        sh.updates.install,
        sh.updates.settings.borrow()
    );
    if automatic_disabled() {
        glib::g_debug!("neoscad", "update: automatic checks off for this process");
        return;
    }
    let weak = Rc::downgrade(sh);
    glib::timeout_add_seconds_local_once(FIRST_CHECK_SECS, {
        let weak = weak.clone();
        move || auto_check(&weak)
    });
    glib::timeout_add_seconds_local(TICK_SECS, move || {
        if weak.upgrade().is_none() {
            return glib::ControlFlow::Break;
        }
        auto_check(&weak);
        glib::ControlFlow::Continue
    });
}

/// An automatic check, when one is due.
fn auto_check(weak: &Weak<Shared>) {
    let Some(sh) = weak.upgrade() else { return };
    let now = now();
    if !sh.updates.settings.borrow().due(now) || sh.updates.checking.get() {
        return;
    }
    // A build with no trusted key (none exists before the release key is
    // made) could verify nothing: don't ask the network at all.
    if client::update::trusted_keys().is_empty() {
        glib::g_debug!("neoscad", "update: no feed key in this build; not checking");
        return;
    }
    sh.updates.change(|s| s.checked = now);
    glib::spawn_future_local(check(sh, false));
}

/// Main menu > Check for Updates.
pub fn check_now(sh: &Rc<Shared>) {
    if sh.updates.checking.get() {
        return;
    }
    if client::update::trusted_keys().is_empty() {
        tell(
            sh,
            "This build can't check for updates: it has no update key",
        );
        return;
    }
    glib::spawn_future_local(check(sh.clone(), true));
}

/// Fetch, verify, remember and show. `manual` says what was found.
async fn check(sh: Rc<Shared>, manual: bool) {
    let u = &sh.updates;
    u.checking.set(true);
    let result = fetch_and_evaluate(u).await;
    u.checking.set(false);
    match result {
        Ok(Outcome::Available(n)) => {
            glib::g_debug!(
                "neoscad",
                "update: {} available ({:?})",
                n.version,
                n.install
            );
            if update::should_show(&n, &u.settings.borrow(), manual) {
                set_notice(&sh, Some(n));
            }
        }
        Ok(Outcome::UpToDate { latest }) => {
            glib::g_debug!("neoscad", "update: up to date (the feed has {latest})");
            set_notice(&sh, None);
            if manual {
                tell(&sh, &format!("NeoSCAD {VERSION} is up to date"));
            }
        }
        Err(e) => {
            glib::g_debug!("neoscad", "update: refused or failed: {e}");
            if manual {
                tell(&sh, &format!("Could not check for updates: {e}"));
            }
        }
    }
}

async fn fetch_and_evaluate(u: &Updates) -> Result<Outcome, String> {
    let custom = std::env::var(update::FEED_URL_ENV).ok();
    let base = update::feed_base(custom.as_deref())?;
    let channel = u.settings.borrow().channel();
    let (feed_url, sig_url) = update::feed_urls(&base, channel);
    let session = soup::Session::new();
    session.set_user_agent("neoscad");
    session.set_timeout(TIMEOUT_SECS);
    let feed = get(&session, &feed_url).await?;
    let sig = get(&session, &sig_url).await?;
    let mut settings = u.settings.borrow().clone();
    let outcome = update::evaluate(
        &mut settings,
        &client::update::trusted_keys(),
        &feed,
        &sig,
        VERSION,
        u.install,
        std::env::consts::ARCH,
    )
    .map_err(|e| e.to_string())?;
    // Only the serial changes here; the user may have flipped a setting
    // while the request was out, so it is copied rather than the whole.
    u.change(|s| {
        s.stable_serial = settings.stable_serial;
        s.rc_serial = settings.rc_serial;
    });
    Ok(outcome)
}

/// One small file, read up to `MAX_FEED_BYTES`.
async fn get(session: &soup::Session, url: &str) -> Result<Vec<u8>, String> {
    let msg = soup::Message::new("GET", url).map_err(|e| format!("{url}: {e}"))?;
    let stream = session
        .send_future(&msg, glib::Priority::DEFAULT)
        .await
        .map_err(|e| format!("{url}: {e}"))?;
    if msg.status() != soup::Status::Ok {
        return Err(format!("{url}: HTTP {}", msg.status().into_glib()));
    }
    let mut out = Vec::new();
    loop {
        let chunk = stream
            .read_bytes_future(16 * 1024, glib::Priority::DEFAULT)
            .await
            .map_err(|e| format!("{url}: {e}"))?;
        if chunk.is_empty() {
            break;
        }
        out.extend_from_slice(&chunk);
        if out.len() > update::MAX_FEED_BYTES {
            return Err(format!("{url}: larger than a feed"));
        }
    }
    Ok(out)
}

/// The notice every window's banner shows (new windows ask for it).
fn set_notice(sh: &Shared, notice: Option<Notice>) {
    for w in sh.windows() {
        w.show_update(notice.as_ref());
    }
    *sh.updates.notice.borrow_mut() = notice;
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

/// The banner's Details: how to update this install, with the download
/// (a Flatpak's bundle) or the release page, and Later.
pub fn show_details(sh: &Rc<Shared>, parent: &gtk::Window) {
    let Some(n) = sh.updates.notice() else { return };
    let dialog = adw::AlertDialog::new(Some(&n.title()), Some(&n.body()));
    dialog.add_response("later", "Later");
    if n.download_url() != n.release_url {
        dialog.add_response("notes", "Release Notes");
    }
    dialog.add_response("download", n.download_label());
    dialog.set_response_appearance("download", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("download"));
    dialog.set_close_response("later");
    let (weak, parent_weak) = (Rc::downgrade(sh), parent.downgrade());
    dialog.connect_response(None, move |_, response| {
        let Some(sh) = weak.upgrade() else { return };
        let url = match response {
            "download" => n.download_url().to_string(),
            "notes" => n.release_url.clone(),
            _ => {
                // Put off: automatic checks don't show this version again.
                sh.updates.change(|s| s.dismissed = Some(n.version.clone()));
                set_notice(&sh, None);
                return;
            }
        };
        // Through the OpenURI portal inside the Flatpak: the browser
        // downloads the bundle, and Software opens it.
        let parent = parent_weak.upgrade();
        gtk::UriLauncher::new(&url).launch(parent.as_ref(), gio::Cancellable::NONE, |r| {
            if let Err(e) = r {
                glib::g_warning!("neoscad", "update: could not open the link: {e}");
            }
        });
    });
    dialog.present(Some(parent));
}

/// Main menu > Preferences: the two update settings, the Language page
/// (NeoSCAD's extensions, `linux_app::extensions`), and the Agents page
/// (`super::agent::page`).
pub fn preferences(sh: &Rc<Shared>, parent: Option<&gtk::Window>) {
    let s = sh.updates.settings.borrow().clone();
    let automatic = adw::SwitchRow::builder()
        .title("Check for updates automatically")
        .subtitle(
            "About once a day, ask neoscad.org whether a newer release exists. \
             The request sends nothing that identifies you or this computer.",
        )
        .active(s.automatic)
        .build();
    let rc = adw::SwitchRow::builder()
        .title("Receive release candidates")
        .subtitle("Also be told about test versions of the next release")
        .active(s.rc)
        .build();
    let weak = Rc::downgrade(sh);
    automatic.connect_active_notify(move |row| {
        if let Some(sh) = weak.upgrade() {
            sh.updates.change(|s| s.automatic = row.is_active());
            if row.is_active() && !automatic_disabled() {
                auto_check(&Rc::downgrade(&sh));
            }
        }
    });
    let weak = Rc::downgrade(sh);
    rc.connect_active_notify(move |row| {
        if let Some(sh) = weak.upgrade() {
            // Another channel: what the old one offered no longer
            // applies, and its check is due now rather than tomorrow.
            sh.updates.change(|s| {
                s.rc = row.is_active();
                s.checked = 0;
                s.dismissed = None;
            });
            set_notice(&sh, None);
            if !automatic_disabled() {
                auto_check(&Rc::downgrade(&sh));
            }
        }
    });
    let group = adw::PreferencesGroup::builder().title("Updates").build();
    group.add(&automatic);
    group.add(&rc);
    let page = adw::PreferencesPage::builder()
        .title("General")
        .icon_name("preferences-system-symbolic")
        .build();
    page.add(&group);
    let dialog = adw::PreferencesDialog::new();
    dialog.add(&page);
    dialog.add(&language_page(sh));
    dialog.add(&super::agent::page(sh));
    dialog.present(parent);
}

/// Preferences > Language: NeoSCAD's extensions to the OpenSCAD language,
/// as the macOS app's Settings > Language has them. Off by default: off, a
/// file means what it means in OpenSCAD.
fn language_page(sh: &Rc<Shared>) -> adw::PreferencesPage {
    let on = *sh.extensions.borrow();
    let sketch = adw::SwitchRow::builder()
        .title("Constrained sketches (sketch)")
        .subtitle(
            "Points, lines, arcs and circles tied by constraints and solved into a 2D shape, \
             like the command line's --enable sketch. Off, sketch() is an unknown module \
             as in OpenSCAD.",
        )
        .active(on.sketch)
        .build();
    let query = adw::SwitchRow::builder()
        .title("Geometry queries (query)")
        .subtitle(
            "Bounding boxes, measurements, distances and named anchors of a module's \
             children as values (child_bounds() and the like), like the command line's \
             --enable query. Off, they are unknown functions as in OpenSCAD.",
        )
        .active(on.query)
        .build();
    let exact = adw::SwitchRow::builder()
        .title("Exact STEP export (exact)")
        .subtitle(
            "File > Export writes STEP whose cylinders, spheres, cones and tori are true \
             surfaces rather than triangles, for CAD programs such as FreeCAD, like the \
             command line's --enable exact. Curves are exact unless $fn is set; anything \
             else is written as facets and listed after the export.",
        )
        .active(on.exact)
        .build();
    let fillet = adw::SwitchRow::builder()
        .title("Edge fillets and chamfers (fillet)")
        .subtitle(
            "fillet_edges() and chamfer_edges() round or bevel the edges of any solid that a \
             selector picks (\"|z\" for vertical edges, \">z\" for the top outline), as FreeCAD's \
             and CadQuery's fillets do, like the command line's --enable fillet. Off, they are \
             unknown modules as in OpenSCAD.",
        )
        .active(on.fillet)
        .build();
    let weak = Rc::downgrade(sh);
    fillet.connect_active_notify(move |row| {
        if let Some(sh) = weak.upgrade() {
            let mut s = *sh.extensions.borrow();
            s.fillet = row.is_active();
            sh.set_extensions(s);
        }
    });
    let weak = Rc::downgrade(sh);
    exact.connect_active_notify(move |row| {
        if let Some(sh) = weak.upgrade() {
            let mut s = *sh.extensions.borrow();
            s.exact = row.is_active();
            sh.set_extensions(s);
        }
    });
    let weak = Rc::downgrade(sh);
    sketch.connect_active_notify(move |row| {
        if let Some(sh) = weak.upgrade() {
            let mut s = *sh.extensions.borrow();
            s.sketch = row.is_active();
            sh.set_extensions(s);
        }
    });
    let weak = Rc::downgrade(sh);
    query.connect_active_notify(move |row| {
        if let Some(sh) = weak.upgrade() {
            let mut s = *sh.extensions.borrow();
            s.query = row.is_active();
            sh.set_extensions(s);
        }
    });
    let group = adw::PreferencesGroup::builder()
        .title("NeoSCAD extensions")
        .description("Not in OpenSCAD; every open document runs again when one changes.")
        .build();
    group.add(&sketch);
    group.add(&query);
    group.add(&exact);
    group.add(&fillet);
    let page = adw::PreferencesPage::builder()
        .title("Language")
        .icon_name("accessories-text-editor-symbolic")
        .build();
    page.add(&group);
    page
}
