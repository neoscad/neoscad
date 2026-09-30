//! The host's end of the editor bridge, without the web view.
//!
//! The editor is the macOS app's CodeMirror bundle (`apple/Editor/web`),
//! unchanged: it posts to `window.webkit.messageHandlers.editor`, which
//! WebKitGTK provides as it does WKWebView, and the host calls
//! `window.NeoSCADEditor.<function>` back. The protocol is the one
//! `apple/App/Editor/EditorController.swift` documents (its header); this
//! module is its state machine, so the GTK window only moves messages.
//!
//! Who owns the text: CodeMirror, while editing (selection, undo). Every
//! transaction comes here in order and is applied to the host's copy
//! ([`client::EditorText`]), which is what Save writes and what the core's
//! buffer is edited from. Each change names the editor version it applies
//! to (`base`); one that does not fit the copy (a lost message, an offset
//! past the end, a length that disagrees) makes the copy stale, and the
//! host asks the editor for its whole text (a resync) instead of guessing.

use client::{EditorText, TextEdit, Utf16Edit};
use serde_json::Value;

/// How a transaction changed the text, for the document's change count:
/// an undo takes a change back, so undoing to the saved text is clean
/// again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditKind {
    Edit,
    Undo,
    Redo,
}

/// One message from the page.
#[derive(Debug, Clone, PartialEq)]
pub enum Incoming {
    /// The page is up: the host loads the text.
    Ready,
    /// One transaction.
    Changes(Changes),
    /// A key the app owns (F5 "preview", F6 "render").
    Command(String),
    /// A JSON-RPC message for the language server.
    Lsp(String),
    /// Go to a definition in another file (0-based line, UTF-16 column).
    Open {
        uri: String,
        line: u64,
        character: u64,
    },
    /// A script error, for the log.
    Log { message: String },
    /// A message this host does not know; logged, never fatal, so an
    /// editor bundle newer than the app still edits.
    Unknown(String),
}

/// A `changes` message. Every field is optional here because a message
/// that lacks one is a disagreement to recover from, not a crash.
#[derive(Debug, Clone, PartialEq)]
pub struct Changes {
    pub base: Option<u64>,
    pub version: Option<u64>,
    /// `None` when an edit was malformed (not `[from, to, insert]`).
    pub edits: Option<Vec<Utf16Edit>>,
    pub kind: EditKind,
    /// The editor's UTF-16 length after the transaction.
    pub length: Option<u64>,
    pub undo_depth: Option<u64>,
    pub redo_depth: Option<u64>,
}

/// Parse one posted message; `None` for something that is not an object
/// with a `type`.
pub fn parse(m: &Value) -> Option<Incoming> {
    let ty = m.get("type")?.as_str()?;
    let s = |k: &str| m.get(k).and_then(Value::as_str).map(str::to_string);
    let n = |k: &str| m.get(k).and_then(Value::as_u64);
    Some(match ty {
        "ready" => Incoming::Ready,
        "changes" => Incoming::Changes(Changes {
            base: n("base"),
            version: n("version"),
            edits: m.get("edits").and_then(parse_edits),
            kind: match m.get("kind").and_then(Value::as_str) {
                Some("undo") => EditKind::Undo,
                Some("redo") => EditKind::Redo,
                _ => EditKind::Edit,
            },
            length: n("length"),
            undo_depth: n("undoDepth"),
            redo_depth: n("redoDepth"),
        }),
        "command" => Incoming::Command(s("name")?),
        "lsp" => Incoming::Lsp(s("message")?),
        "open" => Incoming::Open {
            uri: s("uri")?,
            line: n("line").unwrap_or(0),
            character: n("character").unwrap_or(0),
        },
        "log" => Incoming::Log {
            message: s("message").unwrap_or_default(),
        },
        other => Incoming::Unknown(other.to_string()),
    })
}

/// `[[from, to, insert], ...]`; `None` if any edit is not that shape.
fn parse_edits(v: &Value) -> Option<Vec<Utf16Edit>> {
    v.as_array()?
        .iter()
        .map(|e| match e.as_array()?.as_slice() {
            [from, to, insert] => Some(Utf16Edit {
                from: from.as_u64()?,
                to: to.as_u64()?,
                insert: insert.as_str()?.to_string(),
            }),
            _ => None,
        })
        .collect()
}

/// What the host does after a transaction.
#[derive(Debug, Clone, PartialEq)]
pub enum ChangeOutcome {
    /// A load or a resync is on its way and will replace this text: drop
    /// the transaction.
    Ignored,
    /// Applied to the copy: forward `edits` (UTF-8 offsets) to the core
    /// and count the change as `kind`.
    Applied {
        edits: Vec<TextEdit>,
        kind: EditKind,
    },
    /// It did not fit: ask the editor for its text
    /// (`NeoSCADEditor.text()`) and hand the answer to
    /// [`EditorBridge::resynced`].
    Resync,
}

/// The bridge's state for one editor page.
#[derive(Debug, Default, Clone)]
pub struct EditorBridge {
    ready: bool,
    /// The editor version the host's copy matches; `None` while a load or
    /// a resync is on its way.
    version: Option<u64>,
    pub undo_depth: u64,
    pub redo_depth: u64,
    /// Transactions applied, and how many forced a resync (tests, logs).
    pub change_count: u64,
    pub resync_count: u64,
}

impl EditorBridge {
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    pub fn version(&self) -> Option<u64> {
        self.version
    }

    /// The page said `ready`: the host now calls `load`.
    pub fn page_ready(&mut self) {
        self.ready = true;
        self.version = None;
    }

    /// The page went away (the web process ended, the page reloads).
    pub fn page_lost(&mut self) {
        self.ready = false;
        self.version = None;
    }

    /// A load was sent: changes are dropped until its answer arrives.
    pub fn load_sent(&mut self) {
        self.version = None;
    }

    /// The answer of `load` (or any call returning the history state:
    /// `{version, undoDepth, redoDepth}`).
    pub fn history(&mut self, state: &Value) {
        if let Some(v) = state.get("version").and_then(Value::as_u64) {
            self.version = Some(v);
        }
        if let Some(u) = state.get("undoDepth").and_then(Value::as_u64) {
            self.undo_depth = u;
        }
        if let Some(r) = state.get("redoDepth").and_then(Value::as_u64) {
            self.redo_depth = r;
        }
    }

    /// One transaction, applied to `text` if it fits.
    pub fn changes(&mut self, c: Changes, text: &mut EditorText) -> ChangeOutcome {
        if let Some(u) = c.undo_depth {
            self.undo_depth = u;
        }
        if let Some(r) = c.redo_depth {
            self.redo_depth = r;
        }
        let Some(version) = self.version else {
            return ChangeOutcome::Ignored;
        };
        let (Some(base), Some(next), Some(edits), Some(length)) =
            (c.base, c.version, c.edits, c.length)
        else {
            return self.resync();
        };
        if base != version {
            return self.resync();
        }
        match text.apply(&edits) {
            Ok(edits) if text.utf16_length() == length => {
                self.version = Some(next);
                self.change_count += 1;
                ChangeOutcome::Applied {
                    edits,
                    kind: c.kind,
                }
            }
            _ => self.resync(),
        }
    }

    fn resync(&mut self) -> ChangeOutcome {
        self.version = None;
        self.resync_count += 1;
        ChangeOutcome::Resync
    }

    /// The editor's answer to `text()` (`{version, text}`) after a
    /// resync: the text to replace the copy with, which is now in step at
    /// that version.
    pub fn resynced(&mut self, answer: &Value) -> Option<String> {
        let v = answer.get("version").and_then(Value::as_u64)?;
        let t = answer.get("text").and_then(Value::as_str)?;
        self.version = Some(v);
        Some(t.to_string())
    }
}

/// A script calling `NeoSCADEditor.<function>` with `args`, spliced as
/// JSON literals so no text is ever interpreted as code (WebKitGTK's
/// `call_async_javascript_function` takes its arguments as a GVariant
/// dictionary; building the call as JSON keeps this testable and the one
/// quoting rule in one place).
pub fn call_script(function: &str, args: &[Value]) -> String {
    let args: Vec<String> = args.iter().map(Value::to_string).collect();
    format!(
        "return window.NeoSCADEditor.{function}({});",
        args.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn changes(base: u64, version: u64, edits: Value, length: u64) -> Changes {
        match parse(&json!({
            "type": "changes", "base": base, "version": version, "edits": edits,
            "kind": "edit", "length": length, "undoDepth": 1, "redoDepth": 0,
        })) {
            Some(Incoming::Changes(c)) => c,
            other => panic!("not changes: {other:?}"),
        }
    }

    fn loaded(bridge: &mut EditorBridge, version: u64) {
        bridge.page_ready();
        bridge.load_sent();
        bridge.history(&json!({"version": version, "undoDepth": 0, "redoDepth": 0}));
    }

    #[test]
    fn parses_every_message_type() {
        assert_eq!(parse(&json!({"type": "ready"})), Some(Incoming::Ready));
        assert_eq!(
            parse(&json!({"type": "command", "name": "render"})),
            Some(Incoming::Command("render".into()))
        );
        assert_eq!(
            parse(&json!({"type": "open", "uri": "file:///a.scad", "line": 3, "character": 4})),
            Some(Incoming::Open {
                uri: "file:///a.scad".into(),
                line: 3,
                character: 4
            })
        );
        assert_eq!(
            parse(&json!({"type": "later"})),
            Some(Incoming::Unknown("later".into()))
        );
        assert_eq!(parse(&json!("ready")), None);
        assert_eq!(parse(&json!({"type": "command"})), None);
    }

    #[test]
    fn a_change_on_the_current_version_edits_the_copy_in_utf8_offsets() {
        let mut b = EditorBridge::default();
        let mut text = EditorText::new("é = 1;".into());
        loaded(&mut b, 1);
        // "é" is one UTF-16 unit and two UTF-8 bytes: offset 4 is byte 5.
        let out = b.changes(changes(1, 2, json!([[4, 5, "2"]]), 6), &mut text);
        assert_eq!(
            out,
            ChangeOutcome::Applied {
                edits: vec![TextEdit {
                    start: 5,
                    end: 6,
                    text: "2".into()
                }],
                kind: EditKind::Edit
            }
        );
        assert_eq!(text.text(), "é = 2;");
        assert_eq!(b.version(), Some(2));
        assert_eq!(b.undo_depth, 1);
    }

    #[test]
    fn changes_before_the_load_answers_are_dropped() {
        let mut b = EditorBridge::default();
        let mut text = EditorText::new("a".into());
        b.page_ready();
        let out = b.changes(changes(0, 1, json!([[0, 0, "x"]]), 2), &mut text);
        assert_eq!(out, ChangeOutcome::Ignored);
        assert_eq!(text.text(), "a");
    }

    #[test]
    fn a_stale_base_a_bad_offset_or_a_wrong_length_resyncs() {
        let mut text = EditorText::new("abc".into());
        let mut b = EditorBridge::default();
        loaded(&mut b, 5);
        assert_eq!(
            b.changes(changes(4, 6, json!([[0, 0, "x"]]), 4), &mut text),
            ChangeOutcome::Resync
        );
        assert_eq!(b.version(), None, "waits for the editor's text");

        loaded(&mut b, 5);
        assert_eq!(
            b.changes(changes(5, 6, json!([[9, 9, "x"]]), 4), &mut text),
            ChangeOutcome::Resync
        );
        loaded(&mut b, 5);
        assert_eq!(
            b.changes(changes(5, 6, json!([[0, 0, "x"]]), 99), &mut text),
            ChangeOutcome::Resync
        );
        loaded(&mut b, 5);
        assert_eq!(
            b.changes(changes(5, 6, json!([[0, "x"]]), 4), &mut text),
            ChangeOutcome::Resync
        );
        assert_eq!(b.resync_count, 4);
        assert_eq!(
            b.resynced(&json!({"version": 9, "text": "fresh"})),
            Some("fresh".into())
        );
        assert_eq!(b.version(), Some(9));
    }

    #[test]
    fn edits_of_one_transaction_apply_in_order() {
        let mut b = EditorBridge::default();
        let mut text = EditorText::new("cube(1);\nsphere(2);".into());
        loaded(&mut b, 1);
        // Last change first, as bridge.js sends them.
        let out = b.changes(
            changes(1, 2, json!([[16, 17, "3"], [5, 6, "10"]]), 20),
            &mut text,
        );
        assert!(matches!(out, ChangeOutcome::Applied { .. }), "{out:?}");
        assert_eq!(text.text(), "cube(10);\nsphere(3);");
    }

    #[test]
    fn scripts_quote_their_arguments_as_json() {
        assert_eq!(
            call_script(
                "load",
                &[json!("a\"); alert(1); //"), Value::Null, json!(false)]
            ),
            r#"return window.NeoSCADEditor.load("a\"); alert(1); //", null, false);"#
        );
    }
}
