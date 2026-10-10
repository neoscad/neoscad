//! The desktop agent protocol: requests answered from a fake app.

use std::sync::Mutex;

use super::*;
use crate::SourceRange;

/// An app with one text per document, counting versions as an editor
/// would, and recording what the agent asked of it.
#[derive(Default)]
struct FakeApp {
    texts: Mutex<Vec<(u64, String, u64)>>,
    calls: Mutex<Vec<String>>,
    decline: bool,
}

impl FakeApp {
    fn with(docs: &[(u64, &str)]) -> FakeApp {
        FakeApp {
            texts: Mutex::new(docs.iter().map(|(id, t)| (*id, t.to_string(), 1)).collect()),
            ..FakeApp::default()
        }
    }

    fn log(&self, s: String) {
        self.calls.lock().unwrap().push(s);
    }
}

impl AgentHost for FakeApp {
    fn read(&self, document: u64) -> Result<AgentDocumentState, String> {
        self.log(format!("read {document}"));
        let texts = self.texts.lock().unwrap();
        let (_, text, version) = texts
            .iter()
            .find(|t| t.0 == document)
            .ok_or("closed")?
            .clone();
        Ok(AgentDocumentState {
            version,
            text,
            selection: Some(EditorSelection {
                anchor: EditorPosition {
                    line: 0,
                    character: 2,
                },
                head: EditorPosition {
                    line: 0,
                    character: 2,
                },
            }),
            overrides: vec![ParameterOverride {
                name: "teeth".into(),
                value: ParameterValue::Number { value: 12.0 },
            }],
            parts: false,
            enable: vec!["sketch".into(), "exact".into()],
            run: AgentRunStatus {
                mode: Some(RenderMode::Preview),
                summary: "Previewed".into(),
                running: false,
            },
            console: vec![
                ConsoleLine {
                    kind: ConsoleKind::Warning,
                    text: "unknown variable".into(),
                    location: Some(SourceRange {
                        path: "/models/gears.scad".into(),
                        start_line: 2,
                        start_character: 0,
                        end_line: 2,
                        end_character: 3,
                    }),
                },
                ConsoleLine {
                    kind: ConsoleKind::Echo,
                    text: "ECHO: 1".into(),
                    location: Some(SourceRange {
                        path: "/models/lib.scad".into(),
                        start_line: 0,
                        start_character: 0,
                        end_line: 0,
                        end_character: 1,
                    }),
                },
            ],
        })
    }

    fn edit(&self, document: u64, edit: AgentEditRequest) -> Result<AgentEditOutcome, String> {
        self.log(format!(
            "edit {document} v{} {} edits '{}' by {:?}",
            edit.version,
            edit.edits.len(),
            edit.summary,
            edit.client
        ));
        let mut texts = self.texts.lock().unwrap();
        let t = texts.iter_mut().find(|t| t.0 == document).ok_or("closed")?;
        if t.2 != edit.version {
            return Ok(AgentEditOutcome::Stale { version: t.2 });
        }
        if self.decline {
            return Ok(AgentEditOutcome::Declined);
        }
        t.2 += 1;
        Ok(AgentEditOutcome::Applied { version: t.2 })
    }

    fn reveal(
        &self,
        document: u64,
        from: EditorPosition,
        to: EditorPosition,
    ) -> Result<(), String> {
        self.log(format!(
            "reveal {document} {}:{}-{}:{}",
            from.line, from.character, to.line, to.character
        ));
        Ok(())
    }

    fn camera(&self, document: u64, change: AgentCameraChange) -> Result<AgentCamera, String> {
        self.log(format!("camera {document} {change:?}"));
        Ok(AgentCamera {
            vpt: vec![0.0; 3],
            vpr: vec![55.0, 0.0, 25.0],
            vpd: change.vpd.unwrap_or(140.0),
            vpf: 22.5,
        })
    }

    fn capture(&self, document: u64, max_side: u32) -> Result<AgentCapture, String> {
        self.log(format!("capture {document} {max_side}"));
        Ok(AgentCapture {
            png: b"\x89PNG\r\n\x1a\nrest".to_vec(),
            width: max_side,
            height: max_side / 2,
            camera: AgentCamera {
                vpt: vec![0.0; 3],
                vpr: vec![0.0; 3],
                vpd: 100.0,
                vpf: 22.5,
            },
            backend: "Test".into(),
        })
    }

    fn annotate(
        &self,
        document: u64,
        lines: Vec<ViewLine>,
        markers: Vec<ViewMarker>,
    ) -> Result<(), String> {
        self.log(format!(
            "annotate {document} {} lines {} markers {:?}",
            lines.len(),
            markers.len(),
            markers.first().map(|m| (&m.label, &m.color))
        ));
        Ok(())
    }
}

fn two_docs() -> AgentDocuments {
    let mut d = AgentDocuments::default();
    assert!(d.open(1, "gears.scad", Some("/models/gears.scad")));
    assert!(d.open(2, "Untitled", None));
    assert!(d.focus(1, 1000));
    assert!(d.focus(2, 2000));
    d
}

#[test]
fn the_last_focused_document_is_the_default() {
    let mut d = two_docs();
    assert_eq!(d.target(None).unwrap().id, 2);
    assert_eq!(d.target(Some(1)).unwrap().id, 1);
    assert!(d.target(Some(9)).unwrap_err().contains("not open"));
    // Focus in the same millisecond (or with a clock that stepped back)
    // still puts the newly focused one first.
    assert!(d.focus(1, 2000));
    assert_eq!(d.target(None).unwrap().id, 1);
    assert!(d.list()[0].focused_ms > d.list()[1].focused_ms);
    // Renaming changes the entry in place; the same name changes nothing.
    assert!(!d.open(1, "gears.scad", Some("/models/gears.scad")));
    assert!(d.open(1, "gear2.scad", Some("/models/gear2.scad")));
    assert_eq!(d.target(None).unwrap().file, "gear2.scad");
    assert!(d.close(1));
    assert!(!d.close(1));
    assert_eq!(d.target(None).unwrap().id, 2);
    assert!(d.close(2));
    assert!(d.target(None).unwrap_err().contains("no document"));
}

#[test]
fn nothing_is_read_or_changed_until_the_user_allows_it() {
    let app = FakeApp::with(&[(1, "cube(1);")]);
    let docs = two_docs();
    for method in ["read", "edit", "capture", "documents", "console"] {
        let e = handle_request(&app, &docs, false, None, method, &json!({})).unwrap_err();
        assert_eq!(e, NOT_ALLOWED);
    }
    assert!(app.calls.lock().unwrap().is_empty());
}

#[test]
fn read_answers_in_the_web_pages_shape() {
    let app = FakeApp::with(&[(1, "cube(1);\n"), (2, "sphere(2);")]);
    let docs = two_docs();
    let r = handle_request(&app, &docs, true, None, "read", &json!({"document": 1})).unwrap();
    assert_eq!(r["document"], 1);
    assert_eq!(r["file"], "gears.scad");
    assert_eq!(r["path"], "/models/gears.scad");
    assert_eq!(r["text"], "cube(1);\n");
    assert_eq!(r["version"], 1);
    assert_eq!(r["selection"]["anchor"], json!([0, 2]));
    assert_eq!(r["values"]["teeth"], 12.0);
    assert_eq!(r["run"]["summary"], "Previewed");
    assert_eq!(r["run"]["mode"], "preview");
    // The app's extensions, which `neoscad mcp` adds to its own `--enable`
    // for this text.
    assert_eq!(r["enable"], json!(["sketch", "exact"]));
    // Errors and warnings only, 1-based, the file named only when it is
    // not the document.
    assert_eq!(
        r["diagnostics"],
        json!([{"kind": "warning", "text": "unknown variable", "line": 3}])
    );
    // Without `document`: the most recently focused.
    let r = handle_request(&app, &docs, true, None, "read", &json!({})).unwrap();
    assert_eq!(r["document"], 2);
    assert_eq!(r["path"], Value::Null);
    let c = handle_request(&app, &docs, true, None, "console", &json!({"document": 1})).unwrap();
    assert_eq!(c["lines"][1]["file"], "lib.scad");
    assert_eq!(c["state"], "idle");
    let e = handle_request(&app, &docs, true, None, "read", &json!({"document": "x"})).unwrap_err();
    assert!(e.contains("document"), "{e}");
}

#[test]
fn edits_are_versioned_sorted_and_said_to_be_declined() {
    let app = FakeApp::with(&[(1, "cube(1);"), (2, "x")]);
    let docs = two_docs();
    let edit = |v: u64, edits: Value| {
        handle_request(
            &app,
            &docs,
            true,
            Some("Claude Code"),
            "edit",
            &json!({"document": 1, "version": v, "edits": edits, "summary": "line 1"}),
        )
    };
    let one = json!([{"from": [0, 5], "to": [0, 6], "insert": "2"}]);
    let r = edit(1, one.clone()).unwrap();
    assert_eq!(r["version"], 2);
    // The agent's version is now stale.
    let e = edit(1, one.clone()).unwrap_err();
    assert!(e.contains("version 2, not 1"), "{e}");
    // Out of order is sorted; overlapping is refused before the app sees it.
    let r = edit(
        2,
        json!([{"from": [0, 6], "to": [0, 7], "insert": ""},
               {"from": [0, 0], "to": [0, 1], "insert": "C"}]),
    )
    .unwrap();
    assert_eq!(r["version"], 3);
    let e = edit(
        3,
        json!([{"from": [0, 0], "to": [0, 4], "insert": ""},
               {"from": [0, 2], "to": [0, 5], "insert": ""}]),
    )
    .unwrap_err();
    assert!(e.contains("overlap"), "{e}");
    assert!(edit(3, json!([])).unwrap_err().contains("no edits"));
    assert!(
        edit(3, json!([{"from": [0, 4], "to": [0, 1], "insert": ""}]))
            .unwrap_err()
            .contains("ends before")
    );
    assert!(
        edit(3, json!([{"from": [0], "to": [0, 1], "insert": ""}]))
            .unwrap_err()
            .contains("edit 1 must be")
    );
    let calls = app.calls.lock().unwrap().clone();
    assert!(
        calls[0].contains("edit 1 v1 1 edits 'line 1' by Some(\"Claude Code\")"),
        "{calls:?}"
    );
    // The user's "Reject".
    let declining = FakeApp {
        decline: true,
        ..FakeApp::with(&[(1, "a"), (2, "b")])
    };
    let e = handle_request(
        &declining,
        &docs,
        true,
        None,
        "edit",
        &json!({"version": 1, "edits": one}),
    )
    .unwrap_err();
    assert!(e.contains("declined"), "{e}");
}

#[test]
fn view_requests_are_bounded_before_the_app_draws() {
    let app = FakeApp::with(&[(1, ""), (2, "")]);
    let docs = two_docs();
    let call = |m: &str, p: Value| handle_request(&app, &docs, true, None, m, &p);
    let c = call("capture", json!({})).unwrap();
    assert_eq!(c["width"], CAPTURE_DEFAULT);
    assert!(c["png"].as_str().unwrap().starts_with("iVBORw0KGg"));
    assert!(call("capture", json!({"size": 4096})).is_err());
    assert!(call("capture", json!({"size": 10})).is_err());
    let cam = call("camera", json!({"view": "iso", "vpd": 50})).unwrap();
    assert_eq!(cam["vpd"], 50.0);
    assert!(call("camera", json!({"view": "sideways"})).is_err());
    assert!(call("camera", json!({"vpt": [1, 2]})).is_err());
    assert!(call("camera", json!({"vpd": -1})).is_err());
    call(
        "annotate",
        json!({"markers": [{"point": [1, 2, 3], "label": "a".repeat(60), "color": "#ff0000"}],
               "lines": [{"points": [[0, 0, 0], [1, 1, 1]]}]}),
    )
    .unwrap();
    let too_many: Vec<Value> = (0..=MAX_MARKS)
        .map(|_| json!({"point": [0, 0, 0]}))
        .collect();
    assert!(call("annotate", json!({"markers": too_many})).is_err());
    assert!(
        call(
            "annotate",
            json!({"markers": [{"point": [0, 0, 0], "color": "red"}]})
        )
        .is_err()
    );
    call("reveal", json!({"from": [2, 0], "to": [1, 0]})).unwrap();
    assert!(call("reveal", json!({"from": [2]})).is_err());
    assert!(
        call("frobnicate", json!({}))
            .unwrap_err()
            .contains("frobnicate")
    );
    let calls = app.calls.lock().unwrap().clone();
    assert!(calls.contains(&"capture 2 768".to_string()), "{calls:?}");
    // Labels are cut, colours parsed; a range never ends before it starts.
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("annotate 2 1 lines 1 markers")
                && c.contains(&format!("\"{}\"", "a".repeat(40)))
                && c.contains("[1.0, 0.0, 0.0, 1.0]")),
        "{calls:?}"
    );
    assert!(calls.contains(&"reveal 2 2:0-2:0".to_string()), "{calls:?}");
}

#[test]
fn messages_read_and_written() {
    assert_eq!(
        incoming(&json!({"type": "welcome", "client": "Claude Code", "protocol": 1})),
        Incoming::Welcome {
            client: Some("Claude Code".into()),
            server: None,
            protocol: 1
        }
    );
    assert_eq!(
        incoming(&json!({"type": "activity", "tool": "check", "document": 2})),
        Incoming::Activity {
            tool: Some("check".into()),
            document: Some(2)
        }
    );
    assert!(matches!(
        incoming(&json!({"id": 7, "method": "read", "params": {}})),
        Incoming::Request { id: 7, .. }
    ));
    assert_eq!(incoming(&json!({"type": "future"})), Incoming::Other);
    let docs = two_docs();
    let h = hello("NeoSCAD", "0.3.1", "macos", docs.list());
    assert_eq!(h["protocol"], AGENT_PROTOCOL);
    assert_eq!(h["documents"][0]["id"], 2);
    assert_eq!(
        documents_note(docs.list())["documents"][1]["file"],
        "gears.scad"
    );
    assert_eq!(reply(3, Err("no".into()))["error"]["message"], "no");
    assert_eq!(reply(3, Ok(json!({"a": 1})))["result"]["a"], 1);
    assert_eq!(bye("disconnected by the user", false)["reconnect"], false);
}

#[test]
fn status_lines_and_base64() {
    let mut s = AgentStatus::default();
    assert_eq!(status_line(&s), "Connect your AI agent");
    s.clients.push(AgentClient {
        id: 1,
        name: Some("Claude Code".into()),
        activity: None,
        document: None,
    });
    assert_eq!(status_line(&s), "Claude Code connected");
    s.clients[0].activity = Some(activity_text("edit").into());
    assert_eq!(status_line(&s), "Claude Code is editing");
    s.clients.push(AgentClient {
        id: 2,
        name: None,
        activity: None,
        document: None,
    });
    assert_eq!(status_line(&s), "Claude Code is editing");
    s.clients[0].activity = None;
    assert_eq!(status_line(&s), "2 agents connected");
    assert_eq!(activity_text("check"), "is checking the model");
    assert_eq!(base64(b""), "");
    assert_eq!(base64(b"f"), "Zg==");
    assert_eq!(base64(b"fo"), "Zm8=");
    assert_eq!(base64(b"foo"), "Zm9v");
    assert_eq!(base64(b"\x89PNG"), "iVBORw==");
}
