//! A window's side of an AI agent's requests (docs/agent-bridge.md,
//! "Desktop apps"), on the main loop: read the document, apply an edit
//! through the editor's `agentEdit` (one undoable, highlighted step), show
//! a place, move the camera, capture the view as shown, mark the view. The
//! link (`super::super::agent`) finds the window by its document number
//! and answers the agent with what these send their [`Reply`].
//!
//! Nothing here waits: an answer that needs the editor (the selection, an
//! edit) is sent from the editor's callback, an edit the user must approve
//! from the approval bar's buttons, and a capture after a pending preview
//! from the end of that run.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use serde_json::{Value, json};

use client::agent::{
    AgentCamera, AgentCameraChange, AgentDocumentState, AgentEditOutcome, AgentEditRequest,
    AgentRunStatus, EditorPosition, EditorSelection,
};
use client::{ViewLine, ViewMarker};
use linux_app::agent::{self as logic, CaptureSource, EditCheck, Reply};

use super::Window;

/// How long an edit waits for the user's Apply before it is declined: the
/// web page's 140 s, under the command line's 150 s, so a late Apply never
/// applies an edit the agent was told failed.
const APPROVAL_SECS: u32 = 140;

/// "Claude Code wants to change line 12-14." with Reject and Apply, under
/// the header bar while an edit waits for the user ("Ask before applying
/// an agent's edits"). A revealer of our own, as the disk notice is,
/// because an `AdwBanner` has one button and this choice needs two.
pub(super) struct ApprovalBar {
    pub(super) revealer: gtk::Revealer,
    title: gtk::Label,
    reject: gtk::Button,
    apply: gtk::Button,
}

/// An edit waiting for the user, and the timer that declines it.
pub(super) struct Pending {
    edit: AgentEditRequest,
    reply: Reply<AgentEditOutcome>,
    timer: glib::SourceId,
}

impl ApprovalBar {
    pub(super) fn new() -> ApprovalBar {
        let title = gtk::Label::builder()
            .wrap(true)
            .xalign(0.0)
            .hexpand(true)
            .build();
        let reject = gtk::Button::with_label("Reject");
        let apply = gtk::Button::with_label("Apply");
        apply.add_css_class("suggested-action");
        let row = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(12)
            .margin_top(6)
            .margin_bottom(6)
            .margin_start(12)
            .margin_end(12)
            .build();
        row.append(&title);
        row.append(&reject);
        row.append(&apply);
        let revealer = gtk::Revealer::builder()
            .child(&row)
            .reveal_child(false)
            .build();
        ApprovalBar {
            revealer,
            title,
            reject,
            apply,
        }
    }
}

/// "2 agent marks" and a clear button, over the view's top left while an
/// agent's marks show: the macOS app's `AgentMarksChip`. Without it the
/// user's only way to get rid of an agent's marks was to open another
/// document, so a marker pointing at a problem already fixed stayed in
/// the way.
pub(super) struct MarksChip {
    pub(super) root: gtk::Box,
    label: gtk::Label,
    clear: gtk::Button,
}

impl MarksChip {
    pub(super) fn new() -> MarksChip {
        let label = gtk::Label::new(None);
        label.add_css_class("caption");
        let clear = gtk::Button::builder()
            .icon_name("window-close-symbolic")
            .tooltip_text("Clear the agent\u{2019}s marks")
            .valign(gtk::Align::Center)
            .build();
        clear.add_css_class("flat");
        clear.add_css_class("circular");
        clear.update_property(&[gtk::accessible::Property::Label(
            "Clear the agent\u{2019}s marks",
        )]);
        // `osd` is libadwaita's style for controls over pictures and
        // video, legible on the light and the dark view alike.
        let root = gtk::Box::builder()
            .orientation(gtk::Orientation::Horizontal)
            .spacing(6)
            .halign(gtk::Align::Start)
            .valign(gtk::Align::Start)
            .margin_top(8)
            .margin_start(8)
            .visible(false)
            .build();
        root.add_css_class("osd");
        root.add_css_class("agent-marks");
        root.append(&super::super::agent::sparkle());
        root.append(&label);
        root.append(&clear);
        MarksChip { root, label, clear }
    }

    /// Show `text` ("2 agent marks"), or hide the chip for `None`.
    pub(super) fn show(&self, text: Option<&str>) {
        // Logged only when it changes, for linux/smoke.sh: the overlay is
        // redrawn on every pick and finding as well.
        if self.root.is_visible() != text.is_some() || self.label.text() != text.unwrap_or("") {
            glib::g_debug!("neoscad", "agent: marks chip: {}", text.unwrap_or("hidden"));
        }
        self.label.set_text(text.unwrap_or(""));
        self.root.set_visible(text.is_some());
    }
}

impl Window {
    /// Connect the approval bar's buttons and the marks chip's (from
    /// `setup`).
    pub(super) fn connect_agent(self: &Rc<Self>) {
        let me = Rc::downgrade(self);
        self.marks_chip.clear.connect_clicked(move |_| {
            if let Some(w) = me.upgrade() {
                w.clear_agent_marks();
            }
        });
        let me = Rc::downgrade(self);
        self.approval.apply.connect_clicked(move |_| {
            if let Some(w) = me.upgrade() {
                w.answer_approval(true);
            }
        });
        let me = Rc::downgrade(self);
        self.approval.reject.connect_clicked(move |_| {
            if let Some(w) = me.upgrade() {
                w.answer_approval(false);
            }
        });
        let me = Rc::downgrade(self);
        self.win.connect_is_active_notify(move |win| {
            if win.is_active()
                && let Some(w) = me.upgrade()
            {
                w.shared.agents.link.document_focused(w.agent_id);
            }
        });
    }

    pub fn agent_id(&self) -> u64 {
        self.agent_id
    }

    /// Tell the link this window's document as the title shows it, and its
    /// file once saved (cheap when nothing changed: called with the titles).
    pub(super) fn agent_register(&self) {
        let (name, path) = {
            let st = self.st.borrow();
            (
                st.doc.name(),
                st.doc.file().map(|p| p.to_string_lossy().into_owned()),
            )
        };
        self.shared
            .agents
            .link
            .document_opened(self.agent_id, &name, path.as_deref());
    }

    /// The window closed: its document leaves the agent's list, and an edit
    /// waiting for the user is dropped (the agent hears the document
    /// closed).
    pub(super) fn agent_closed(&self) {
        self.shared.agents.link.document_closed(self.agent_id);
        if let Some(p) = self.st.borrow_mut().approval.take() {
            p.timer.remove();
        }
    }

    fn agent_state(&self, selection: Option<EditorSelection>) -> AgentDocumentState {
        let st = self.st.borrow();
        AgentDocumentState {
            version: st.doc.revision(),
            text: st.doc.text.text(),
            selection,
            overrides: st.lp.overrides(),
            parts: st.lp.parts(),
            enable: st.lp.enable().to_vec(),
            run: AgentRunStatus {
                mode: st.lp.last_mode(),
                summary: st.summary.clone(),
                running: st.running || st.timer.is_some(),
            },
            console: st.console_lines.clone(),
        }
    }

    pub(in crate::app) fn agent_read(self: &Rc<Self>, reply: Reply<AgentDocumentState>) {
        let in_step = self.web.is_some() && self.st.borrow().bridge.version().is_some();
        if !in_step {
            reply.send(Ok(self.agent_state(None)));
            return;
        }
        // The text and its version are taken when the selection comes
        // back, so all three are of one moment.
        self.call("selectionPositions", &[], move |w, v| {
            reply.send(Ok(w.agent_state(selection(&v))));
        });
    }

    /// An agent's edit: at once, or after the user's Apply.
    pub(in crate::app) fn agent_edit(
        self: &Rc<Self>,
        edit: AgentEditRequest,
        ask: bool,
        reply: Reply<AgentEditOutcome>,
    ) {
        if !ask {
            self.apply_agent_edit(edit, reply);
            return;
        }
        if self.st.borrow().approval.is_some() {
            reply.send(Err(
                "an earlier edit is still waiting for the user's answer; try again after it".into(),
            ));
            return;
        }
        // A stale edit is refused now rather than after the user is asked.
        let revision = self.st.borrow().doc.revision();
        if edit.version != revision {
            reply.send(Ok(AgentEditOutcome::Stale { version: revision }));
            return;
        }
        self.approval.title.set_text(&logic::approval_text(&edit));
        self.approval.revealer.set_reveal_child(true);
        let me = Rc::downgrade(self);
        let timer = glib::timeout_add_seconds_local_once(APPROVAL_SECS, move || {
            if let Some(w) = me.upgrade() {
                // This source is done: its id is let go, not removed.
                let pending = w.st.borrow_mut().approval.take();
                if let Some(Pending { reply, .. }) = pending {
                    reply.send(Ok(AgentEditOutcome::Declined));
                }
                w.approval.revealer.set_reveal_child(false);
                glib::g_debug!("neoscad", "agent: edit not answered, declined");
            }
        });
        self.st.borrow_mut().approval = Some(Pending { edit, reply, timer });
        glib::g_debug!("neoscad", "agent: edit waits for approval");
    }

    fn answer_approval(self: &Rc<Self>, apply: bool) {
        let Some(p) = self.st.borrow_mut().approval.take() else {
            return;
        };
        p.timer.remove();
        self.approval.revealer.set_reveal_child(false);
        if apply {
            // Checked again: the user may have typed while asked.
            self.apply_agent_edit(p.edit, p.reply);
        } else {
            glib::g_debug!("neoscad", "agent: edit rejected");
            p.reply.send(Ok(AgentEditOutcome::Declined));
        }
    }

    /// Into the editor as one undoable, highlighted step, if the document
    /// is still at the version the agent read and the editor at the version
    /// the window's copy matches; checked again by the editor itself, in
    /// the same turn as the edit (`agentEdit`'s `expectVersion`). The edit comes
    /// back as an ordinary change (`changes`), which counts the revision,
    /// marks the document edited and previews it.
    fn apply_agent_edit(self: &Rc<Self>, edit: AgentEditRequest, reply: Reply<AgentEditOutcome>) {
        if self.web.is_none() {
            reply.send(Err(
                "this NeoSCAD window has no editor (its editor files were not found)".into(),
            ));
            return;
        }
        let (revision, editor_version) = {
            let st = self.st.borrow();
            (st.doc.revision(), st.bridge.version())
        };
        let editor_version = match logic::edit_check(revision, edit.version, editor_version) {
            EditCheck::Stale { version } => {
                glib::g_debug!(
                    "neoscad",
                    "agent: edit on version {} refused (at {version})",
                    edit.version
                );
                reply.send(Ok(AgentEditOutcome::Stale { version }));
                return;
            }
            EditCheck::Apply { editor_version } => editor_version,
        };
        let args = logic::agent_edit_args(editor_version, &edit.edits);
        let n = edit.edits.len();
        self.call("agentEdit", &args, move |_, answer| {
            let out = logic::edit_answer(revision, editor_version, &answer);
            glib::g_debug!("neoscad", "agent: edit of {n} changes: {out:?}");
            reply.send(Ok(out));
        });
    }

    pub(in crate::app) fn agent_reveal(
        self: &Rc<Self>,
        from: EditorPosition,
        to: EditorPosition,
        reply: Reply<()>,
    ) {
        if self.web.is_none() {
            reply.send(Err("this NeoSCAD window has no editor".into()));
            return;
        }
        let args = [from.line, from.character, to.line, to.character].map(|n| json!(n));
        self.call("revealRange", &args, move |_, _| reply.send(Ok(())));
    }

    pub(in crate::app) fn agent_camera(
        &self,
        change: &AgentCameraChange,
        reply: Reply<AgentCamera>,
    ) {
        let r = match self.view.canvas.borrow_mut().as_mut() {
            Some(c) => logic::apply_camera(&mut c.viewport, change),
            None => Err(NO_VIEW.into()),
        };
        self.view.queue();
        reply.send(r);
    }

    /// The view as shown, after the preview that is due or running (an
    /// agent's edit is followed by one): the copy is made when the run
    /// ends, so it shows the edited model.
    pub(in crate::app) fn agent_capture(
        self: &Rc<Self>,
        max_side: u32,
        reply: Reply<CaptureSource>,
    ) {
        let busy = {
            let st = self.st.borrow();
            st.running || st.timer.is_some()
        };
        if busy {
            self.st
                .borrow_mut()
                .after_run
                .push(Box::new(move |w| w.capture_now(max_side, reply)));
            // Bounded however the run ends: the agent's own wait is too.
            return;
        }
        self.capture_now(max_side, reply);
    }

    fn capture_now(&self, max_side: u32, reply: Reply<CaptureSource>) {
        let canvas = self.view.canvas.borrow();
        let Some(c) = canvas.as_ref() else {
            reply.send(Err(NO_VIEW.into()));
            return;
        };
        let (w, h) = logic::capture_size(c.viewport.size(), max_side);
        let backend = linux_app::host::gpu()
            .map(|g| format!("{:?}", g.adapter_info().backend))
            .unwrap_or_default();
        reply.send(
            c.viewport
                .copy_as_shown(w, h)
                .map(|copy| CaptureSource {
                    copy,
                    camera: logic::camera_of(&c.viewport),
                    backend,
                })
                .map_err(|e| format!("could not copy the view: {e}")),
        );
    }

    /// Run what waited for the run to end (a capture).
    pub(super) fn after_run(self: &Rc<Self>) {
        let waiting = std::mem::take(&mut self.st.borrow_mut().after_run);
        for f in waiting {
            f(self);
        }
    }

    pub(in crate::app) fn agent_annotate(
        &self,
        lines: &[ViewLine],
        markers: &[ViewMarker],
        reply: Reply<()>,
    ) {
        if self.view.canvas.borrow().is_none() {
            reply.send(Err(NO_VIEW.into()));
            return;
        }
        self.st.borrow_mut().agent_marks = logic::marks(lines, markers);
        self.update_overlay();
        reply.send(Ok(()));
    }

    /// The marks chip's clear button: the agent's layer goes, the panels'
    /// marks stay (they are the user's own, cleared from their panels).
    fn clear_agent_marks(&self) {
        self.st.borrow_mut().agent_marks = render::viewport::Annotations::default();
        glib::g_debug!("neoscad", "agent: marks cleared");
        self.update_overlay();
    }
}

const NO_VIEW: &str =
    "NeoSCAD has no 3D view on this computer (no Vulkan or OpenGL device was found)";

/// `selectionPositions()`'s `{anchor: [line, character], head}`.
fn selection(v: &Value) -> Option<EditorSelection> {
    let pos = |p: &Value| {
        Some(EditorPosition {
            line: u32::try_from(p.get(0)?.as_u64()?).ok()?,
            character: u32::try_from(p.get(1)?.as_u64()?).ok()?,
        })
    };
    Some(EditorSelection {
        anchor: pos(&v["anchor"])?,
        head: pos(&v["head"])?,
    })
}
