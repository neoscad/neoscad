//! `neoscad mcp --browser` end to end: the bridge's checks (Host, Origin,
//! token), the one-tab policy, and every browser tool over MCP stdio with
//! a fake tab on the WebSocket answering as the web page does
//! (web/src/agent/). The real page is driven by web/e2e/agent.spec.js.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tungstenite::{Message, WebSocket};

const BIN: &str = env!("CARGO_BIN_EXE_neoscad");
/// The page origin these tests connect as (`--browser-url`).
const PAGE: &str = "http://127.0.0.1:9/try/";
const ORIGIN: &str = "http://127.0.0.1:9";
/// A 1x1 PNG, as a capture's answer.
const PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nsbrowser-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    lang::paths::plain(d.canonicalize().unwrap())
}

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Mcp {
    fn start(dir: &Path, args: &[&str]) -> Mcp {
        let mut child = Command::new(BIN)
            .arg("mcp")
            .args(args)
            .current_dir(dir)
            .env_remove("OPENSCADPATH")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Mcp {
            stdin: child.stdin.take().unwrap(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
            next: 1,
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        assert!(
            self.stdout.read_line(&mut line).unwrap() > 0,
            "server closed"
        );
        let r: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(r["id"], id, "{r}");
        r
    }

    fn tool(&mut self, name: &str, args: Value) -> Value {
        self.call("tools/call", json!({"name": name, "arguments": args}))["result"].clone()
    }

    /// The port and token of the link browser_connect gives.
    fn link(&mut self) -> (u16, String) {
        let t = text(&self.tool("browser_connect", json!({})));
        let at = t.find("#connect=").expect(&t) + "#connect=".len();
        let (port, token) = t[at..]
            .split_whitespace()
            .next()
            .unwrap()
            .split_once('.')
            .unwrap();
        (port.parse().unwrap(), token.to_string())
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text(r: &Value) -> String {
    r["content"][0]["text"].as_str().unwrap_or("").to_string()
}

/// A raw HTTP request to the bridge: the status line of its answer, and
/// the whole answer.
fn http(port: u16, head: &str) -> (String, String) {
    let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    s.write_all(head.as_bytes()).unwrap();
    // Until the server closes, or the head of a 101 (whose socket stays
    // open for the tab's messages).
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    while let Ok(n @ 1..) = s.read(&mut chunk) {
        buf.extend_from_slice(&chunk[..n]);
        if buf.starts_with(b"HTTP/1.1 101") && buf.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let out = String::from_utf8_lossy(&buf).into_owned();
    (out.lines().next().unwrap_or("").to_string(), out)
}

fn upgrade(token: &str, origin: &str, host: &str) -> String {
    format!(
        "GET /ws?token={token} HTTP/1.1\r\nHost: {host}\r\nOrigin: {origin}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
    )
}

#[test]
fn only_the_page_origin_with_the_token_on_this_host_connects() {
    let dir = scratch("checks");
    let mut s = Mcp::start(&dir, &["--browser", "--browser-url", PAGE]);
    let (port, token) = s.link();
    let host = format!("127.0.0.1:{port}");
    let status = |head: String| http(port, &head).0;
    assert!(status(upgrade(&token, ORIGIN, &host)).contains(" 101 "));
    assert!(status(upgrade("0123", ORIGIN, &host)).contains(" 403 "));
    assert!(status(upgrade("", ORIGIN, &host)).contains(" 403 "));
    assert!(status(upgrade(&token, "https://evil.example", &host)).contains(" 403 "));
    assert!(status(upgrade(&token, "https://neoscad.org", &host)).contains(" 403 "));
    // A DNS-rebound name for this address is not this server.
    assert!(status(upgrade(&token, ORIGIN, &format!("evil.example:{port}"))).contains(" 421 "));
    // The relay window's own origin connects (it is served here).
    assert!(
        status(upgrade(
            &token,
            &format!("http://localhost:{port}"),
            &format!("localhost:{port}")
        ))
        .contains(" 101 ")
    );
    // The relay page talks to the page's origin only, and cannot be framed.
    let (st, relay) = http(
        port,
        &format!("GET /relay HTTP/1.1\r\nHost: {host}\r\n\r\n"),
    );
    assert!(st.contains(" 200 "), "{st}");
    assert!(relay.contains(&format!("const PAGE_ORIGIN = \"{ORIGIN}\";")));
    assert!(relay.contains("frame-ancestors 'none'"));
    assert!(relay.contains("X-Frame-Options: DENY"));
    assert!(
        http(port, &format!("GET / HTTP/1.1\r\nHost: {host}\r\n\r\n"))
            .0
            .contains(" 404 ")
    );
    assert!(http(port, "garbage\r\n\r\n").0.contains(" 400 "));
}

/// What the fake tab saw: its requests, in order, and how it was closed.
#[derive(Debug, Default)]
struct Seen {
    requests: Vec<Value>,
    closed: Option<u16>,
}

/// The fake page's document.
struct Doc {
    text: String,
    version: u64,
}

/// `[line, UTF-16 column]` as a byte offset in `text` (the editor's
/// positions; the page does this with CodeMirror).
fn offset(text: &str, p: &Value) -> usize {
    let (l, c) = (
        p[0].as_u64().unwrap() as usize,
        p[1].as_u64().unwrap() as usize,
    );
    let start: usize = text.split('\n').take(l).map(|x| x.len() + 1).sum();
    let line = text[start..].split('\n').next().unwrap();
    let mut units = 0;
    for (i, ch) in line.char_indices() {
        if units >= c {
            return start + i;
        }
        units += ch.len_utf16();
    }
    start + line.len()
}

fn answer(doc: &mut Doc, method: &str, p: &Value) -> Result<Value, String> {
    Ok(match method {
        "read" => json!({
            "file": "box.scad", "version": doc.version, "text": doc.text,
            "selection": {"anchor": [1, 3], "head": [1, 4]},
            "values": {"size": 7}, "parts": false,
            "run": {"summary": "Previewed in 3 ms."},
            "diagnostics": [{"kind": "warning", "line": 3, "text": "a warning"}],
        }),
        "edit" => {
            if p["version"] != doc.version {
                return Err("stale".into());
            }
            let mut edits: Vec<&Value> = p["edits"].as_array().unwrap().iter().collect();
            edits.sort_by_key(|e| std::cmp::Reverse(offset(&doc.text, &e["from"])));
            for e in edits {
                let (a, z) = (offset(&doc.text, &e["from"]), offset(&doc.text, &e["to"]));
                doc.text.replace_range(a..z, e["insert"].as_str().unwrap());
            }
            doc.version += 1;
            json!({"version": doc.version})
        }
        "camera" => {
            let mut c = json!({"vpt": [0, 0, 0], "vpr": [55, 0, 25], "vpd": 140, "vpf": 22.5});
            for k in ["vpt", "vpr", "vpd"] {
                if !p[k].is_null() {
                    c[k] = p[k].clone();
                }
            }
            c
        }
        "capture" => json!({"png": PNG, "width": 1, "height": 1, "backend": "test",
                            "camera": {"vpt": [0, 0, 0], "vpr": [55, 0, 25], "vpd": 140, "vpf": 22.5}}),
        "console" => {
            json!({"summary": "Previewed in 3 ms.", "lines": [{"kind": "echo", "text": "ECHO: 7"}]})
        }
        _ => json!({}),
    })
}

/// Connect a fake tab that answers like the page; what it sees goes to
/// the returned log.
fn fake_tab(port: u16, token: &str, text: &str) -> Arc<Mutex<Seen>> {
    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    let req = tungstenite::http::Request::builder()
        .uri(format!("ws://127.0.0.1:{port}/ws?token={token}"))
        .header("Host", format!("127.0.0.1:{port}"))
        .header("Origin", ORIGIN)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header(
            "Sec-WebSocket-Key",
            tungstenite::handshake::client::generate_key(),
        )
        .body(())
        .unwrap();
    let (mut ws, _): (WebSocket<TcpStream>, _) = tungstenite::client(req, stream).unwrap();
    ws.send(Message::text(
        json!({"type": "hello", "file": "box.scad", "browser": "Test"}).to_string(),
    ))
    .unwrap();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let log = seen.clone();
    let mut doc = Doc {
        text: text.to_string(),
        version: 1,
    };
    std::thread::spawn(move || {
        loop {
            match ws.read() {
                Ok(Message::Text(t)) => {
                    let m: Value = serde_json::from_str(t.as_str()).unwrap();
                    let Some(id) = m["id"].as_u64() else { continue };
                    let method = m["method"].as_str().unwrap().to_string();
                    log.lock().unwrap().requests.push(m.clone());
                    let reply = match answer(&mut doc, &method, &m["params"]) {
                        Ok(r) => json!({"id": id, "result": r}),
                        Err(e) => json!({"id": id, "error": {"message": e}}),
                    };
                    if ws.send(Message::text(reply.to_string())).is_err() {
                        break;
                    }
                }
                Ok(Message::Close(f)) => {
                    log.lock().unwrap().closed = Some(f.map_or(0, |f| u16::from(f.code)));
                    let _ = ws.flush();
                }
                Ok(_) => {}
                Err(_) => break,
            }
        }
    });
    // Wait for the hello to be seen: the next request finds the tab.
    std::thread::sleep(Duration::from_millis(100));
    seen
}

const MODEL: &str = "size = 5;\n// øx\necho(size);\ncube(size);\n";

#[test]
fn the_tools_act_on_the_connected_page() {
    let dir = scratch("tools");
    let mut s = Mcp::start(&dir, &["--browser", "--browser-url", PAGE]);
    // Nothing connected yet: the page tools and the page default say how.
    let r = s.tool("editor_read", json!({}));
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("browser_connect"), "{r}");
    let r = s.tool("evaluate", json!({}));
    assert!(text(&r).contains("give path"), "{r}");
    assert!(text(&s.tool("browser_connect", json!({}))).contains("Not connected yet"));

    let (port, token) = s.link();
    let seen = fake_tab(port, &token, MODEL);
    let t = text(&s.tool("browser_connect", json!({})));
    assert!(
        t.starts_with("Connected: the web page's box.scad in Test (directly)"),
        "{t}"
    );

    // editor_read: numbered text, the version, the selection in byte
    // columns (the tab's [1, 3]-[1, 4] is the "ø", bytes 4 to 6), the
    // customizer and the last run's warnings.
    let t = text(&s.tool("editor_read", json!({})));
    assert!(
        t.starts_with("box.scad, version 1, 5 lines; selected 2:4-2:6; customizer: size=7"),
        "{t}"
    );
    assert!(
        t.contains("last run: Previewed in 3 ms.\nwarning line 3: a warning"),
        "{t}"
    );
    assert!(t.contains("     4\tcube(size);\n"), "{t}");

    // The model tools use the page's text and customizer values.
    let r = s.tool("evaluate", json!({}));
    let t = text(&r);
    assert!(
        t.starts_with("the web page's box.scad (version 1)\nok"),
        "{t}"
    );
    assert!(t.contains("ECHO: 7"), "{t}");
    assert_eq!(
        r["structuredContent"]["page"],
        json!({"file": "box.scad", "version": 1})
    );
    let r = s.tool("render", json!({}));
    assert!(text(&r).contains("volume 343 mm³"), "{r}");
    // A path or source still wins.
    assert!(!text(&s.tool("evaluate", json!({"source": "cube(1);"}))).contains("web page"));

    // editor_edit: a stale version is refused; old/new and at (byte
    // columns, after a two-byte "ø") land where they should.
    let r = s.tool(
        "editor_edit",
        json!({"version": 9, "edits": [{"old": "cube", "new": "sphere"}]}),
    );
    assert_eq!(r["isError"], true);
    assert!(text(&r).contains("changed since version 9"), "{r}");
    let r = s.tool(
        "editor_edit",
        json!({"version": 1, "edits": [{"old": "size", "new": "s"}]}),
    );
    assert!(text(&r).contains("occurs 3 times"), "{r}");
    let r = s.tool(
        "editor_edit",
        json!({"version": 1, "edits": [{"old": "cube(size);", "new": "sphere(size);"},
                                       {"at": [2, 6, 2, 7], "new": "y"}]}),
    );
    assert_eq!(r["isError"], false, "{r}");
    assert!(
        text(&r).contains("applied 2 edits to the web page's box.scad (line 2, 4); now version 2"),
        "{r}"
    );
    let edit = seen
        .lock()
        .unwrap()
        .requests
        .iter()
        .rfind(|q| q["method"] == "edit")
        .cloned()
        .unwrap();
    assert_eq!(edit["params"]["edits"][0]["from"], json!([1, 4]), "{edit}");
    let t = text(&s.tool("editor_read", json!({})));
    assert!(
        t.contains("     2\t// øy\n") && t.contains("     4\tsphere(size);\n"),
        "{t}"
    );
    let r = s.tool(
        "editor_edit",
        json!({"version": 2, "edits": [{"at": [2, 5, 2, 6], "new": ""}]}),
    );
    assert!(text(&r).contains("inside a character"), "{r}");
    let r = s.tool("editor_edit", json!({"version": 2, "edits": [{"old": "sphere", "new": "a"}, {"old": "sphere(size)", "new": "b"}]}));
    assert!(text(&r).contains("overlap"), "{r}");
    let r = s.tool("editor_edit", json!({"edits": []}));
    assert!(text(&r).contains("needs `version`"), "{r}");

    // format rewrites the page as one edit.
    let r = s.tool("format", json!({}));
    assert!(
        text(&r).contains("already formatted")
            || text(&r).contains("reformatted the web page's box.scad"),
        "{r}"
    );

    // editor_reveal, the camera, a capture, marks and the console.
    assert!(text(&s.tool("editor_reveal", json!({"at": [4]}))).contains("showing 4:1-4:14"));
    assert!(text(&s.tool("editor_reveal", json!({"text": "echo"}))).contains("showing 3:1-3:5"));
    let r = s.tool("view_camera", json!({"vpd": 80, "view": "iso"}));
    assert!(text(&r).contains("$vpd = 80"), "{r}");
    let cam = seen
        .lock()
        .unwrap()
        .requests
        .iter()
        .rfind(|q| q["method"] == "camera")
        .cloned()
        .unwrap();
    assert_eq!(cam["params"]["view"], "diagonal");
    assert!(s.tool("view_camera", json!({"view": "sideways"}))["isError"] == true);
    let r = s.tool("view_capture", json!({}));
    assert_eq!(r["content"][1]["type"], "image", "{r}");
    assert_eq!(r["content"][1]["data"], PNG);
    assert!(text(&r).contains("1x1 (test)"), "{r}");
    let r = s.tool("view_annotate", json!({"markers": [{"point": [1, 2, 3], "label": "here"}], "lines": [{"points": [[0, 0, 0], [5, 5, 5]], "color": "#00ff00"}]}));
    assert!(text(&r).contains("showing 1 marker and 1 line"), "{r}");
    let marks = seen
        .lock()
        .unwrap()
        .requests
        .iter()
        .rfind(|q| q["method"] == "annotate")
        .cloned()
        .unwrap();
    assert_eq!(marks["params"]["markers"][0]["color"], "#ff5a8a");
    assert!(s.tool("view_annotate", json!({"markers": [{"point": [1, 2]}]}))["isError"] == true);
    assert!(text(&s.tool("view_annotate", json!({}))).contains("cleared"));
    assert!(text(&s.tool("console_read", json!({}))).contains("echo: ECHO: 7"));
}

#[test]
fn a_new_tab_replaces_the_old_one() {
    let dir = scratch("replace");
    let mut s = Mcp::start(&dir, &["--browser", "--browser-url", PAGE]);
    let (port, token) = s.link();
    let first = fake_tab(port, &token, "cube(1);\n");
    let second = fake_tab(port, &token, "cube(2);\n");
    std::thread::sleep(Duration::from_millis(200));
    assert_eq!(first.lock().unwrap().closed, Some(4001));
    assert!(text(&s.tool("editor_read", json!({}))).contains("cube(2);"));
    assert!(second.lock().unwrap().closed.is_none());
}

#[test]
fn without_browser_the_page_tools_are_not_listed() {
    let dir = scratch("plain");
    let mut s = Mcp::start(&dir, &[]);
    let r = s.call("tools/list", json!({}));
    let names: Vec<&str> = r["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(!names.contains(&"editor_read"));
    let r = s.call(
        "tools/call",
        json!({"name": "editor_read", "arguments": {}}),
    );
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap()
            .contains("Unknown tool"),
        "{r}"
    );
    let mut b = Mcp::start(&dir, &["--browser"]);
    let r = b.call("tools/list", json!({}));
    let tools = r["result"]["tools"].as_array().unwrap();
    // The model tools, `format` (which formats the page's text; opt-in
    // without `--browser`) and the eight page tools; not `test`.
    assert_eq!(tools.len(), 15);
    let r = b.call("initialize", json!({"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "test"}}));
    let instructions = r["result"]["instructions"].as_str().unwrap();
    assert!(instructions.contains("browser_connect"));
    // The page's paragraph leaves no room for the recipes under the
    // 2,048 characters Claude Code keeps, so they are a pointer to the
    // resource instead of text the client would cut mid-recipe.
    assert!(
        instructions.encode_utf16().count() <= 2048,
        "{instructions}"
    );
    assert!(
        instructions
            .ends_with("are the resource neoscad://recipes, and `docs` gives each by name."),
        "{instructions}"
    );
    // The default page is neoscad.org's.
    assert!(
        text(&b.tool("browser_connect", json!({}))).contains("https://neoscad.org/try/#connect=")
    );
}
