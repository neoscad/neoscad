//! The check and measure panels (the macOS app's `CheckPanel.swift` and
//! `MeasurePanel.swift`, the parts this milestone ports).
//!
//! Check: `neoscad check` on the document for a printer (a preset, or
//! check's own defaults), its summary line and the findings, each with its
//! severity, message and fix. Activating a finding marks it in the 3D view
//! (its numbered ring and box, as `snapshot --issues` numbers them) and
//! turns the view to it; activating it again clears the mark.
//!
//! Measure: `neoscad measure` on the document (volume, area and size),
//! then two points picked on the model's surface in the view and the
//! distance between them, drawn in the view.
//!
//! Like the customizer, the panels hold no state of their own: they ask
//! the window ([`Request`]) and are shown its results.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;

use client::{CheckReport, FindingSeverity, MeasureReport};
use linux_app::inspect as logic;

#[derive(Debug, Clone)]
pub enum Request {
    Check,
    /// A printer preset's id (`client::CUSTOM_PRINTER` for check's own
    /// numbers).
    Printer(String),
    SelectFinding(u32),
    Measure,
    Picking(bool),
    ClearPicks,
}

fn page_box() -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Vertical, 12);
    b.set_margin_top(12);
    b.set_margin_bottom(12);
    b.set_margin_start(12);
    b.set_margin_end(12);
    b
}

fn note(text: &str) -> gtk::Label {
    let l = gtk::Label::builder()
        .label(text)
        .wrap(true)
        .xalign(0.0)
        .selectable(true)
        .build();
    l.add_css_class("dim-label");
    l
}

pub struct CheckPanel {
    pub root: gtk::Box,
    run: gtk::Button,
    spinner: gtk::Spinner,
    summary: gtk::Label,
    list: gtk::ListBox,
    ids: RefCell<Vec<u32>>,
    /// The Check button had the keyboard when the check started: the
    /// report's first finding takes it (the button is insensitive while
    /// a check runs, which loses the focus). Typing elsewhere meanwhile
    /// keeps it there.
    had_focus: Cell<bool>,
}

impl std::fmt::Debug for CheckPanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CheckPanel").finish_non_exhaustive()
    }
}

impl CheckPanel {
    pub fn new(request: impl Fn(Request) + 'static) -> Rc<CheckPanel> {
        let request: Rc<dyn Fn(Request)> = Rc::new(request);
        let presets = client::printer_presets();
        let mut labels = vec!["Default (0.4 mm nozzle, no bed)".to_string()];
        labels.extend(presets.iter().map(|p| p.name.clone()));
        let labels: Vec<&str> = labels.iter().map(String::as_str).collect();
        let printer = gtk::DropDown::from_strings(&labels);
        printer.set_hexpand(true);
        printer.set_tooltip_text(Some("The printer the model is checked for"));
        let r = request.clone();
        printer.connect_selected_notify(move |d| {
            let id = match d.selected() {
                0 => client::CUSTOM_PRINTER.to_string(),
                i => presets
                    .get(i as usize - 1)
                    .map_or_else(|| client::CUSTOM_PRINTER.to_string(), |p| p.id.clone()),
            };
            r(Request::Printer(id));
        });
        let run = gtk::Button::builder()
            .label("Check")
            .tooltip_text("Check the model for 3D printing")
            .build();
        run.add_css_class("suggested-action");
        let r = request.clone();
        run.connect_clicked(move |_| r(Request::Check));
        let spinner = gtk::Spinner::builder().visible(false).build();
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.append(&printer);
        bar.append(&spinner);
        bar.append(&run);

        let summary = note(
            "Thin walls, overhangs, floating pieces, bed fit and parts that \
             intersect. A check renders the model in full.",
        );
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::Single)
            .valign(gtk::Align::Start)
            .build();
        list.add_css_class("boxed-list");
        list.set_visible(false);
        let scroll = gtk::ScrolledWindow::builder()
            .child(&list)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();

        let root = page_box();
        root.append(&bar);
        root.append(&summary);
        root.append(&scroll);
        let panel = Rc::new(CheckPanel {
            root,
            run,
            spinner,
            summary,
            list,
            ids: RefCell::default(),
            had_focus: Cell::new(false),
        });
        let me = Rc::downgrade(&panel);
        panel.list.connect_row_activated(move |_, row| {
            let Some(p) = me.upgrade() else { return };
            let id = p.ids.borrow().get(row.index() as usize).copied();
            if let Some(id) = id {
                request(Request::SelectFinding(id));
            }
        });
        panel
    }

    pub fn focus_run(&self) -> bool {
        self.run.grab_focus()
    }

    pub fn set_running(&self, running: bool) {
        if running {
            self.had_focus.set(self.run.has_focus());
        }
        self.run.set_sensitive(!running);
        if running {
            self.spinner.start();
            self.spinner.set_visible(true);
            self.summary.set_label("Checking…");
        } else {
            self.spinner.stop();
            self.spinner.set_visible(false);
        }
    }

    pub fn show_error(&self, message: &str) {
        self.had_focus.set(false);
        self.set_running(false);
        self.summary.set_label(message);
    }

    /// Show a report, `selected` marked.
    pub fn show_report(&self, r: &CheckReport, selected: Option<u32>) {
        let focus = self.had_focus.replace(false);
        self.set_running(false);
        self.summary.set_label(&client::check_summary(r));
        self.list.remove_all();
        let mut ids = Vec::new();
        for f in &r.findings {
            let (title, subtitle) = logic::finding_text(f);
            let row = adw::ActionRow::builder()
                .title(&title)
                .subtitle(&subtitle)
                .use_markup(false)
                .activatable(true)
                .subtitle_lines(4)
                .build();
            let (icon, class) = logic::severity_style(f.severity);
            let image = gtk::Image::from_icon_name(icon);
            image.add_css_class(class);
            image.set_tooltip_text(Some(logic::severity_name(f.severity)));
            row.add_prefix(&image);
            if f.point.len() != 3 || f.severity == FindingSeverity::Info {
                // Nothing in the view to show for it.
                row.set_tooltip_text(Some("Not marked in the view"));
            }
            self.list.append(&row);
            ids.push(f.id);
        }
        self.list.set_visible(!ids.is_empty());
        *self.ids.borrow_mut() = ids;
        self.select(selected);
        if focus && let Some(row) = self.list.row_at_index(0) {
            row.grab_focus();
        }
    }

    /// Show `id` selected in the list.
    pub fn select(&self, id: Option<u32>) {
        let index = id.and_then(|id| self.ids.borrow().iter().position(|i| *i == id));
        match index.and_then(|i| self.list.row_at_index(i as i32)) {
            Some(row) => self.list.select_row(Some(&row)),
            None => self.list.unselect_all(),
        }
    }
}

pub struct MeasurePanel {
    pub root: gtk::Box,
    run: gtk::Button,
    spinner: gtk::Spinner,
    summary: gtk::Label,
    picking: gtk::ToggleButton,
    clear: gtk::Button,
    picks: gtk::Label,
}

impl std::fmt::Debug for MeasurePanel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeasurePanel").finish_non_exhaustive()
    }
}

impl MeasurePanel {
    pub fn new(request: impl Fn(Request) + 'static) -> Rc<MeasurePanel> {
        let request: Rc<dyn Fn(Request)> = Rc::new(request);
        let run = gtk::Button::builder()
            .label("Measure")
            .tooltip_text("Measure the model as the text is now")
            .hexpand(true)
            .build();
        run.add_css_class("suggested-action");
        let r = request.clone();
        run.connect_clicked(move |_| r(Request::Measure));
        let spinner = gtk::Spinner::builder().visible(false).build();
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.append(&run);
        bar.append(&spinner);
        let summary = note(
            "Volume, surface area and size, and the distance between two \
             points picked on the model. After an edit, measure again.",
        );

        let picking = gtk::ToggleButton::builder()
            .label("Pick Points")
            .tooltip_text("Click the model in the view to pick a point")
            .sensitive(false)
            .hexpand(true)
            .build();
        let r = request.clone();
        picking.connect_toggled(move |b| r(Request::Picking(b.is_active())));
        let clear = gtk::Button::builder()
            .icon_name("edit-clear-symbolic")
            .tooltip_text("Forget the picked points")
            .sensitive(false)
            .build();
        let r = request.clone();
        clear.connect_clicked(move |_| r(Request::ClearPicks));
        let pick_bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        pick_bar.add_css_class("linked");
        pick_bar.append(&picking);
        pick_bar.append(&clear);
        let picks = note("");
        picks.remove_css_class("dim-label");
        picks.add_css_class("numeric");

        let root = page_box();
        root.append(&bar);
        root.append(&summary);
        root.append(&pick_bar);
        root.append(&picks);
        Rc::new(MeasurePanel {
            root,
            run,
            spinner,
            summary,
            picking,
            clear,
            picks,
        })
    }

    pub fn focus_run(&self) -> bool {
        self.run.grab_focus()
    }

    pub fn set_running(&self, running: bool) {
        self.run.set_sensitive(!running);
        if running {
            self.spinner.start();
            self.spinner.set_visible(true);
            self.summary.set_label("Measuring…");
        } else {
            self.spinner.stop();
            self.spinner.set_visible(false);
        }
    }

    pub fn show_error(&self, message: &str) {
        self.set_running(false);
        self.summary.set_label(message);
    }

    /// A measurement: its summary, and picking on only with a solid.
    pub fn show_report(&self, r: &MeasureReport, solid: bool) {
        self.set_running(false);
        self.summary.set_label(&logic::measure_text(r));
        self.picking.set_sensitive(solid);
        if !solid {
            self.picking.set_active(false);
        }
    }

    pub fn show_picks(&self, picks: &[Vec<f64>], picking: bool) {
        self.clear.set_sensitive(!picks.is_empty());
        self.picks.set_label(&if picking || !picks.is_empty() {
            logic::picks_text(picks)
        } else {
            String::new()
        });
    }
}
