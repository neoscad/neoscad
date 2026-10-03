//! Plain `neoscad mcp` with a running NeoSCAD app (docs/agent-bridge.md,
//! "Desktop apps"): a test app made of the apps' own Rust (`agent_link`'s
//! listener over `client::agent`), with documents held in memory, driven
//! by the real server over MCP stdio and the real per-user socket (a named
//! pipe on Windows). Each test has a scratch rendezvous directory
//! (`NEOSCAD_AGENT_DIR`), so no real app is ever involved.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agent_link::discovery::Rendezvous;
use agent_link::{AgentLink, LinkConfig};
use lang::source::SourceFile;
use neoscad_client::agent::{
    AgentCamera, AgentCameraChange, AgentCapture, AgentDocumentState, AgentEditOutcome,
    AgentEditRequest, AgentHost, AgentRunStatus, EditorPosition,
};
use neoscad_client::{ViewLine, ViewMarker};
use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_neoscad");

/// A scratch directory with `work/` (the server's working directory) and
/// `models/` (where the app's documents are saved, outside every root).
/// Short: the rendezvous inside it holds a socket, whose path is limited
/// to about 100 bytes.
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nsapp{}{name}", std::process::id() % 100_000));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(d.join("work")).unwrap();
    std::fs::create_dir_all(d.join("models")).unwrap();
    lang::paths::plain(d.canonicalize().unwrap())
}

#[derive(Debug, Clone)]
struct Doc {
    id: u64,
    file: String,
    path: Option<String>,
    text: String,
    version: u64,
}

/// The test app: its documents' text and versions, and what the agent did.
#[derive(Default)]
struct App {
    docs: Mutex<Vec<Doc>>,
    log: Mutex<Vec<String>>,
}

impl App {
    fn doc(&self, id: u64) -> Result<Doc, String> {
        self.docs
            .lock()
            .unwrap()
            .iter()
            .find(|d| d.id == id)
            .cloned()
            .ok_or_else(|| format!("document {id} was closed"))
    }

    fn text(&self, id: u64) -> String {
        self.doc(id).unwrap().text
    }
}

impl AgentHost for App {
    fn read(&self, document: u64) -> Result<AgentDocumentState, String> {
        let d = self.doc(document)?;
        Ok(AgentDocumentState {
            version: d.version,
            text: d.text,
            selection: None,
            overrides: Vec::new(),
            parts: false,
            run: AgentRunStatus {
                mode: None,
                summary: "Previewed".into(),
                running: false,
            },
            console: Vec::new(),
        })
    }

    fn edit(&self, document: u64, edit: AgentEditRequest) -> Result<AgentEditOutcome, String> {
        let mut docs = self.docs.lock().unwrap();
        let d = docs
            .iter_mut()
            .find(|d| d.id == document)
            .ok_or("the document was closed")?;
        if d.version != edit.version {
            return Ok(AgentEditOutcome::Stale { version: d.version });
        }
        // As the editor applies them: positions in the text the agent read,
        // so from the last to the first.
        let sf = SourceFile::new(PathBuf::from("doc.scad"), d.text.as_bytes().to_vec());
        let mut text = d.text.clone();
        for e in edit.edits.iter().rev() {
            let from = sf.offset_at_utf16(e.from.line, e.from.character) as usize;
            let to = sf.offset_at_utf16(e.to.line, e.to.character) as usize;
            text.replace_range(from..to, &e.insert);
        }
        d.text = text;
        d.version += 1;
        self.log.lock().unwrap().push(format!(
            "edit by {}: {}",
            edit.client.as_deref().unwrap_or("?"),
            edit.summary
        ));
        Ok(AgentEditOutcome::Applied { version: d.version })
    }

    fn reveal(&self, document: u64, from: EditorPosition, _: EditorPosition) -> Result<(), String> {
        self.log.lock().unwrap().push(format!(
            "reveal {document} {}:{}",
            from.line, from.character
        ));
        Ok(())
    }

    fn camera(&self, _: u64, change: AgentCameraChange) -> Result<AgentCamera, String> {
        Ok(AgentCamera {
            vpt: vec![0.0; 3],
            vpr: vec![55.0, 0.0, 25.0],
            vpd: change.vpd.unwrap_or(140.0),
            vpf: 22.5,
        })
    }

    fn capture(&self, document: u64, max_side: u32) -> Result<AgentCapture, String> {
        let mut png = Vec::new();
        {
            let mut e = png::Encoder::new(&mut png, 2, 1);
            e.set_color(png::ColorType::Rgba);
            e.set_depth(png::BitDepth::Eight);
            let mut w = e.write_header().unwrap();
            w.write_image_data(&[255, 0, 0, 255, 0, 0, 255, 255])
                .unwrap();
        }
        self.log
            .lock()
            .unwrap()
            .push(format!("capture {document} {max_side}"));
        Ok(AgentCapture {
            png,
            width: 2,
            height: 1,
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
        _: Vec<ViewLine>,
        markers: Vec<ViewMarker>,
    ) -> Result<(), String> {
        self.log
            .lock()
            .unwrap()
            .push(format!("annotate {document} {}", markers.len()));
        Ok(())
    }
}

/// A running test app: its documents and its link.
struct Running {
    app: Arc<App>,
    link: AgentLink,
}

fn start_app(rendezvous: &Path, docs: &[Doc]) -> Running {
    let app = Arc::new(App::default());
    *app.docs.lock().unwrap() = docs.to_vec();
    let link = AgentLink::new(
        app.clone(),
        LinkConfig {
            app: "NeoSCAD".into(),
            version: "test".into(),
            platform: "test".into(),
            rendezvous: Some(Rendezvous::at(rendezvous)),
        },
    );
    for d in docs {
        link.document_opened(d.id, &d.file, d.path.as_deref());
        link.document_focused(d.id);
    }
    link.set_allowed(true);
    link.start().unwrap();
    Running { app, link }
}

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
    /// `notifications/tools/list_changed` seen so far.
    list_changed: usize,
    /// What `initialize` said.
    instructions: String,
}

impl Mcp {
    fn start(dir: &Path, rendezvous: &Path) -> Mcp {
        Mcp::start_in(&dir.join("work"), rendezvous, &[])
    }

    fn start_in(cwd: &Path, rendezvous: &Path, env: &[(&str, &Path)]) -> Mcp {
        let mut cmd = Command::new(BIN);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd
            .arg("mcp")
            .current_dir(cwd)
            .env("NEOSCAD_AGENT_DIR", rendezvous)
            .env_remove("OPENSCADPATH")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut m = Mcp {
            stdin: child.stdin.take().unwrap(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
            next: 1,
            list_changed: 0,
            instructions: String::new(),
        };
        let init = m.call(
            "initialize",
            json!({"protocolVersion": "2025-11-25", "capabilities": {},
                   "clientInfo": {"name": "test-client", "title": "Test Client", "version": "1"}}),
        );
        assert!(
            init["result"]["capabilities"]["tools"]["listChanged"] == true,
            "{init}"
        );
        m.instructions = init["result"]["instructions"]
            .as_str()
            .unwrap_or("")
            .to_string();
        m
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
        loop {
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "server closed"
            );
            let r: Value = serde_json::from_str(&line).unwrap();
            if r["method"] == "notifications/tools/list_changed" {
                self.list_changed += 1;
                continue;
            }
            assert_eq!(r["id"], id, "{r}");
            return r;
        }
    }

    fn tool(&mut self, name: &str, args: Value) -> Value {
        self.call("tools/call", json!({"name": name, "arguments": args}))["result"].clone()
    }

    fn names(&mut self) -> Vec<String> {
        self.call("tools/list", json!({}))["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
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

fn gear(dir: &Path) -> Doc {
    let path = dir.join("models").join("gear.scad");
    std::fs::write(dir.join("models").join("lib.scad"), "size = 4;\n").unwrap();
    let text = "include <lib.scad>\ncube(size);\n".to_string();
    std::fs::write(&path, &text).unwrap();
    Doc {
        id: 7,
        file: "gear.scad".into(),
        path: Some(path.to_string_lossy().into_owned()),
        text,
        version: 1,
    }
}

#[test]
fn the_agent_reads_edits_and_checks_the_apps_document() {
    let dir = scratch("rw");
    let rv = dir.join("rv");
    let doc = gear(&dir);
    let app = start_app(&rv, std::slice::from_ref(&doc));
    let mut m = Mcp::start(&dir, &rv);
    // The app is found before the first answer: its paragraph, its tools
    // (and `format`), and no browser_connect.
    assert!(
        m.instructions.contains("NeoSCAD app open"),
        "{}",
        m.instructions
    );
    assert!(m.instructions.encode_utf16().count() <= 2048);
    let names = m.names();
    for t in [
        "editor_read",
        "editor_edit",
        "view_capture",
        "console_read",
        "format",
    ] {
        assert!(names.contains(&t.to_string()), "{names:?}");
    }
    assert!(!names.contains(&"browser_connect".to_string()));

    // The app is told who connected.
    let deadline = Instant::now() + Duration::from_secs(10);
    while app
        .link
        .status()
        .clients
        .first()
        .and_then(|c| c.name.clone())
        != Some("Test Client".into())
    {
        assert!(Instant::now() < deadline, "{:?}", app.link.status());
        std::thread::sleep(Duration::from_millis(20));
    }

    let start = Instant::now();
    let r = m.tool("editor_read", json!({}));
    let latency = start.elapsed();
    eprintln!("editor_read through neoscad mcp and the app: {latency:?}");
    let t = text(&r);
    assert!(
        t.starts_with("gear.scad in NeoSCAD (document 1), version 1, 3 lines, saved as "),
        "{t}"
    );
    assert!(t.contains("     2\tcube(size);"), "{t}");

    // An edit: one step in the app, the version moves on.
    let r = m.tool(
        "editor_edit",
        json!({"version": 1, "edits": [{"old": "cube(size);", "new": "cube(size * 2);"}]}),
    );
    assert!(
        text(&r).contains(
            "applied 1 edit to gear.scad in NeoSCAD (document 1) (line 2); now version 2"
        ),
        "{r}"
    );
    assert_eq!(r["structuredContent"]["document"], 1);
    assert_eq!(app.app.text(7), "include <lib.scad>\ncube(size * 2);\n");
    assert_eq!(
        app.app.log.lock().unwrap().last().unwrap(),
        "edit by Test Client: line 2"
    );
    // The old version is stale now.
    let r = m.tool(
        "editor_edit",
        json!({"version": 1, "edits": [{"old": "size", "new": "s"}]}),
    );
    assert_eq!(r["isError"], true);
    assert!(
        text(&r).contains("the document's text changed since version 1"),
        "{r}"
    );

    // The model tools run the unsaved text under its real path: the
    // include beside it resolves, though `models/` is no root.
    let r = m.tool("check", json!({}));
    let t = text(&r);
    assert!(
        t.starts_with("gear.scad in NeoSCAD (document 1, version 2)\n"),
        "{t}"
    );
    assert_eq!(r["structuredContent"]["document"]["version"], 2);
    assert_eq!(
        r["structuredContent"]["model"]["bbox"]["size"],
        json!([8.0, 8.0, 8.0]),
        "the edited size: {r}"
    );
    // Readable, not writable: an export beside it is refused.
    let r = m.tool(
        "render",
        json!({"path": dir.join("models/gear.scad"), "export": dir.join("models/out.stl")}),
    );
    assert_eq!(r["isError"], true, "{r}");

    // The view tools go to the app.
    let r = m.tool("view_capture", json!({"size": 300}));
    assert_eq!(r["content"][1]["type"], "image", "{r}");
    m.tool(
        "view_annotate",
        json!({"markers": [{"point": [0, 0, 0], "label": "here"}]}),
    );
    m.tool("editor_reveal", json!({"at": [2]}));
    let log = app.app.log.lock().unwrap().clone();
    assert!(log.contains(&"capture 7 300".to_string()), "{log:?}");
    assert!(log.contains(&"annotate 7 1".to_string()), "{log:?}");
    assert!(log.contains(&"reveal 7 1:0".to_string()), "{log:?}");
    drop(m);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Started where no folder is a root (Claude Desktop's `/`, or the home
/// folder), the server still runs the app's document under its own path:
/// its folder is readable while it is open, never writable.
#[test]
fn with_no_roots_the_apps_document_still_runs() {
    let dir = scratch("noroot");
    let rv = dir.join("rv");
    let doc = gear(&dir);
    let app = start_app(&rv, std::slice::from_ref(&doc));
    let home_env = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    let mut m = Mcp::start_in(&dir, &rv, &[(home_env, &dir)]);
    let r = m.tool("check", json!({}));
    assert_eq!(r["isError"], false, "{r}");
    assert_eq!(
        r["structuredContent"]["model"]["bbox"]["size"],
        json!([4.0, 4.0, 4.0]),
        "the include beside the document resolved: {r}"
    );
    let r = m.tool(
        "render",
        json!({"path": doc.path.as_deref().unwrap(), "export": dir.join("models/out.stl")}),
    );
    assert_eq!(r["isError"], true, "{r}");
    assert!(text(&r).contains("there are none"), "{r}");
    // Its file can be read by path while it is open...
    let r = m.tool("evaluate", json!({"path": doc.path.as_deref().unwrap()}));
    assert_eq!(r["isError"], false, "{r}");
    // ... and not once the app has closed it.
    app.link.document_closed(7);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let r = m.tool("evaluate", json!({"path": doc.path.as_deref().unwrap()}));
        if r["isError"] == true {
            assert!(text(&r).contains("outside the allowed roots"), "{r}");
            break;
        }
        assert!(Instant::now() < deadline, "{r}");
        std::thread::sleep(Duration::from_millis(20));
    }
    drop((m, app));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_server_waits_for_the_app_and_reconnects_after_a_restart() {
    let dir = scratch("wait");
    let rv = dir.join("rv");
    let doc = gear(&dir);
    let mut m = Mcp::start(&dir, &rv);
    // No app: nothing of the app's is listed or said, and the model tools
    // need a path or source at once (they do not wait for an app).
    assert!(!m.instructions.contains("NeoSCAD app"));
    assert!(!m.names().contains(&"editor_read".to_string()));
    let start = Instant::now();
    let r = m.tool("check", json!({}));
    assert!(text(&r).contains("give path"), "{r}");
    assert!(start.elapsed() < Duration::from_secs(2));

    // Before any app, its tools are as unknown as unlisted ones.
    let r = m.call(
        "tools/call",
        json!({"name": "editor_read", "arguments": {}}),
    );
    assert!(
        r["error"]["message"]
            .as_str()
            .unwrap_or("")
            .contains("Unknown tool"),
        "{r}"
    );

    // The app starts later: the server finds it by itself, lists its
    // tools and tells the client the list changed.
    let app = start_app(&rv, std::slice::from_ref(&doc));
    let deadline = Instant::now() + Duration::from_secs(10);
    while !m.names().contains(&"editor_read".to_string()) {
        assert!(Instant::now() < deadline, "the app was never found");
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(m.list_changed >= 1);
    assert!(text(&m.tool("editor_read", json!({}))).starts_with("gear.scad in NeoSCAD"));

    // The app quits: its tools go from the list...
    drop(app);
    let deadline = Instant::now() + Duration::from_secs(10);
    while m.names().contains(&"editor_read".to_string()) {
        assert!(Instant::now() < deadline, "the app's tools stayed listed");
        std::thread::sleep(Duration::from_millis(50));
    }
    // ... and comes back (a new process, a new socket) while the agent
    // asks: the call waits for it.
    let rv2 = rv.clone();
    let docs = vec![doc];
    let starter = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(700));
        start_app(&rv2, &docs)
    });
    let start = Instant::now();
    let r = m.tool("editor_read", json!({}));
    assert!(text(&r).starts_with("gear.scad in NeoSCAD"), "{r}");
    assert!(start.elapsed() >= Duration::from_millis(600));
    let app = starter.join().unwrap();
    assert!(m.names().contains(&"editor_read".to_string()));
    drop(app);
    drop(m);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn consent_off_and_disconnect_end_the_connection() {
    let dir = scratch("off");
    let rv = dir.join("rv");
    let doc = gear(&dir);
    let app = start_app(&rv, std::slice::from_ref(&doc));
    let mut m = Mcp::start(&dir, &rv);
    assert!(text(&m.tool("editor_read", json!({}))).starts_with("gear.scad"));

    // "Disconnect": the server is asked not to reconnect by itself, so the
    // next call waits for an app and then says how to connect.
    let id = app.link.status().clients[0].id;
    app.link.disconnect(id);
    // Until the server has read the app's `bye`, a call still goes out on
    // the closing connection and is answered "disconnected before it
    // answered" (what an agent sees if the user disconnects it mid-call).
    let deadline = Instant::now() + Duration::from_secs(10);
    while m.names().contains(&"editor_read".to_string()) {
        assert!(Instant::now() < deadline, "the app's tools stayed listed");
        std::thread::sleep(Duration::from_millis(50));
    }
    let start = Instant::now();
    let r = m.tool("editor_read", json!({}));
    assert_eq!(r["isError"], true, "{r}");
    assert!(text(&r).contains("no NeoSCAD app is connected"), "{r}");
    assert!(
        start.elapsed() >= Duration::from_secs(4),
        "it waited for the app"
    );
    assert!(app.link.status().clients.is_empty());

    // A new session connects again; turning agents off ends it, and no
    // socket is left to find.
    let mut m2 = Mcp::start(&dir, &rv);
    assert!(text(&m2.tool("editor_read", json!({}))).starts_with("gear.scad"));
    app.link.set_allowed(false);
    assert!(Rendezvous::at(&rv).scan().is_empty());
    assert!(app.link.start().is_err());
    let r = m2.tool("editor_read", json!({}));
    assert!(text(&r).contains("no NeoSCAD app is connected"), "{r}");
    drop((m, m2, app));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn two_documents_the_focused_one_by_default() {
    let dir = scratch("two");
    let rv = dir.join("rv");
    let first = gear(&dir);
    let second = Doc {
        id: 9,
        file: "Untitled".into(),
        path: None,
        text: "sphere(3);\n".into(),
        version: 5,
    };
    // Opened in this order, so the second is focused last.
    let app = start_app(&rv, &[first, second]);
    let mut m = Mcp::start(&dir, &rv);
    let t = text(&m.tool("editor_read", json!({})));
    assert!(
        t.starts_with("Untitled in NeoSCAD (document 2), version 5"),
        "{t}"
    );
    assert!(t.contains("not saved yet"), "{t}");
    assert!(t.contains("also open (pass document): 1 gear.scad"), "{t}");
    let t = text(&m.tool("editor_read", json!({"document": 1})));
    assert!(t.starts_with("gear.scad in NeoSCAD (document 1)"), "{t}");
    // An edit goes to the document asked for, not the focused one.
    let r = m.tool(
        "editor_edit",
        json!({"document": 1, "version": 1, "edits": [{"old": "cube", "new": "cylinder"}]}),
    );
    assert_eq!(r["isError"], false, "{r}");
    assert!(app.app.text(7).contains("cylinder(size)"));
    assert_eq!(app.app.text(9), "sphere(3);\n");
    // The user focuses the first window: it becomes the default.
    app.link.document_focused(7);
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let t = text(&m.tool("editor_read", json!({})));
        if t.starts_with("gear.scad") {
            break;
        }
        assert!(Instant::now() < deadline, "{t}");
        std::thread::sleep(Duration::from_millis(20));
    }
    // An unsaved document's model runs under a plain name in the working
    // directory.
    let r = m.tool("evaluate", json!({"document": 2}));
    assert_eq!(r["isError"], true, "evaluate takes no document: {r}");
    let r = m.tool("editor_read", json!({"document": 5}));
    assert!(text(&r).contains("document 5 is not open"), "{r}");
    drop(m);
    drop(app);
    let _ = std::fs::remove_dir_all(&dir);
}
