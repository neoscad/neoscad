//! The console pane: the run's summary line (`client::describe_render`)
//! over its lines, each coloured by its filter group
//! (`client::console_group`, Adwaita's `error`/`warning` styles), and a
//! line that points into the document selects that span in the editor
//! when activated.

use std::cell::RefCell;
use std::rc::Rc;

use gtk::prelude::*;

use client::{ConsoleGroup, ConsoleLine, SourceRange};

pub struct Console {
    pub root: gtk::Box,
    summary: gtk::Label,
    list: gtk::ListBox,
    /// Where each row points, by row index (`None`: not in the document).
    ranges: Rc<RefCell<Vec<Option<SourceRange>>>>,
}

impl std::fmt::Debug for Console {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Console").finish_non_exhaustive()
    }
}

impl Console {
    /// A console calling `reveal` with the span of an activated line.
    pub fn new(reveal: impl Fn(&SourceRange) + 'static) -> Console {
        let summary = gtk::Label::builder()
            .xalign(0.0)
            .wrap(true)
            .selectable(true)
            .margin_start(12)
            .margin_end(12)
            .margin_top(6)
            .margin_bottom(6)
            .build();
        summary.add_css_class("heading");
        let list = gtk::ListBox::builder()
            .selection_mode(gtk::SelectionMode::None)
            .activate_on_single_click(true)
            .build();
        list.add_css_class("console");
        let scroll = gtk::ScrolledWindow::builder()
            .child(&list)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Automatic)
            .build();
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&summary);
        root.append(&gtk::Separator::new(gtk::Orientation::Horizontal));
        root.append(&scroll);
        let ranges: Rc<RefCell<Vec<Option<SourceRange>>>> = Rc::default();
        let r = ranges.clone();
        list.connect_row_activated(move |_, row| {
            let at = usize::try_from(row.index()).ok();
            let range = at.and_then(|i| r.borrow().get(i).cloned().flatten());
            if let Some(range) = range {
                reveal(&range);
            }
        });
        Console {
            root,
            summary,
            list,
            ranges,
        }
    }

    pub fn set_summary(&self, text: &str) {
        self.summary.set_text(text);
    }

    /// Show `lines`, those in `doc` (the document's core path) activatable.
    pub fn set_lines(&self, lines: &[ConsoleLine], doc: &str) {
        self.list.remove_all();
        let mut ranges = self.ranges.borrow_mut();
        ranges.clear();
        for l in lines {
            let label = gtk::Label::builder()
                .label(&l.text)
                .xalign(0.0)
                .wrap(true)
                .wrap_mode(gtk::pango::WrapMode::WordChar)
                .selectable(false)
                .margin_start(12)
                .margin_end(12)
                .margin_top(2)
                .margin_bottom(2)
                .build();
            label.add_css_class("monospace");
            match client::console_group(l.kind) {
                ConsoleGroup::Errors => label.add_css_class("error"),
                ConsoleGroup::Warnings => label.add_css_class("warning"),
                ConsoleGroup::Echo => {}
                ConsoleGroup::Other => label.add_css_class("dim-label"),
            }
            let row = gtk::ListBoxRow::builder().child(&label).build();
            let range = l.location.clone().filter(|r| r.path == doc);
            row.set_activatable(range.is_some());
            if range.is_some() {
                row.set_tooltip_text(Some("Show in the editor"));
            }
            ranges.push(range);
            self.list.append(&row);
        }
    }
}
