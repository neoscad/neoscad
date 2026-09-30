//! The file-manager preview: a model's picture with short notes on what
//! went wrong, and the page that shows it above the source
//! (`docs/audits/shared-core.md`, step 6).
//!
//! The macOS Quick Look and thumbnail extensions had this in Swift; a
//! GNOME thumbnailer or a Windows preview handler needs it verbatim. The
//! host keeps what is its own: reading the file, a watchdog that returns at
//! a deadline even if a step ignores cancellation, and handing the page to
//! its preview API.

use serde::{Deserialize, Serialize};

use crate::{Diagnostic, ResourceLimits, Severity};

/// Files larger than this are shown as text only: parsing megabytes of
/// generated code is not worth a file-manager hiccup.
pub const PREVIEW_MAX_SOURCE_BYTES: u64 = 4 << 20;

/// Source beyond this many characters is cut from the page, with a note:
/// WebKit (and WebView2) lay out every line of a `<pre>`, and a preview is
/// for a glance.
pub const PREVIEW_MAX_SOURCE_CHARS: usize = 200_000;

/// The attachment name the page's `<img>` refers to (`cid:`): the picture
/// travels beside the page, not inlined as base64.
pub const PREVIEW_IMAGE_ID: &str = "model.png";

/// What a preview shows.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreviewOutcome {
    /// The model's picture (an opaque PNG), or `None`: the model failed,
    /// timed out or was too large to try.
    pub png: Option<Vec<u8>>,
    /// The file's text, for the page to show beneath the picture.
    pub source: String,
    /// Short sentences for the user: files that could not be read, the
    /// first error, a timeout.
    pub notes: Vec<String>,
    /// Whether the host's deadline passed before the picture was ready.
    pub timed_out: bool,
    /// Files the model uses that could not be read, as the model names
    /// them (in a sandbox, usually its siblings).
    pub unreadable: Vec<String>,
}

/// Tight limits: nobody asked for this render. The rest are `base`'s (the
/// agent defaults).
pub fn preview_limits(base: ResourceLimits) -> ResourceLimits {
    ResourceLimits {
        time_seconds: Some(5.0),
        memory_bytes: Some(512 << 20),
        ..base
    }
}

/// A preview that stops at a note (the file could not be read or opened).
pub fn preview_failed(source: String, note: String) -> PreviewOutcome {
    PreviewOutcome {
        source,
        notes: vec![note],
        ..PreviewOutcome::default()
    }
}

/// A file too large to render.
pub fn preview_too_large(source: String) -> PreviewOutcome {
    preview_failed(
        source,
        "This file is too large to render in Quick Look.".into(),
    )
}

/// The host's deadline passed first.
pub fn preview_timed_out(source: String, deadline_seconds: u32) -> PreviewOutcome {
    PreviewOutcome {
        timed_out: true,
        ..preview_failed(
            source,
            format!(
                "The model took longer than Quick Look allows ({deadline_seconds} s). \
                 Open it in NeoSCAD to render it."
            ),
        )
    }
}

/// A finished picture request's outcome: the picture, the files it could
/// not read (named relative to `dir`, the document's directory, as the
/// model wrote them) and a note on the first error or an empty model.
pub fn preview_outcome(
    source: String,
    png: Option<Vec<u8>>,
    exit_code: u8,
    empty: bool,
    diagnostics: &[Diagnostic],
    console: &str,
    dir: &str,
) -> PreviewOutcome {
    let mut r = PreviewOutcome {
        png,
        source,
        ..PreviewOutcome::default()
    };
    r.unreadable = unreadable_files(diagnostics, console, dir);
    if !r.unreadable.is_empty() {
        // Missing and sandbox-blocked files fail alike, so the note names
        // both causes rather than guessing.
        r.notes.push(format!(
            "Skipped files Quick Look could not read: {}. Quick Look can open only the \
             previewed file; open it in NeoSCAD to see the whole model.",
            r.unreadable.join(", ")
        ));
    }
    if exit_code != 0 {
        let first = diagnostics.iter().find(|d| d.severity == Severity::Error);
        match first {
            Some(d) if d.code == "resource-limit" => r.notes.push(format!(
                "The model is too large for Quick Look: {}. Open it in NeoSCAD to render it.",
                d.message
            )),
            Some(d) => r.notes.push(match d.line {
                Some(line) => format!("Error on line {line}: {}", d.message),
                None => format!("Error: {}", d.message),
            }),
            None => r.notes.push("The model has errors.".into()),
        }
    } else if empty {
        r.notes.push("The model is empty.".into());
    }
    r
}

/// Files a model could not read: the loader's diagnostics (`include`,
/// `use`) and the console lines of `import()` and library files, in order
/// and without repeats. `import()` names its file by absolute path, as
/// OpenSCAD does; one inside `dir` is shown relative to it.
///
/// The names still come out of the messages' text ("Can't find include
/// file 'parts.scad'."): the diagnostics carry no structured path yet
/// (`docs/followups.md`).
pub fn unreadable_files(diagnostics: &[Diagnostic], console: &str, dir: &str) -> Vec<String> {
    let prefix = if dir.ends_with('/') {
        dir.to_string()
    } else {
        format!("{dir}/")
    };
    let mut names: Vec<String> = Vec::new();
    let mut add = |line: &str| {
        if let Some(name) = quoted(line) {
            let name = name.strip_prefix(&prefix).unwrap_or(name).to_string();
            if !names.contains(&name) {
                names.push(name);
            }
        }
    };
    for d in diagnostics {
        if d.code == "include-not-found" || d.code == "library-not-found" {
            add(&d.message);
        }
    }
    for line in console.split('\n').filter(|l| l.contains("Can't open")) {
        add(line);
    }
    names
}

/// The first `'...'` after the apostrophe of "Can't", which every one of
/// these messages starts with.
fn quoted(s: &str) -> Option<&str> {
    let from = s.find("Can't").map_or(0, |i| i + "Can't".len());
    let rest = &s[from..];
    let start = rest.find('\'')? + 1;
    let end = rest[start..].find('\'')?;
    Some(&rest[start..start + end])
}

/// `s` with `&`, `<`, `>` and `"` escaped for HTML text and attributes.
pub fn escape_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            c => out.push(c),
        }
    }
    out
}

/// The preview page: the picture (as the `cid:` attachment
/// [`PREVIEW_IMAGE_ID`]), the notes, then the source. HTML because a
/// preview pane scrolls it and selects its text, and a long file's source
/// needs both.
pub fn preview_html(r: &PreviewOutcome, title: &str) -> String {
    let mut notes = r.notes.clone();
    let mut source = r.source.as_str();
    if let Some((cut, _)) = source.char_indices().nth(PREVIEW_MAX_SOURCE_CHARS) {
        source = &source[..cut];
        notes.push("The source is cut short here; open the file to see all of it.".into());
    }
    let title = escape_html(title);
    let image = if r.png.is_some() {
        format!("<div class=\"model\"><img src=\"cid:{PREVIEW_IMAGE_ID}\" alt=\"{title}\"></div>")
    } else {
        String::new()
    };
    let note_list = if notes.is_empty() {
        String::new()
    } else {
        let items: String = notes
            .iter()
            .map(|n| format!("<li>{}</li>", escape_html(n)))
            .collect();
        format!("<ul class=\"notes\">{items}</ul>")
    };
    format!(
        "<!DOCTYPE html>\n\
<html><head><meta charset=\"utf-8\"><title>{title}</title>\n\
<style>\n\
:root {{ color-scheme: light dark; }}\n\
body {{ margin: 0; font: 13px -apple-system, sans-serif; }}\n\
.model img {{ max-width: 100%; height: auto; display: block; margin: 0 auto; }}\n\
.notes {{ margin: 8px 12px; padding: 6px 10px 6px 28px; border-radius: 6px;\n         \
background: rgba(255, 196, 0, 0.18); }}\n\
pre {{ margin: 0; padding: 12px; font: 12px ui-monospace, Menlo, monospace;\n      \
white-space: pre-wrap; overflow-wrap: anywhere; tab-size: 4; }}\n\
</style></head>\n\
<body>{image}{note_list}<pre>{}</pre></body></html>",
        escape_html(source)
    )
}
