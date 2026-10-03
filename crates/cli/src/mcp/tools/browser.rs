//! The tools that act on the connected web page (`neoscad mcp --browser`;
//! docs/mcp.md, "The web page") or on a running NeoSCAD app's open
//! document (plain `neoscad mcp`; "The desktop apps"): its editor, its 3D
//! view and its console, through the browser bridge
//! ([`crate::mcp::bridge`]) or the app link ([`crate::mcp::app`]). Both
//! speak the same requests, so one [`Surface`] serves the two, and the
//! texts differ only in what they call it. The model tools use the
//! surface's text too when a call gives neither `path` nor `source`
//! ([`Tools::page_model`]).
//!
//! Positions an agent gives and reads are 1-based lines and 1-based byte
//! columns, as every diagnostic here reports them; the editor counts
//! 0-based lines and UTF-16 columns. The two meet only through
//! `lang::source` (`utf16_position`, `offset_at_utf16`), never another
//! conversion, so a line with a `°` or an emoji cannot shift an edit.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use lang::source::SourceFile;
use serde_json::{Value, json};

use super::{Label, Model, Out, Reply, Tools, bool_arg, check_text, num, scad_string, str_arg};
use crate::mcp::app::{AppDocument, Apps, NO_APP};
use crate::mcp::bridge::{Bridge, MAX_WAIT, NOT_CONNECTED, open_in_browser};

/// The browser tools, in the order `tools/list` gives them (after the
/// model tools).
pub const NAMES: &[&str] = &[
    "browser_connect",
    "editor_read",
    "editor_edit",
    "editor_reveal",
    "view_camera",
    "view_capture",
    "view_annotate",
    "console_read",
];

/// The app tools: the browser's but `browser_connect` (an app needs no
/// link), in the same order.
pub const APP_NAMES: &[&str] = &[
    "editor_read",
    "editor_edit",
    "editor_reveal",
    "view_camera",
    "view_capture",
    "view_annotate",
    "console_read",
];

/// Where the editor and view tools act: the connected web page, or one
/// document of a connected NeoSCAD app.
pub(super) enum Surface<'a> {
    Page(&'a Bridge),
    App(&'a Arc<Apps>, AppDocument),
}

impl Surface<'_> {
    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        match self {
            Surface::Page(b) => b.request(method, params, timeout),
            Surface::App(apps, doc) => apps.request(doc, method, params, timeout),
        }
    }

    /// "the page", "the document": whose text, editor and view.
    fn the(&self) -> &'static str {
        match self {
            Surface::Page(_) => "the page",
            Surface::App(..) => "the document",
        }
    }

    /// "the web page", "the NeoSCAD app": who answers.
    fn who(&self) -> &'static str {
        match self {
            Surface::Page(_) => "the web page",
            Surface::App(..) => "the NeoSCAD app",
        }
    }

    /// A document by name: "the web page's gears.scad", "gears.scad in
    /// NeoSCAD (document 2)".
    fn name(&self, file: &str) -> String {
        match self {
            Surface::Page(_) => format!("the web page's {file}"),
            Surface::App(_, d) => format!("{file} in NeoSCAD (document {})", d.number),
        }
    }
}

/// How long the page gets for a request it answers at once.
const QUICK: Duration = Duration::from_secs(15);
/// An edit may wait for the user to approve it (the page's "ask before
/// applying" switch).
const EDIT: Duration = Duration::from_secs(150);
/// A capture waits for the page's pending preview first.
const CAPTURE: Duration = Duration::from_secs(90);
/// The capture's longest side unless `size` says otherwise: as the
/// snapshot's, since an image's cost in context grows with its pixels.
const CAPTURE_SIZE: u64 = 768;
/// Marks a call may place, so a runaway loop in an agent's arguments
/// cannot stall the page's viewer.
const MAX_MARKS: usize = 500;
const MAX_POINTS: usize = 20_000;
/// The colour of an agent's marks unless it names one: the icon's pink,
/// apart from the magenta and violet the page's own measurements use.
const MARK_COLOR: &str = "#ff5a8a";

pub fn list() -> Vec<Value> {
    let num3 = json!({"type": "array", "items": {"type": "number"}});
    let tool = |name: &str, description: &str, props: Value, read_only: bool| {
        json!({
            "name": name,
            "description": description,
            "inputSchema": {"type": "object", "properties": props},
            "annotations": {"readOnlyHint": read_only},
        })
    };
    let mut edit = tool(
        "editor_edit",
        "Edit the page's text as one undoable step the user sees highlighted. `version` from editor_read (a stale one is refused). `edits`: [{old, new}] (old must occur once) or [{at: [line, col, end_line, end_col], new}] (1-based, byte columns as diagnostics give); or `text` replaces all.",
        json!({
            "version": {"type": "integer"},
            "edits": {"type": "array", "items": {"type": "object"}},
            "text": {"type": "string"},
        }),
        false,
    );
    edit["inputSchema"]["required"] = json!(["version"]);
    vec![
        tool(
            "browser_connect",
            "A link that connects the user's NeoSCAD web page (neoscad.org/try); give it to them, then call again with wait_seconds to wait for the page rather than polling. Once connected, the editor, view and console tools act on the page.",
            json!({"open": {"type": "boolean", "description": "Also open it in their browser"},
                   "wait_seconds": {"type": "number", "description": "Wait up to this long (max 120) for the page"}}),
            true,
        ),
        tool(
            "editor_read",
            "The page's editor text (numbered lines), its version for editor_edit, the selection, and the last run's errors and warnings.",
            json!({}),
            true,
        ),
        edit,
        tool(
            "editor_reveal",
            "Select and scroll to a place in the page's editor, to show the user: `at` [line, col?, end_line?, end_col?] (1-based) or the first match of `text`.",
            json!({"at": num3, "text": {"type": "string"}}),
            true,
        ),
        tool(
            "view_camera",
            "Get or set the page's 3D view camera ($vpt, $vpr, $vpd, $vpf). Set any of vpt, vpr, vpd, after `view` (top, bottom, left, right, front, back, diagonal) or `fit` (View All).",
            json!({"vpt": num3, "vpr": num3, "vpd": {"type": "number"},
                   "view": {"type": "string"}, "fit": {"type": "boolean"}}),
            false,
        ),
        tool(
            "view_capture",
            "A PNG of the page's 3D view as the user sees it (their camera, the grid, your marks), after any pending preview. `size`: the longest side, default 768.",
            json!({"size": {"type": "number"}}),
            true,
        ),
        tool(
            "view_annotate",
            "Mark the page's 3D view for the user: `markers` [{point: [x,y,z], label}], `lines` [{points: [[x,y,z], ...]}], each with an optional color (#rrggbb). Replaces earlier marks; neither clears them.",
            json!({"markers": {"type": "array", "items": {"type": "object"}},
                   "lines": {"type": "array", "items": {"type": "object"}}}),
            false,
        ),
        tool(
            "console_read",
            "The page's console from its last preview or render: the summary, echo() output, warnings and errors with their lines.",
            json!({}),
            true,
        ),
    ]
}

/// The app tools' list: the browser's, with `document` (which open
/// document; the most recently focused by default) and without
/// `browser_connect`, described for the app.
pub fn app_list() -> Vec<Value> {
    list()
        .into_iter()
        .filter(|t| t["name"] != "browser_connect")
        .map(|mut t| {
            let d = t["description"]
                .as_str()
                .unwrap_or("")
                .replace("The page's ", "The NeoSCAD app's ")
                .replace("the page's ", "the NeoSCAD app's ");
            t["description"] = json!(d);
            t["inputSchema"]["properties"]["document"] = json!({"type": "integer"});
            t
        })
        .collect()
}

/// The page's document, as `read` gives it.
#[derive(Debug)]
struct Page {
    file: String,
    /// The app's document: its file, when saved (the page's is never one).
    path: Option<PathBuf>,
    version: u64,
    text: String,
    defines: Vec<String>,
    parts: bool,
    raw: Value,
}

impl Page {
    fn read(s: &Surface) -> Result<Page, String> {
        let r = s.request("read", json!({}), QUICK)?;
        let broken = |what: &str| match s {
            Surface::Page(_) => {
                format!("the web page sent no {what} (reload it and connect again)")
            }
            Surface::App(..) => {
                format!("the NeoSCAD app sent no {what} (update it to match this neoscad)")
            }
        };
        let text = r["text"]
            .as_str()
            .ok_or_else(|| broken("text"))?
            .to_string();
        let version = r["version"].as_u64().ok_or_else(|| broken("version"))?;
        // An app's document runs under its own path, so its includes
        // resolve beside it; it must be absolute, as every path the app
        // sends is.
        let path = match s {
            Surface::App(..) => r["path"]
                .as_str()
                .map(PathBuf::from)
                .filter(|p| p.is_absolute()),
            Surface::Page(_) => None,
        };
        let defines = r["values"]
            .as_object()
            .into_iter()
            .flatten()
            .filter_map(|(k, v)| define(k, v))
            .collect();
        Ok(Page {
            file: match s {
                Surface::App(..) => r["file"].as_str().unwrap_or("Untitled").to_string(),
                Surface::Page(_) => safe_name(r["file"].as_str()),
            },
            path,
            version,
            text,
            defines,
            parts: r["parts"].as_bool().unwrap_or(false),
            raw: r,
        })
    }

    fn source(&self) -> SourceFile {
        SourceFile::new(PathBuf::from(&self.file), self.text.as_bytes().to_vec())
    }
}

/// The page's file name as a document name here: a plain `.scad` name, or
/// `page.scad`. It becomes a path under `base_dir`, so nothing the page
/// sends may climb out of it.
fn safe_name(f: Option<&str>) -> String {
    let name = f
        .and_then(|f| Path::new(f).file_name())
        .and_then(|n| n.to_str())
        .unwrap_or("");
    let plain = name.len() > 5
        && name.ends_with(".scad")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
        && !name.starts_with('.');
    if plain { name } else { "page.scad" }.to_string()
}

/// A customizer value as a `-D` assignment (what `neoscad serve`'s
/// `defines` take); None for a name or value that is not one.
fn define(name: &str, v: &Value) -> Option<String> {
    let mut chars = name.chars();
    let ident = chars
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !ident {
        return None;
    }
    let number = |v: &Value| v.as_f64().filter(|x| x.is_finite()).map(|x| format!("{x}"));
    let value = match v {
        Value::Bool(b) => b.to_string(),
        Value::Number(_) => number(v)?,
        Value::String(s) => format!("\"{}\"", scad_string(s)),
        Value::Array(items) => {
            let n: Option<Vec<String>> = items.iter().map(number).collect();
            format!("[{}]", n?.join(", "))
        }
        _ => return None,
    };
    Some(format!("{name}={value}"))
}

/// The byte offset of 1-based `line` and byte `col`, or why not.
fn offset_of(sf: &SourceFile, text: &str, line: u64, col: u64) -> Result<u32, String> {
    let lines = u64::from(sf.line_count());
    if line < 1 || line > lines {
        return Err(format!(
            "line {line} is not in the text (it has {lines} lines)"
        ));
    }
    let (start, end) = (
        u64::from(sf.line_start(line as u32)),
        u64::from(sf.line_end(line as u32)),
    );
    if col < 1 || start + col - 1 > end {
        return Err(format!(
            "column {col} is not on line {line}, which is {} bytes long (columns count bytes from 1, as diagnostics do)",
            end - start
        ));
    }
    let off = start + col - 1;
    if !text.is_char_boundary(off as usize) {
        return Err(format!("column {col} of line {line} is inside a character"));
    }
    Ok(off as u32)
}

/// A byte offset as the editor's position: `[0-based line, UTF-16 column]`.
fn editor_pos(sf: &SourceFile, offset: u32) -> Value {
    let (l, c) = sf.utf16_position(offset);
    json!([l, c])
}

/// The editor's `[line, column]` as the agent's `line:col` (1-based, byte
/// column).
fn agent_pos(sf: &SourceFile, p: &Value) -> Option<(u32, u32)> {
    let l = u32::try_from(p.get(0)?.as_u64()?).ok()?;
    let c = u32::try_from(p.get(1)?.as_u64()?).ok()?;
    Some(sf.line_col(sf.offset_at_utf16(l, c)))
}

/// Up to four whole numbers of `at`.
fn at_arg(args: &Value) -> Result<Option<Vec<u64>>, String> {
    let Some(at) = args.get("at").filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let n: Option<Vec<u64>> = at
        .as_array()
        .map(|a| a.iter().map(Value::as_u64).collect())
        .unwrap_or(None);
    match n {
        Some(n) if (1..=4).contains(&n.len()) => Ok(Some(n)),
        _ => {
            Err("`at` must be [line, col, end_line, end_col]: 1 to 4 whole numbers, 1-based".into())
        }
    }
}

impl Tools {
    pub(super) fn browser_tool(&self, s: &Surface, name: &str, args: &Value) -> Reply {
        match (name, s) {
            ("browser_connect", Surface::Page(b)) => connect(b, args),
            ("editor_read", _) => editor_read(s),
            ("editor_edit", _) => editor_edit(s, args),
            ("editor_reveal", _) => editor_reveal(s, args),
            ("view_camera", _) => view_camera(s, args),
            ("view_capture", _) => view_capture(s, args),
            ("view_annotate", _) => view_annotate(s, args),
            ("console_read", _) => console_read(s),
            _ => Err(format!("unknown tool '{name}'")),
        }
    }

    /// The page's text as the model of a tool call, with the page's
    /// customizer values and `part()` switch.
    pub(super) fn page_model(
        &self,
        s: &Surface,
        base: PathBuf,
        _inline: &str,
    ) -> Result<Model<'_>, String> {
        let page = Page::read(s).map_err(|e| {
            if e == NOT_CONNECTED {
                format!("give path (a .scad file) or source (OpenSCAD text); or, for the user's web page, {NOT_CONNECTED}")
            } else if e == NO_APP {
                format!("give path (a .scad file) or source (OpenSCAD text); {NO_APP}")
            } else {
                e
            }
        })?;
        let turn = self
            .inline
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // An app's saved document runs where it is, as the app runs it, so
        // its includes resolve (its directory is readable while it is
        // open: `Roots::set_document_dirs`). Anything else is a document
        // under `base_dir` with a plain name, which the page cannot choose.
        let (doc, base) = match &page.path {
            Some(p) => (
                session::normal(p),
                p.parent().map_or(base, Path::to_path_buf),
            ),
            None => (base.join(safe_name(Some(&page.file))), base),
        };
        self.local
            .session()
            .open(&doc, Some(page.text.into_bytes()));
        let (number, activity) = match s {
            Surface::App(apps, d) => (
                Some(d.number),
                Some(apps.activity(d, &super::current_tool())),
            ),
            Surface::Page(_) => (None, None),
        };
        Ok(Model {
            opened: vec![doc.clone()],
            path: doc,
            base,
            label: Label::Page {
                file: page.file,
                version: page.version,
                number,
            },
            defines: page.defines,
            parts: page.parts,
            _turn: Some(turn),
            _activity: activity,
        })
    }

    /// `format` of the page: its text formatted in place, as one undoable
    /// edit (or with `check`, what would change).
    pub(super) fn format_page(
        &self,
        s: &Surface,
        id: &Value,
        base: &Path,
        check: bool,
        diff: bool,
    ) -> Reply {
        let page = Page::read(s)?;
        let r = self.run(
            id,
            "format",
            &json!({"text": page.text, "cwd": base, "diff": true}),
        )?;
        if !r["error"].is_null() {
            return Err(format!(
                "not formatted: {}",
                r["error"]["message"].as_str().unwrap_or("")
            ));
        }
        let d = r["diff"].as_str().unwrap_or("");
        let formatted = r["text"].as_str().unwrap_or(&page.text);
        let text = if d.is_empty() || formatted == page.text {
            "already formatted".to_string()
        } else if check {
            check_text(d, diff)
        } else {
            let sf = page.source();
            let edit = json!({"from": [0, 0], "to": editor_pos(&sf, page.text.len() as u32), "insert": formatted});
            let r = s.request(
                "edit",
                json!({"version": page.version, "edits": [edit], "summary": "format"}),
                EDIT,
            )?;
            format!(
                "reformatted {} (whitespace only); version {}",
                s.name(&page.file),
                r["version"]
            )
        };
        Ok(Out {
            text: format!("{} (version {})\n{text}", s.name(&page.file), page.version),
            structured: Value::Null,
            png: None,
        })
    }
}

fn text_out(text: String) -> Reply {
    Ok(Out {
        text,
        structured: Value::Null,
        png: None,
    })
}

fn connect(b: &Bridge, args: &Value) -> Reply {
    let link = b.link();
    let opened = if bool_arg(args, "open") {
        match open_in_browser(&link) {
            Ok(()) => "\nOpened it in the default browser.".to_string(),
            Err(e) => format!("\n{e}: give the user the link instead."),
        }
    } else {
        String::new()
    };
    // Negative, NaN or missing is no wait; MAX_WAIT caps the rest, so a
    // call cannot park a thread for ever.
    let wait = args["wait_seconds"]
        .as_f64()
        .filter(|s| *s > 0.0)
        .map_or(Duration::ZERO, |s| {
            Duration::from_secs_f64(s.min(MAX_WAIT.as_secs_f64()))
        });
    let tab = if wait.is_zero() {
        b.tab()
    } else {
        b.wait_for_tab(wait)
    };
    let text = match tab {
        Some(t) => format!(
            "Connected: the web page's {} in {} ({}). To connect another tab (it replaces this one), open: {link}{opened}",
            t.hello["file"].as_str().unwrap_or("document"),
            t.hello["browser"].as_str().unwrap_or("a browser"),
            if t.via == "relay" {
                // Closing the relay window is how the user disconnects, so
                // a window closed by mistake looks like a page that went
                // away; saying so up front lets the agent warn them.
                "through its connection window: tell the user to keep that small window open, since closing it disconnects"
            } else {
                "directly"
            },
        ),
        None => format!(
            "Not connected{}. Give the user this link to open in a desktop browser (Chrome, Edge, Firefox or Safari), or to paste into the page's \"Connect your AI agent\" panel: {link}{opened}\n\
             If the page says it cannot reach neoscad directly, tell the user to click \"Open a connection window\" there. \
             Then call browser_connect with wait_seconds (up to 120) to wait for the page instead of calling it repeatedly.",
            if wait.is_zero() {
                " yet".to_string()
            } else {
                format!(" after waiting {} s", wait.as_secs())
            },
        ),
    };
    text_out(text)
}

fn editor_read(s: &Surface) -> Reply {
    let page = Page::read(s)?;
    let sf = page.source();
    let r = &page.raw;
    let lines = page.text.split('\n').count();
    let mut text = match s {
        Surface::Page(_) => format!("{}, version {}, {lines} lines", page.file, page.version),
        Surface::App(..) => format!(
            "{}, version {}, {lines} lines, {}",
            s.name(&page.file),
            page.version,
            page.path
                .as_ref()
                .map_or("not saved yet".to_string(), |p| format!(
                    "saved as {}",
                    p.display()
                ))
        ),
    };
    let sel = &r["selection"];
    if let (Some(a), Some(h)) = (agent_pos(&sf, &sel["anchor"]), agent_pos(&sf, &sel["head"])) {
        let (from, to) = (a.min(h), a.max(h));
        if from == to {
            text.push_str(&format!("; cursor at {}:{}", from.0, from.1));
        } else {
            text.push_str(&format!(
                "; selected {}:{}-{}:{}",
                from.0, from.1, to.0, to.1
            ));
        }
    }
    if !page.defines.is_empty() {
        text.push_str(&format!("; customizer: {}", page.defines.join(", ")));
    }
    if page.parts {
        text.push_str("; part() on");
    }
    if let Some(s) = r["run"]["summary"].as_str().filter(|s| !s.is_empty()) {
        text.push_str(&format!("\nlast run: {s}"));
    }
    for d in r["diagnostics"].as_array().into_iter().flatten().take(20) {
        text.push_str(&format!("\n{}", console_line(d)));
    }
    if let Surface::App(apps, d) = s {
        let others: Vec<String> = apps
            .documents()
            .iter()
            .filter(|o| o.number != d.number)
            .map(|o| format!("{} {}", o.number, o.file))
            .collect();
        if !others.is_empty() {
            text.push_str(&format!(
                "\nalso open (pass document): {}",
                others.join(", ")
            ));
        }
    }
    text.push_str("\n---\n");
    for (i, l) in page.text.split('\n').enumerate() {
        text.push_str(&format!("{:>6}\t{l}\n", i + 1));
    }
    text_out(text)
}

fn console_line(d: &Value) -> String {
    let kind = d["kind"].as_str().unwrap_or("info");
    let body = d["text"].as_str().unwrap_or("");
    match (d["line"].as_u64(), d["file"].as_str()) {
        (Some(l), Some(f)) => format!("{kind} {f}:{l}: {body}"),
        (Some(l), None) => format!("{kind} line {l}: {body}"),
        _ => format!("{kind}: {body}"),
    }
}

fn editor_edit(s: &Surface, args: &Value) -> Reply {
    let version = args["version"].as_u64().ok_or(format!(
        "editor_edit needs `version`, the number editor_read gives (read {}'s text first)",
        s.the()
    ))?;
    let page = Page::read(s)?;
    if page.version != version {
        return Err(format!(
            "{}'s text changed since version {version} (the user may be typing; it is at version {} now): editor_read again and make the edit on the new text",
            s.the(),
            page.version
        ));
    }
    let sf = page.source();
    let mut ranges: Vec<(u32, u32, String, usize)> = Vec::new();
    match (
        args.get("edits").filter(|v| !v.is_null()),
        str_arg(args, "text"),
    ) {
        (Some(_), Some(_)) => return Err("give edits or text, not both".into()),
        (None, None) => {
            return Err(
                "give edits ([{old, new}] or [{at, new}]) or text (the whole new text)".into(),
            );
        }
        (None, Some(t)) => ranges.push((0, page.text.len() as u32, t.to_string(), 0)),
        (Some(edits), None) => {
            let edits = edits.as_array().ok_or("`edits` must be an array")?;
            if edits.is_empty() {
                return Err("`edits` is empty: nothing to change".into());
            }
            for (i, e) in edits.iter().enumerate() {
                let i = i + 1;
                let new = e["new"].as_str().ok_or(format!(
                    "edit {i} needs `new`, the replacement text (\"\" deletes)"
                ))?;
                let (from, to) = match (e["old"].as_str(), e.get("at").filter(|v| !v.is_null())) {
                    (Some(""), None) => return Err(format!("edit {i}: `old` is empty")),
                    (Some(old), None) => {
                        let hits: Vec<usize> =
                            page.text.match_indices(old).map(|(k, _)| k).collect();
                        match hits[..] {
                            [k] => (k as u32, (k + old.len()) as u32),
                            [] => {
                                return Err(format!(
                                    "edit {i}: `old` does not occur in {}'s text (its spaces and line breaks must match exactly; editor_read shows the text)",
                                    s.the()
                                ));
                            }
                            _ => {
                                return Err(format!(
                                    "edit {i}: `old` occurs {} times; include more of the text around it so it occurs once",
                                    hits.len()
                                ));
                            }
                        }
                    }
                    (None, Some(_)) => match at_arg(e)?.as_deref() {
                        Some(&[l, c, el, ec]) => {
                            let from = offset_of(&sf, &page.text, l, c)?;
                            let to = offset_of(&sf, &page.text, el, ec)?;
                            if to < from {
                                return Err(format!("edit {i}: `at` ends before it starts"));
                            }
                            (from, to)
                        }
                        _ => {
                            return Err(format!(
                                "edit {i}: `at` must be [line, col, end_line, end_col] (1-based; the end is exclusive)"
                            ));
                        }
                    },
                    _ => {
                        return Err(format!(
                            "edit {i}: give `old` (the text to replace) or `at` ([line, col, end_line, end_col]), not both"
                        ));
                    }
                };
                ranges.push((from, to, new.to_string(), i));
            }
        }
    }
    ranges.sort_by_key(|r| (r.0, r.1));
    if let Some(w) = ranges.windows(2).find(|w| w[0].1 > w[1].0) {
        return Err(format!("edits {} and {} overlap", w[0].3, w[1].3));
    }
    let mut touched: Vec<(u32, u32)> = ranges
        .iter()
        .map(|r| (sf.line_of(r.0), sf.line_of(r.1)))
        .collect();
    touched.dedup();
    let summary = touched
        .iter()
        .map(|&(a, z)| {
            if a == z {
                a.to_string()
            } else {
                format!("{a}-{z}")
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    let edits: Vec<Value> = ranges
        .iter()
        .map(|(a, z, t, _)| json!({"from": editor_pos(&sf, *a), "to": editor_pos(&sf, *z), "insert": t}))
        .collect();
    let n = edits.len();
    let r = s.request(
        "edit",
        json!({"version": version, "edits": edits, "summary": format!("line {summary}")}),
        EDIT,
    )?;
    let v = r["version"].as_u64().unwrap_or(0);
    let mut structured = json!({"version": v, "file": page.file, "lines": summary});
    if let Surface::App(_, d) = s {
        structured["document"] = json!(d.number);
    }
    Ok(Out {
        text: format!(
            "applied {n} edit{} to {} (line {summary}); now version {v}. {} previews it: view_capture shows the result, console_read its messages.",
            if n == 1 { "" } else { "s" },
            s.name(&page.file),
            match s {
                Surface::Page(_) => "The page",
                Surface::App(..) => "NeoSCAD",
            }
        ),
        structured,
        png: None,
    })
}

fn editor_reveal(s: &Surface, args: &Value) -> Reply {
    let page = Page::read(s)?;
    let sf = page.source();
    let t = &page.text;
    let line_len = |l: u64| u64::from(sf.line_end(l as u32) - sf.line_start(l as u32));
    let (from, to) = match (at_arg(args)?, str_arg(args, "text")) {
        (Some(_), Some(_)) => return Err("give at or text, not both".into()),
        (None, None) => return Err("give at ([line, col?, end_line?, end_col?]) or text".into()),
        (None, Some(needle)) => match t.find(needle).filter(|_| !needle.is_empty()) {
            Some(k) => (k as u32, (k + needle.len()) as u32),
            None => return Err(format!("`text` does not occur in {}'s text", s.the())),
        },
        (Some(at), None) => match at[..] {
            [l] => (
                offset_of(&sf, t, l, 1)?,
                offset_of(&sf, t, l, line_len(l) + 1)?,
            ),
            [l, c] => {
                let p = offset_of(&sf, t, l, c)?;
                (p, p)
            }
            [l, c, el] => (
                offset_of(&sf, t, l, c)?,
                offset_of(&sf, t, el, line_len(el) + 1)?,
            ),
            [l, c, el, ec] => (offset_of(&sf, t, l, c)?, offset_of(&sf, t, el, ec)?),
            _ => unreachable!("at_arg gives 1 to 4 numbers"),
        },
    };
    s.request(
        "reveal",
        json!({"from": editor_pos(&sf, from), "to": editor_pos(&sf, to.max(from))}),
        QUICK,
    )?;
    let (l, c) = sf.line_col(from);
    let (el, ec) = sf.line_col(to.max(from));
    text_out(format!(
        "showing {l}:{c}{} in {}'s editor",
        if (l, c) == (el, ec) {
            String::new()
        } else {
            format!("-{el}:{ec}")
        },
        s.the()
    ))
}

fn vec3(v: &Value) -> Option<Vec<f64>> {
    let a: Option<Vec<f64>> = v.as_array()?.iter().map(Value::as_f64).collect();
    a.filter(|a| a.len() == 3 && a.iter().all(|x| x.is_finite()))
}

fn camera_text(c: &Value) -> String {
    let v = |k: &str| {
        let n: Vec<String> = c[k]
            .as_array()
            .into_iter()
            .flatten()
            .map(|x| num(x.as_f64().unwrap_or(0.0)))
            .collect();
        format!("[{}]", n.join(", "))
    };
    format!(
        "$vpt = {}; $vpr = {}; $vpd = {}; $vpf = {}",
        v("vpt"),
        v("vpr"),
        num(c["vpd"].as_f64().unwrap_or(0.0)),
        num(c["vpf"].as_f64().unwrap_or(0.0))
    )
}

fn view_camera(s: &Surface, args: &Value) -> Reply {
    let mut set = json!({});
    for k in ["vpt", "vpr"] {
        if let Some(v) = args.get(k).filter(|v| !v.is_null()) {
            set[k] = json!(vec3(v).ok_or(format!("`{k}` must be [x, y, z]"))?);
        }
    }
    if let Some(d) = args.get("vpd").filter(|v| !v.is_null()) {
        match d.as_f64() {
            Some(d) if d.is_finite() && d > 0.0 => set["vpd"] = json!(d),
            _ => return Err("`vpd` must be a distance above 0".into()),
        }
    }
    if let Some(v) = str_arg(args, "view") {
        let v = v.to_ascii_lowercase();
        let v = if v == "iso" {
            "diagonal".to_string()
        } else {
            v
        };
        if ![
            "top", "bottom", "left", "right", "front", "back", "diagonal",
        ]
        .contains(&v.as_str())
        {
            return Err("`view` is one of top, bottom, left, right, front, back, diagonal".into());
        }
        set["view"] = json!(v);
    }
    if bool_arg(args, "fit") {
        set["fit"] = json!(true);
    }
    let r = s.request("camera", set, QUICK)?;
    Ok(Out {
        text: format!("{}'s camera: {}", s.the(), camera_text(&r)),
        structured: json!({"camera": r}),
        png: None,
    })
}

fn view_capture(s: &Surface, args: &Value) -> Reply {
    let size = match args.get("size").filter(|v| !v.is_null()) {
        None => CAPTURE_SIZE,
        Some(s) => match s.as_f64() {
            Some(s) if (64.0..=2048.0).contains(&s) => s as u64,
            _ => return Err("`size` is the longest side in pixels, 64 to 2048".into()),
        },
    };
    let r = s.request("capture", json!({"size": size}), CAPTURE)?;
    let png = decode_base64(r["png"].as_str().unwrap_or(""))
        .filter(|p| p.starts_with(b"\x89PNG\r\n\x1a\n"))
        .ok_or(format!("{} sent no image", s.who()))?;
    let mut text = format!(
        "{}'s 3D view, {}x{} ({}); {}",
        s.the(),
        r["width"],
        r["height"],
        r["backend"].as_str().unwrap_or("?"),
        camera_text(&r["camera"])
    );
    if let Some(s) = r["run"]["summary"].as_str().filter(|s| !s.is_empty()) {
        text.push_str(&format!("\nlast run: {s}"));
    }
    Ok(Out {
        text,
        structured: json!({"width": r["width"], "height": r["height"], "camera": r["camera"], "run": r["run"]}),
        png: Some(png),
    })
}

fn color_arg(v: &Value) -> Result<String, String> {
    match v.as_str() {
        None if v.is_null() => Ok(MARK_COLOR.into()),
        Some(c)
            if (c.len() == 7 || c.len() == 4)
                && c.starts_with('#')
                && c[1..].chars().all(|x| x.is_ascii_hexdigit()) =>
        {
            Ok(c.to_string())
        }
        _ => Err(format!("color {v} must be #rrggbb")),
    }
}

fn view_annotate(s: &Surface, args: &Value) -> Reply {
    let empty = Vec::new();
    let markers = args["markers"].as_array().unwrap_or(&empty);
    let lines = args["lines"].as_array().unwrap_or(&empty);
    if markers.len() + lines.len() > MAX_MARKS {
        return Err(format!("at most {MAX_MARKS} markers and lines at a time"));
    }
    let mut out_markers = Vec::new();
    for (i, m) in markers.iter().enumerate() {
        let point =
            vec3(&m["point"]).ok_or(format!("marker {}: `point` must be [x, y, z]", i + 1))?;
        let label: String = m["label"].as_str().unwrap_or("").chars().take(40).collect();
        out_markers.push(json!({"point": point, "label": label, "color": color_arg(&m["color"])?}));
    }
    let mut out_lines = Vec::new();
    let mut points = 0;
    for (i, l) in lines.iter().enumerate() {
        let p: Option<Vec<Vec<f64>>> = l["points"]
            .as_array()
            .map(|a| a.iter().map(vec3).collect())
            .unwrap_or(None);
        let p = p.filter(|p| p.len() >= 2).ok_or(format!(
            "line {}: `points` must be two or more [x, y, z]",
            i + 1
        ))?;
        points += p.len();
        if points > MAX_POINTS {
            return Err(format!("at most {MAX_POINTS} points in all"));
        }
        out_lines.push(json!({"points": p, "closed": l["closed"].as_bool().unwrap_or(false), "color": color_arg(&l["color"])?}));
    }
    let (nm, nl) = (out_markers.len(), out_lines.len());
    s.request(
        "annotate",
        json!({"markers": out_markers, "lines": out_lines}),
        QUICK,
    )?;
    text_out(if nm + nl == 0 {
        format!("cleared your marks from {}'s 3D view", s.the())
    } else {
        format!(
            "showing {nm} marker{} and {nl} line{} in {}'s 3D view (view_capture to see them)",
            if nm == 1 { "" } else { "s" },
            if nl == 1 { "" } else { "s" },
            s.the()
        )
    })
}

fn console_read(s: &Surface) -> Reply {
    let r = s.request("console", json!({}), QUICK)?;
    let mut text = r["summary"].as_str().unwrap_or("no run yet").to_string();
    let lines = r["lines"].as_array().map_or(&[][..], Vec::as_slice);
    for l in lines.iter().take(200) {
        text.push('\n');
        text.push_str(&console_line(l));
    }
    if lines.len() > 200 {
        text.push_str(&format!("\n(and {} more lines)", lines.len() - 200));
    }
    text_out(text)
}

/// Standard base64 (RFC 4648), padding optional; None for anything else.
fn decode_base64(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        } as u32)
    };
    let s = s.trim_end_matches('=').as_bytes();
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    for chunk in s.chunks(4) {
        let mut n = 0u32;
        for (i, &c) in chunk.iter().enumerate() {
            n |= val(c)? << (18 - 6 * i);
        }
        let bytes = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
        match chunk.len() {
            4 => out.extend_from_slice(&bytes),
            3 => out.extend_from_slice(&bytes[..2]),
            2 => out.push(bytes[0]),
            _ => return None,
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trips_with_the_encoder() {
        for data in [
            &b""[..],
            b"f",
            b"fo",
            b"foo",
            b"foob",
            b"\x89PNG\r\n\x1a\n\0\xff",
        ] {
            assert_eq!(decode_base64(&super::super::base64(data)).unwrap(), data);
        }
        assert_eq!(decode_base64("Zm9v"), Some(b"foo".to_vec()));
        assert_eq!(decode_base64("Zm9v!"), None);
        assert_eq!(decode_base64("Z"), None);
    }

    #[test]
    fn page_names_and_customizer_values_are_made_safe() {
        assert_eq!(safe_name(Some("gearbox.scad")), "gearbox.scad");
        assert_eq!(
            safe_name(Some("examples/gear-box_2.scad")),
            "gear-box_2.scad"
        );
        assert_eq!(safe_name(Some("../../etc/passwd")), "page.scad");
        assert_eq!(safe_name(Some(".scad")), "page.scad");
        assert_eq!(safe_name(Some("a b.scad")), "page.scad");
        assert_eq!(safe_name(None), "page.scad");
        assert_eq!(define("teeth", &json!(12)), Some("teeth=12".into()));
        assert_eq!(define("r", &json!(2.5)), Some("r=2.5".into()));
        assert_eq!(define("on", &json!(true)), Some("on=true".into()));
        assert_eq!(
            define("s", &json!("a\"b\\")),
            Some("s=\"a\\\"b\\\\\"".into())
        );
        assert_eq!(define("v", &json!([1, 2.5])), Some("v=[1, 2.5]".into()));
        assert_eq!(define("x;y", &json!(1)), None);
        assert_eq!(define("1x", &json!(1)), None);
        assert_eq!(define("v", &json!(["a"])), None);
        assert_eq!(define("o", &json!({})), None);
    }

    #[test]
    fn positions_convert_through_lang_source() {
        // "é" is two bytes and one UTF-16 unit; "😀" four bytes and two.
        let text = "a = 1;\n// é😀x\ncube();";
        let sf = SourceFile::new(PathBuf::from("t.scad"), text.as_bytes().to_vec());
        // Line 2, byte column 10 is the "x" after the emoji.
        let off = offset_of(&sf, text, 2, 10).unwrap();
        assert_eq!(&text[off as usize..off as usize + 1], "x");
        assert_eq!(editor_pos(&sf, off), json!([1, 6]));
        assert_eq!(agent_pos(&sf, &json!([1, 6])), Some((2, 10)));
        // The end of a line is a column; past it, or inside the emoji, not.
        assert!(offset_of(&sf, text, 1, 7).is_ok());
        assert!(offset_of(&sf, text, 1, 8).unwrap_err().contains("6 bytes"));
        assert!(offset_of(&sf, text, 2, 7).unwrap_err().contains("inside"));
        assert!(offset_of(&sf, text, 4, 1).unwrap_err().contains("3 lines"));
        assert!(offset_of(&sf, text, 0, 1).is_err());
    }

    #[test]
    fn connect_waits_when_asked_and_says_what_to_tell_the_user() {
        let b = Bridge::start(crate::mcp::bridge::DEFAULT_PAGE).unwrap();
        let text = connect(&b, &json!({})).unwrap().text;
        assert!(text.starts_with("Not connected yet."), "{text}");
        assert!(text.contains("Open a connection window"), "{text}");
        assert!(text.contains("wait_seconds"), "{text}");
        let start = std::time::Instant::now();
        let text = connect(&b, &json!({"wait_seconds": 0.2})).unwrap().text;
        assert!(start.elapsed() >= Duration::from_millis(200));
        assert!(text.starts_with("Not connected after waiting"), "{text}");
        // A negative wait is no wait.
        let start = std::time::Instant::now();
        connect(&b, &json!({"wait_seconds": -5})).unwrap();
        assert!(start.elapsed() < Duration::from_millis(150));
    }

    #[test]
    fn the_browser_tools_stay_small() {
        let tools = list();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, NAMES);
        for t in &tools {
            assert!(
                t["description"].as_str().unwrap().len() < 300,
                "{}",
                t["name"]
            );
        }
        let visible: Vec<Value> = tools
            .iter()
            .map(|t| json!([t["name"], t["description"], t["inputSchema"]]))
            .collect();
        let size = Value::Array(visible).to_string().len();
        assert!(size < 2600, "the browser tools are {size} bytes");
    }

    /// The app's tools are the browser's less `browser_connect`, plus
    /// `document`: no larger, so a session with the app open pays no more
    /// than one with the web page (docs/mcp.md, "The desktop apps").
    #[test]
    fn the_app_tools_stay_small() {
        let tools = app_list();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, APP_NAMES);
        for t in &tools {
            let d = t["description"].as_str().unwrap();
            assert!(d.len() < 300, "{}", t["name"]);
            assert!(!d.contains("page"), "{d}");
            assert!(t["inputSchema"]["properties"]["document"].is_object());
        }
        let visible: Vec<Value> = tools
            .iter()
            .map(|t| json!([t["name"], t["description"], t["inputSchema"]]))
            .collect();
        let size = Value::Array(visible).to_string().len();
        assert!(size < 2600, "the app tools are {size} bytes");
        eprintln!("the app tools: {size} bytes");
    }
}
