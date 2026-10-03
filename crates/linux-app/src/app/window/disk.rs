//! The window's own file, changed by another program (an AI agent through
//! `neoscad mcp`, another editor): what `client::DiskTracker` decides,
//! carried out in GTK (`docs/audits/agent-connection-desktop.md`,
//! finding 1).
//!
//! - A clean document takes the change in place, through the editor's
//!   `agentEdit`: one undoable step, highlighted. A plain reload would
//!   clear the editor's undo history, so the user could neither see what
//!   changed nor take it back.
//! - A document with unsaved changes shows a bar under the header ("The
//!   file changed on disk": Keep Mine / Reload), as GNOME Text Editor does
//!   for the same case. Save then asks before overwriting.
//! - A file deleted or moved away is said so; saving writes it again.
//!
//! The bar is a revealer of our own rather than an `AdwBanner`: a banner
//! has one button, and this choice needs two.

use std::path::PathBuf;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use client::{DiskAction, ReloadEdit, SaveCheck};
use gtk::{gio, glib};
use serde_json::Value;

use super::Window;

/// The pause after the watcher's event before the file is read: one save
/// is several events, and a program may write in several steps.
const SETTLE_MS: u64 = 100;

/// The bar shown while the file differs from the window's copy.
pub(super) struct DiskNotice {
    pub(super) revealer: gtk::Revealer,
    title: gtk::Label,
    keep: gtk::Button,
    reload: gtk::Button,
}

impl DiskNotice {
    pub(super) fn new() -> DiskNotice {
        let title = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .hexpand(true)
            .build();
        let keep = gtk::Button::with_label("Keep Mine");
        let reload = gtk::Button::with_label("Reload");
        reload.add_css_class("suggested-action");
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(12)
            .margin_end(12)
            .build();
        row.append(&title);
        row.append(&keep);
        row.append(&reload);
        let revealer = gtk::Revealer::builder()
            .child(&row)
            .reveal_child(false)
            .build();
        DiskNotice {
            revealer,
            title,
            keep,
            reload,
        }
    }

    fn show_changed(&self, reloadable: bool) {
        self.title.set_text(if reloadable {
            "The file changed on disk. Reload it and lose your unsaved changes, or keep yours?"
        } else {
            "The file changed on disk, and is no longer UTF-8 text. Saving will replace it."
        });
        self.keep.set_label("Keep Mine");
        self.reload.set_visible(reloadable);
        self.revealer.set_reveal_child(true);
    }

    fn show_missing(&self) {
        self.title
            .set_text("The file was deleted or moved on disk. Saving will write it again.");
        self.keep.set_label("Dismiss");
        self.reload.set_visible(false);
        self.revealer.set_reveal_child(true);
    }

    fn hide(&self) {
        self.revealer.set_reveal_child(false);
    }
}

impl Window {
    /// Connect the bar's buttons (from `setup`).
    pub(super) fn connect_disk_notice(self: &Rc<Self>) {
        let me = Rc::downgrade(self);
        self.notice.keep.connect_clicked(move |_| {
            if let Some(w) = me.upgrade() {
                w.st.borrow_mut().doc.keep_mine();
                w.notice.hide();
            }
        });
        let me = Rc::downgrade(self);
        self.notice.reload.connect_clicked(move |_| {
            if let Some(w) = me.upgrade() {
                w.reload_from_disk();
            }
        });
    }

    /// The files to watch: the last run's (`deps`) and the document's own.
    pub(super) fn watched_paths(&self, deps: Vec<PathBuf>) -> Vec<PathBuf> {
        let mut st = self.st.borrow_mut();
        st.deps = deps.clone();
        let mut all = deps;
        if let Some(f) = st.doc.file() {
            all.push(f.to_path_buf());
        }
        all
    }

    /// The document's own file changed, or was replaced or removed: read
    /// it after a pause (once for a burst of events).
    pub(super) fn disk_changed(self: &Rc<Self>) {
        self.schedule_disk_check(SETTLE_MS);
    }

    fn schedule_disk_check(self: &Rc<Self>, ms: u64) {
        let mut st = self.st.borrow_mut();
        if st.disk_timer.is_some() {
            return;
        }
        let me = Rc::downgrade(self);
        st.disk_timer = Some(glib::timeout_add_local_once(
            Duration::from_millis(ms),
            move || {
                if let Some(w) = me.upgrade() {
                    w.st.borrow_mut().disk_timer = None;
                    w.disk_check();
                }
            },
        ));
    }

    fn disk_check(self: &Rc<Self>) {
        let action = self.st.borrow_mut().doc.disk_check();
        // For the log (linux/smoke.sh): what the change meant.
        glib::g_debug!(
            "neoscad",
            "disk: {}",
            match &action {
                DiskAction::None => "no change".to_string(),
                DiskAction::ReadAgain { .. } => "read again".into(),
                DiskAction::Saved => "the file holds the text".into(),
                DiskAction::Reload { edits } => format!("reload ({} edits)", edits.len()),
                DiskAction::Conflict { .. } => "changed under unsaved edits".into(),
                DiskAction::Missing => "missing".into(),
                DiskAction::Resolved => "back as saved".into(),
            }
        );
        match action {
            DiskAction::None => {}
            DiskAction::ReadAgain { ms } => self.schedule_disk_check(ms),
            DiskAction::Saved | DiskAction::Resolved => {
                self.notice.hide();
                self.update_titles();
            }
            DiskAction::Reload { edits } => self.apply_reload(edits),
            DiskAction::Conflict { reloadable } => self.notice.show_changed(reloadable),
            DiskAction::Missing => self.notice.show_missing(),
        }
    }

    /// The bar's Reload (or the save dialog's): the file's text replaces
    /// the window's, as one undoable step.
    fn reload_from_disk(self: &Rc<Self>) {
        let edits = self.st.borrow_mut().doc.disk_reload();
        match edits {
            Some(edits) => self.apply_reload(edits),
            None => self.toast("The file on disk cannot be read as text."),
        }
    }

    /// Apply a reload's edits through the editor, which reports them back
    /// as a change (`changes`, then `Document::reload_landed`); without an
    /// editor that holds the current text, to the copy, which the editor
    /// then loads.
    fn apply_reload(self: &Rc<Self>, edits: Vec<ReloadEdit>) {
        self.notice.hide();
        let in_step = self.web.is_some() && self.st.borrow().bridge.version().is_some();
        if in_step {
            let args: Value =
                serde_json::from_str(&client::agent_edit_json(&edits)).unwrap_or_default();
            self.call("agentEdit", &[args], |_, _| {});
            return;
        }
        let ready = {
            let mut st = self.st.borrow_mut();
            st.doc.reload_without_editor(&edits);
            st.lp.text_replaced();
            st.bridge.is_ready()
        };
        if ready {
            // A load was on its way with the old text.
            self.load_editor();
        }
        self.update_titles();
        self.schedule();
    }

    /// After a transaction reached the copy: a reload landing makes the
    /// document clean again.
    pub(super) fn check_reload_landed(&self) {
        let landed = self.st.borrow_mut().doc.reload_landed();
        if landed {
            self.notice.hide();
        }
    }

    /// Save to the document's file, asking first if another program
    /// changed it since it was read or saved here; then `then(saved)`.
    pub(super) fn save_to_file(
        self: &Rc<Self>,
        path: PathBuf,
        then: Option<Box<dyn FnOnce(bool)>>,
    ) {
        let check = self.st.borrow().doc.save_check();
        if check == SaveCheck::Write {
            let ok = self.write_to(path);
            if let Some(t) = then {
                t(ok);
            }
            return;
        }
        let name = self.st.borrow().doc.name();
        let dialog = adw::AlertDialog::new(
            Some(&format!("“{name}” Changed on Disk")),
            Some(
                "Another program changed the file since it was opened or saved here. \
                 Saving replaces its changes with yours.",
            ),
        );
        dialog.add_responses(&[
            ("cancel", "_Cancel"),
            ("reload", "_Reload"),
            ("save", "_Save Anyway"),
        ]);
        dialog.set_response_appearance("save", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let me = Rc::downgrade(self);
        dialog.choose(Some(&self.win), gio::Cancellable::NONE, move |response| {
            let Some(w) = me.upgrade() else { return };
            let ok = match response.as_str() {
                "save" => w.write_to(path),
                "reload" => {
                    w.reload_from_disk();
                    false
                }
                _ => false,
            };
            if let Some(t) = then {
                t(ok);
            }
        });
    }

    /// A new document, or the same one under a new name: no notice
    /// carries over, and its own file is watched.
    pub(super) fn disk_reset(self: &Rc<Self>) {
        if let Some(t) = self.st.borrow_mut().disk_timer.take() {
            t.remove();
        }
        self.notice.hide();
        let deps = self.st.borrow().deps.clone();
        self.watch_files(deps);
    }
}
