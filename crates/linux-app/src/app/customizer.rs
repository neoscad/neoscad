//! The customizer panel: the document's annotated parameters, one
//! `AdwPreferencesGroup` per customizer group (`/* [Name] */`), each
//! parameter a row with the control OpenSCAD's customizer gives it: a
//! switch for a boolean, a slider with a field for a number with a range,
//! a spin row for any other number, an entry row for text, a field per
//! element for a vector and a combo row for a dropdown.
//!
//! The panel holds no values of its own. A control's edit goes to the
//! window as a [`Request`]; the window turns it into an edited value
//! (`linux_app::customizer::apply_edit`, `client::edit_parameter`), the
//! loop runs the document again with it, and the panel is shown the
//! values back (`Customizer::show`), so a slider lands on its step and a
//! field on its clamped value. The text is never changed, as in the
//! macOS app's customizer.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use client::{
    Parameter, ParameterControl, ParameterEdit, ParameterGroup, ParameterOverride, ParameterValue,
};
use linux_app::customizer::{self as logic, Range};

/// What a control asked for.
#[derive(Debug, Clone)]
pub enum Request {
    Edit(String, ParameterEdit),
    /// One parameter back to the text's value.
    Revert(String),
    /// Every parameter back to the text's value.
    Reset,
    ApplySet(String),
    SaveSet,
}

/// Puts one row's widgets in step with a value (and whether it is edited).
type Sync = Box<dyn Fn(&ParameterValue, bool)>;

pub struct Customizer {
    pub root: gtk::Box,
    sets: gtk::DropDown,
    set_names: gtk::StringList,
    save: gtk::Button,
    reset: gtk::Button,
    stack: gtk::Stack,
    page: adw::PreferencesPage,
    shown: RefCell<Vec<ParameterGroup>>,
    groups: RefCell<Vec<adw::PreferencesGroup>>,
    rows: RefCell<Vec<(String, Sync)>>,
    first: RefCell<Option<gtk::Widget>>,
    /// Set while the panel sets its own widgets: their signals are not
    /// edits (without it, showing a value would edit it again).
    updating: Rc<Cell<bool>>,
    request: Rc<dyn Fn(Request)>,
}

impl std::fmt::Debug for Customizer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Customizer").finish_non_exhaustive()
    }
}

/// "Design default values", the parameter sets dropdown's first entry, as
/// OpenSCAD names the text's own values.
const DEFAULTS: &str = "Design default values";

impl Customizer {
    pub fn new(request: impl Fn(Request) + 'static) -> Rc<Customizer> {
        let request: Rc<dyn Fn(Request)> = Rc::new(request);
        let updating = Rc::new(Cell::new(false));
        let set_names = gtk::StringList::new(&[DEFAULTS]);
        let sets = gtk::DropDown::builder()
            .model(&set_names)
            .hexpand(true)
            .tooltip_text("Parameter sets from the JSON file beside the model")
            .build();
        let save = gtk::Button::builder()
            .icon_name("document-save-symbolic")
            .tooltip_text("Save the current values as a parameter set")
            .build();
        let reset = gtk::Button::builder()
            .icon_name("edit-undo-symbolic")
            .tooltip_text("Return every value to the one in the text")
            .build();
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        bar.set_margin_top(6);
        bar.set_margin_bottom(6);
        bar.set_margin_start(12);
        bar.set_margin_end(12);
        bar.append(&sets);
        bar.append(&save);
        bar.append(&reset);

        let page = adw::PreferencesPage::new();
        let empty = adw::StatusPage::builder()
            .icon_name("preferences-system-symbolic")
            .title("No Parameters")
            .description(
                "Top-level assignments before the first module or function, \
                 with customizer comments, appear here.",
            )
            .build();
        empty.add_css_class("compact");
        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.add_named(&empty, Some("empty"));
        stack.add_named(&page, Some("parameters"));
        stack.set_visible_child_name("empty");

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&bar);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&stack);

        let c = Rc::new(Customizer {
            root,
            sets,
            set_names,
            save,
            reset,
            stack,
            page,
            shown: RefCell::default(),
            groups: RefCell::default(),
            rows: RefCell::default(),
            first: RefCell::default(),
            updating,
            request,
        });
        let (r, u, names) = (c.request.clone(), c.updating.clone(), c.set_names.clone());
        c.sets.connect_selected_notify(move |d| {
            if u.get() {
                return;
            }
            match d.selected() {
                0 => r(Request::Reset),
                i => {
                    if let Some(name) = names.string(i) {
                        r(Request::ApplySet(name.to_string()));
                    }
                }
            }
        });
        let r = c.request.clone();
        c.save.connect_clicked(move |_| r(Request::SaveSet));
        let r = c.request.clone();
        c.reset.connect_clicked(move |_| r(Request::Reset));
        c.show_sets(false, &[], None, false);
        c
    }

    /// Show `groups` with `overrides` over the text's values: the rows are
    /// built again only when the parameters themselves changed (a typed
    /// edit to the text that changes a value or a range), else the values
    /// are put in the rows that are there, which keeps a field being typed
    /// in and the scroll position.
    pub fn show(&self, groups: &[ParameterGroup], overrides: &[ParameterOverride]) {
        if *self.shown.borrow() != groups {
            self.build(groups, overrides);
        }
        self.updating.set(true);
        for g in groups {
            for p in &g.parameters {
                let v = logic::value_of(p, overrides);
                let edited = logic::is_edited(&p.name, overrides);
                for (_, sync) in self.rows.borrow().iter().filter(|(n, _)| *n == p.name) {
                    sync(&v, edited);
                }
            }
        }
        self.updating.set(false);
        self.reset.set_sensitive(!overrides.is_empty());
    }

    /// The parameter sets dropdown: `available` only for a saved document
    /// (its sets live beside it), with `selected` chosen.
    pub fn show_sets(&self, available: bool, names: &[String], selected: Option<&str>, any: bool) {
        self.updating.set(true);
        let mut all = vec![DEFAULTS];
        all.extend(names.iter().map(String::as_str));
        let old: Vec<String> = (0..self.set_names.n_items())
            .filter_map(|i| self.set_names.string(i).map(|s| s.to_string()))
            .collect();
        if old != all {
            self.set_names.splice(0, self.set_names.n_items(), &all);
        }
        let index = selected
            .and_then(|s| names.iter().position(|n| n == s))
            .map_or(0, |i| i as u32 + 1);
        self.sets.set_selected(index);
        self.updating.set(false);
        self.sets.set_sensitive(available);
        self.save.set_sensitive(available && any);
        self.sets.set_tooltip_text(Some(if available {
            "Parameter sets from the JSON file beside the model"
        } else {
            "Save the document to keep parameter sets beside it"
        }));
    }

    /// Focus the first parameter's control (Alt+1 shows the panel this
    /// way, so the keyboard can reach it from the editor). Whether there
    /// was one.
    pub fn focus_first(&self) -> bool {
        self.first.borrow().as_ref().is_some_and(|w| w.grab_focus())
    }

    fn build(&self, groups: &[ParameterGroup], overrides: &[ParameterOverride]) {
        for g in self.groups.borrow_mut().drain(..) {
            self.page.remove(&g);
        }
        self.rows.borrow_mut().clear();
        *self.first.borrow_mut() = None;
        for g in groups {
            let group = adw::PreferencesGroup::builder().title(&g.name).build();
            for p in &g.parameters {
                let v = logic::value_of(p, overrides);
                let (row, sync) = self.row(p, &v);
                if self.first.borrow().is_none() {
                    *self.first.borrow_mut() = Some(row.clone());
                }
                group.add(&row);
                self.rows.borrow_mut().push((p.name.clone(), sync));
            }
            self.page.add(&group);
            self.groups.borrow_mut().push(group);
        }
        *self.shown.borrow_mut() = groups.to_vec();
        self.stack.set_visible_child_name(if groups.is_empty() {
            "empty"
        } else {
            "parameters"
        });
    }

    /// One parameter's row and how to show a value in it.
    fn row(&self, p: &Parameter, value: &ParameterValue) -> (gtk::Widget, Sync) {
        let name = p.name.clone();
        let edit = {
            let (r, u, name) = (self.request.clone(), self.updating.clone(), name.clone());
            Rc::new(move |e: ParameterEdit| {
                if !u.get() {
                    r(Request::Edit(name.clone(), e));
                }
            })
        };
        // Back to the text's value: shown only while the value is edited.
        let revert = gtk::Button::builder()
            .icon_name("edit-undo-symbolic")
            .tooltip_text("Back to the text's value")
            .valign(gtk::Align::Center)
            .visible(false)
            .build();
        revert.add_css_class("flat");
        let r = self.request.clone();
        revert.connect_clicked(move |_| r(Request::Revert(name.clone())));
        let number = |v: &ParameterValue| match v {
            ParameterValue::Number { value } => *value,
            _ => 0.0,
        };

        let (row, sync): (adw::PreferencesRow, Sync) = match &p.control {
            ParameterControl::Checkbox => {
                let row = adw::SwitchRow::new();
                let e = edit.clone();
                row.connect_active_notify(move |r| {
                    e(ParameterEdit::Set {
                        value: ParameterValue::Bool {
                            value: r.is_active(),
                        },
                    });
                });
                let (w, rv) = (row.clone(), revert.clone());
                let sync: Sync = Box::new(move |v, edited| {
                    if let ParameterValue::Bool { value } = v {
                        w.set_active(*value);
                    }
                    rv.set_visible(edited);
                });
                described(row.upcast_ref(), p);
                row.add_suffix(&revert);
                (row.upcast(), sync)
            }
            ParameterControl::Slider { .. } => {
                let row = adw::ActionRow::new();
                described(row.upcast_ref(), p);
                let range = logic::range(&p.control, number(value)).unwrap_or(Range {
                    lower: 0.0,
                    upper: 1.0,
                    step: 0.1,
                    digits: 1,
                });
                let scale = gtk::Scale::with_range(
                    gtk::Orientation::Horizontal,
                    range.lower,
                    range.upper,
                    range.step,
                );
                scale.set_digits(range.digits as i32);
                scale.set_draw_value(false);
                scale.set_width_request(140);
                scale.set_valign(gtk::Align::Center);
                let e = edit.clone();
                scale.connect_value_changed(move |s| e(ParameterEdit::Slide { value: s.value() }));
                let e = edit.clone();
                let field = NumberField::new(move |x| e(ParameterEdit::Type { value: x }));
                row.add_suffix(&scale);
                row.add_suffix(&field.entry);
                row.add_suffix(&revert);
                let rv = revert.clone();
                let sync: Sync = Box::new(move |v, edited| {
                    let x = number(v);
                    scale.set_value(x);
                    field.show(x);
                    rv.set_visible(edited);
                });
                (row.upcast(), sync)
            }
            ParameterControl::SpinBox { .. } => {
                let range = logic::range(&p.control, number(value)).unwrap_or(Range {
                    lower: -1e9,
                    upper: 1e9,
                    step: 1.0,
                    digits: 0,
                });
                let adj = gtk::Adjustment::new(
                    number(value),
                    range.lower,
                    range.upper,
                    range.step,
                    range.step * 10.0,
                    0.0,
                );
                let row = adw::SpinRow::new(Some(&adj), range.step, range.digits);
                described(row.upcast_ref(), p);
                let e = edit.clone();
                row.connect_value_notify(move |r| e(ParameterEdit::Type { value: r.value() }));
                row.add_suffix(&revert);
                let (w, rv) = (row.clone(), revert.clone());
                let sync: Sync = Box::new(move |v, edited| {
                    let x = number(v);
                    let a = w.adjustment();
                    // A value beyond the bounds (typed into the text)
                    // widens them rather than being clamped by the widget.
                    a.set_lower(a.lower().min(x));
                    a.set_upper(a.upper().max(x));
                    w.set_value(x);
                    rv.set_visible(edited);
                });
                (row.upcast(), sync)
            }
            ParameterControl::Text { .. } => {
                let row = adw::EntryRow::new();
                row.set_title(&p.name);
                row.set_use_markup(false);
                if !p.description.is_empty() {
                    row.set_tooltip_text(Some(&p.description));
                }
                // A maximum length is the core's to apply (it cuts to
                // bytes, `client::edit_parameter`); the row shows the
                // value it ran with.
                let e = edit.clone();
                row.connect_changed(move |r| {
                    e(ParameterEdit::Set {
                        value: ParameterValue::Text {
                            value: r.text().to_string(),
                        },
                    });
                });
                row.add_suffix(&revert);
                let (w, rv) = (row.clone(), revert.clone());
                let sync: Sync = Box::new(move |v, edited| {
                    if let ParameterValue::Text { value } = v
                        && w.text() != value.as_str()
                    {
                        w.set_text(value);
                    }
                    rv.set_visible(edited);
                });
                (row.upcast(), sync)
            }
            ParameterControl::Vector { .. } => {
                let row = adw::ActionRow::new();
                described(row.upcast_ref(), p);
                let n = match value {
                    ParameterValue::Vector { value } => value.len(),
                    _ => 0,
                };
                let fields: Vec<NumberField> = (0..n)
                    .map(|i| {
                        let e = edit.clone();
                        let f = NumberField::new(move |x| {
                            e(ParameterEdit::Item {
                                index: i as u32,
                                value: x,
                            })
                        });
                        row.add_suffix(&f.entry);
                        f
                    })
                    .collect();
                row.add_suffix(&revert);
                let rv = revert.clone();
                let sync: Sync = Box::new(move |v, edited| {
                    if let ParameterValue::Vector { value } = v {
                        for (f, x) in fields.iter().zip(value) {
                            f.show(*x);
                        }
                    }
                    rv.set_visible(edited);
                });
                (row.upcast(), sync)
            }
            ParameterControl::Dropdown { options } => {
                let labels: Vec<&str> = options.iter().map(|o| o.label.as_str()).collect();
                let row = adw::ComboRow::builder()
                    .model(&gtk::StringList::new(&labels))
                    .build();
                described(row.upcast_ref(), p);
                let (e, opts) = (edit.clone(), options.clone());
                row.connect_selected_notify(move |r| {
                    if let Some(o) = opts.get(r.selected() as usize) {
                        e(ParameterEdit::Set {
                            value: o.value.clone(),
                        });
                    }
                });
                row.add_suffix(&revert);
                let (w, rv, opts) = (row.clone(), revert.clone(), options.clone());
                let sync: Sync = Box::new(move |v, edited| {
                    w.set_selected(logic::option_index(&opts, v).unwrap_or(0));
                    rv.set_visible(edited);
                });
                (row.upcast(), sync)
            }
        };
        (row.upcast(), sync)
    }
}

/// A row titled with the parameter's name, its description below. Names
/// and descriptions are the user's text, not Pango markup: a `<` or `&`
/// in a description would otherwise blank the row.
fn described(row: &adw::ActionRow, p: &Parameter) {
    row.set_title(&p.name);
    row.set_use_markup(false);
    if !p.description.is_empty() {
        row.set_subtitle(&p.description);
    }
}

/// A number typed into a field: committed on Enter or when the field
/// loses focus, so a half-typed "1." does not run the model; a field
/// being typed in is not overwritten by the value shown back.
struct NumberField {
    entry: gtk::Entry,
    focused: Rc<Cell<bool>>,
    shown: Rc<Cell<f64>>,
}

impl NumberField {
    fn new(commit: impl Fn(f64) + 'static) -> NumberField {
        let entry = gtk::Entry::builder()
            .width_chars(6)
            .max_width_chars(8)
            .xalign(1.0)
            .input_purpose(gtk::InputPurpose::Number)
            .valign(gtk::Align::Center)
            .build();
        let focused = Rc::new(Cell::new(false));
        let shown = Rc::new(Cell::new(0.0));
        let commit = Rc::new(commit);
        let submit = {
            let (shown, commit) = (shown.clone(), commit.clone());
            move |e: &gtk::Entry| match logic::parse_number(&e.text()) {
                Some(x) => commit(x),
                None => e.set_text(&logic::number_text(shown.get())),
            }
        };
        let s = submit.clone();
        entry.connect_activate(move |e| s(e));
        let focus = gtk::EventControllerFocus::new();
        let f = focused.clone();
        focus.connect_enter(move |_| f.set(true));
        let (f, e) = (focused.clone(), entry.downgrade());
        focus.connect_leave(move |_| {
            f.set(false);
            if let Some(e) = e.upgrade() {
                submit(&e);
            }
        });
        entry.add_controller(focus);
        NumberField {
            entry,
            focused,
            shown,
        }
    }

    fn show(&self, x: f64) {
        self.shown.set(x);
        if !self.focused.get() {
            let t = logic::number_text(x);
            if self.entry.text() != t.as_str() {
                self.entry.set_text(&t);
            }
        }
    }
}

/// Ask for a set's name (GNOME's alert dialog with an entry) and hand it
/// to `save`.
pub fn ask_set_name(
    parent: &impl IsA<gtk::Widget>,
    file: &str,
    suggested: &str,
    save: impl Fn(String) + 'static,
) {
    let dialog = adw::AlertDialog::new(
        Some("Save Parameter Set"),
        Some(&format!("The set is saved in {file}, next to the model.")),
    );
    let entry = gtk::Entry::builder()
        .text(suggested)
        .activates_default(true)
        .build();
    dialog.set_extra_child(Some(&entry));
    dialog.add_responses(&[("cancel", "_Cancel"), ("save", "_Save")]);
    dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("save"));
    dialog.set_close_response("cancel");
    let e = entry.clone();
    dialog.choose(Some(parent), gtk::gio::Cancellable::NONE, move |response| {
        let name = e.text().trim().to_string();
        if response == "save" && !name.is_empty() {
            save(name);
        }
    });
    glib::idle_add_local_once(move || {
        entry.grab_focus();
    });
}
