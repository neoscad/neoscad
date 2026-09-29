//! `neoscad lsp --stdio` end to end: the protocol's framing, the
//! lifecycle, requests answered in order, and diagnostics pushed after
//! the debounce, for a file on disk and its include.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::Duration;

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_neoscad");

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nslsp-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    // Plain, not `\\?\` verbatim on Windows, as a client names files.
    lang::paths::plain(d.canonicalize().unwrap())
}

struct Lsp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

/// An editor's URI for `p`: `file:///tmp/x.scad`, or on Windows
/// `file:///C:/Users/x.scad` (RFC 8089).
fn uri(p: &std::path::Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let slash = if s.starts_with('/') { "" } else { "/" };
    format!("file://{slash}{s}").replace(' ', "%20")
}

impl Lsp {
    fn start() -> Lsp {
        let mut child = Command::new(BIN)
            .args(["lsp", "--stdio"])
            .env_remove("OPENSCADPATH")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Lsp {
            stdin: child.stdin.take().unwrap(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
            next: 0,
        }
    }

    fn send(&mut self, v: &Value) {
        let body = v.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn read(&mut self) -> Value {
        let mut len = 0usize;
        loop {
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "server closed"
            );
            let line = line.trim_end();
            if line.is_empty() {
                break;
            }
            let (k, v) = line.split_once(':').unwrap();
            assert_eq!(k, "Content-Length");
            len = v.trim().parse().unwrap();
        }
        let mut body = vec![0; len];
        self.stdout.read_exact(&mut body).unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let m = self.read();
            if m["id"] == id {
                return m;
            }
            // Diagnostics may arrive in between.
            assert_eq!(m["method"], "textDocument/publishDiagnostics", "{m}");
        }
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }
}

#[test]
fn stdio_session() {
    let dir = scratch("session");
    let main = dir.join("main.scad");
    let inc = dir.join("parts.scad");
    std::fs::write(
        &inc,
        "// A peg.\nmodule peg(h = 5) cylinder(h = h, r = 1);\nmissing_thing();\n",
    )
    .unwrap();
    let text = "include <parts.scad>\npeg(h=3);\ncub(2);\n";
    std::fs::write(&main, text).unwrap();
    let mut s = Lsp::start();
    let init = s.request(
        "initialize",
        json!({"processId": null, "rootUri": uri(&dir), "capabilities": {}}),
    );
    assert_eq!(init["result"]["capabilities"]["positionEncoding"], "utf-16");
    assert_eq!(init["result"]["serverInfo"]["name"], "neoscad");
    s.notify("initialized", json!({}));
    s.notify(
        "textDocument/didOpen",
        json!({"textDocument": {"uri": uri(&main), "languageId": "openscad", "version": 1, "text": text}}),
    );
    // Requests are answered at once, ahead of the diagnostics.
    let h = s.request(
        "textDocument/hover",
        json!({"textDocument": {"uri": uri(&main)}, "position": {"line": 1, "character": 1}}),
    );
    let v = h["result"]["contents"]["value"].as_str().unwrap();
    assert!(
        v.contains("module peg(h=5)") && v.contains("A peg.") && v.contains("parts.scad:2"),
        "{v}"
    );
    let c = s.request(
        "textDocument/completion",
        json!({"textDocument": {"uri": uri(&main)}, "position": {"line": 2, "character": 3}}),
    );
    let items = c["result"]["items"].as_array().unwrap();
    assert!(items.iter().any(|i| i["label"] == "cube"));
    let d = s.request(
        "textDocument/definition",
        json!({"textDocument": {"uri": uri(&main)}, "position": {"line": 1, "character": 0}}),
    );
    assert_eq!(d["result"]["uri"], uri(&inc));
    // The diagnostics come after the pause: the document's, and the
    // included file's under its own URI.
    let mut got = Vec::new();
    while got.len() < 2 {
        let m = s.read();
        assert_eq!(m["method"], "textDocument/publishDiagnostics", "{m}");
        got.push(m["params"].clone());
    }
    let mine = got.iter().find(|p| p["uri"] == uri(&main)).unwrap();
    assert_eq!(mine["version"], 1);
    let codes: Vec<&str> = mine["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["code"].as_str().unwrap())
        .collect();
    assert!(
        codes.iter().filter(|c| **c == "unknown-module").count() == 2,
        "{codes:?}"
    );
    let theirs = got.iter().find(|p| p["uri"] == uri(&inc)).unwrap();
    assert_eq!(theirs["diagnostics"][0]["range"]["start"]["line"], 2);
    // An incremental change, then formatting sees it.
    s.notify(
        "textDocument/didChange",
        json!({"textDocument": {"uri": uri(&main), "version": 2}, "contentChanges": [
            {"range": {"start": {"line": 2, "character": 0}, "end": {"line": 2, "character": 7}}, "text": "cube( 2 );"}
        ]}),
    );
    let f = s.request("textDocument/formatting", json!({"textDocument": {"uri": uri(&main)}, "options": {"tabSize": 4, "insertSpaces": true}}));
    let edits = f["result"].as_array().unwrap();
    assert_eq!(edits.len(), 1, "{f}");
    assert_eq!(edits[0]["newText"], "cube(2);\n");
    // The diagnostics of version 2: only the include's problem is left,
    // shown on the include line.
    loop {
        let m = s.read();
        if m["params"]["uri"] == uri(&main) {
            assert_eq!(m["params"]["version"], 2);
            let list = m["params"]["diagnostics"].as_array().unwrap();
            assert_eq!(list.len(), 1, "{list:?}");
            assert_eq!(list[0]["range"]["start"]["line"], 0);
            break;
        }
    }
    let r = s.request("shutdown", Value::Null);
    assert_eq!(r["result"], Value::Null);
    s.notify("exit", Value::Null);
    let status = wait(&mut s.child);
    assert_eq!(status, Some(0));
}

#[test]
fn exit_without_shutdown_is_an_error() {
    let mut s = Lsp::start();
    s.request("initialize", json!({"capabilities": {}}));
    s.notify("exit", Value::Null);
    assert_eq!(wait(&mut s.child), Some(1));
}

fn wait(child: &mut Child) -> Option<i32> {
    for _ in 0..100 {
        if let Some(st) = child.try_wait().unwrap() {
            return st.code();
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let _ = child.kill();
    None
}
