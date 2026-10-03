//! The app's side of the link over a real socket (a named pipe on
//! Windows), with a stand-in for `neoscad mcp` speaking the protocol by
//! hand: the hello and the documents, a request, consent, Disconnect and
//! stop. `crates/cli/tests/app.rs` drives it with the real `neoscad mcp`.

use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use agent_link::discovery::Rendezvous;
use agent_link::{AgentLink, AgentObserver, LinkConfig, frame, transport};
use client::agent::{
    AgentCamera, AgentCameraChange, AgentCapture, AgentDocumentState, AgentEditOutcome,
    AgentEditRequest, AgentHost, AgentRunStatus, AgentStatus, EditorPosition,
};
use client::{ViewLine, ViewMarker};
use serde_json::{Value, json};

struct App;

impl AgentHost for App {
    fn read(&self, document: u64) -> Result<AgentDocumentState, String> {
        Ok(AgentDocumentState {
            version: 3,
            text: format!("// document {document}\ncube(1);"),
            selection: None,
            overrides: Vec::new(),
            parts: false,
            run: AgentRunStatus {
                mode: None,
                summary: String::new(),
                running: false,
            },
            console: Vec::new(),
        })
    }
    fn edit(&self, _: u64, _: AgentEditRequest) -> Result<AgentEditOutcome, String> {
        Ok(AgentEditOutcome::Applied { version: 4 })
    }
    fn reveal(&self, _: u64, _: EditorPosition, _: EditorPosition) -> Result<(), String> {
        Ok(())
    }
    fn camera(&self, _: u64, _: AgentCameraChange) -> Result<AgentCamera, String> {
        Err("no view".into())
    }
    fn capture(&self, _: u64, _: u32) -> Result<AgentCapture, String> {
        Err("no view".into())
    }
    fn annotate(&self, _: u64, _: Vec<ViewLine>, _: Vec<ViewMarker>) -> Result<(), String> {
        Ok(())
    }
}

#[derive(Default)]
struct Seen(Mutex<Vec<(u64, AgentStatus)>>);

impl AgentObserver for Seen {
    fn status_changed(&self, status: AgentStatus, sequence: u64) {
        self.0.lock().unwrap().push((sequence, status));
    }
}

impl Seen {
    /// The newest status, as an app keeps it.
    fn latest(&self) -> AgentStatus {
        let s = self.0.lock().unwrap();
        s.iter().max_by_key(|(n, _)| *n).unwrap().1.clone()
    }

    fn wait(&self, what: &str, f: impl Fn(&AgentStatus) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !f(&self.latest()) {
            assert!(
                Instant::now() < deadline,
                "never {what}: {:?}",
                self.latest()
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// A short private directory: socket paths are limited to about 100 bytes.
fn scratch(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nal{}{tag}", std::process::id() % 100_000));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn link(dir: &Path) -> (AgentLink, Arc<Seen>) {
    let link = AgentLink::new(
        Arc::new(App),
        LinkConfig {
            app: "NeoSCAD".into(),
            version: "test".into(),
            platform: "test".into(),
            rendezvous: Some(Rendezvous::at(dir)),
        },
    );
    let seen = Arc::new(Seen::default());
    link.set_observer(Some(seen.clone()));
    (link, seen)
}

struct Agent {
    r: BufReader<Box<dyn std::io::Read + Send>>,
    w: Box<dyn std::io::Write + Send>,
}

impl Agent {
    fn connect(dir: &Path) -> Agent {
        let addresses = Rendezvous::at(dir).scan();
        assert_eq!(addresses.len(), 1, "{addresses:?}");
        let c = transport::connect(&addresses[0]).expect("our own socket");
        let mut a = Agent {
            r: BufReader::new(c.reader),
            w: c.writer,
        };
        a.send(json!({"type": "welcome", "client": "Test Agent", "protocol": 1}));
        a
    }

    fn send(&mut self, v: Value) {
        frame::write_message(&mut self.w, &v).unwrap();
    }

    fn next(&mut self) -> Option<Value> {
        frame::read_message(&mut self.r).unwrap_or(None)
    }

    /// The next message of `type`, skipping others.
    fn next_of(&mut self, kind: &str) -> Value {
        loop {
            let m = self.next().expect("the connection ended");
            if m["type"] == kind {
                return m;
            }
        }
    }

    fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({"id": id, "method": method, "params": params}));
        loop {
            let m = self.next().expect("the connection ended");
            if m["id"] == id {
                return m;
            }
        }
    }
}

#[test]
fn nothing_listens_until_the_user_allows_it() {
    let dir = scratch("off");
    let (link, _) = link(&dir);
    assert!(link.start().unwrap_err().contains("not allowed"));
    assert!(!link.status().listening);
    assert!(Rendezvous::at(&dir).scan().is_empty());
    link.set_allowed(true);
    link.start().unwrap();
    assert!(link.status().listening);
    assert_eq!(Rendezvous::at(&dir).scan().len(), 1);
    // Starting again is not an error and does not listen twice.
    link.start().unwrap();
    assert_eq!(Rendezvous::at(&dir).scan().len(), 1);
    // Turning consent off closes the socket at once.
    link.set_allowed(false);
    assert!(!link.status().listening);
    assert!(Rendezvous::at(&dir).scan().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_agent_is_greeted_answered_and_disconnected() {
    let dir = scratch("talk");
    let (link, seen) = link(&dir);
    link.document_opened(1, "gear.scad", Some("/models/gear.scad"));
    link.document_opened(2, "Untitled", None);
    link.document_focused(2);
    link.set_allowed(true);
    link.start().unwrap();

    let mut a = Agent::connect(&dir);
    let hello = a.next_of("hello");
    assert_eq!(hello["app"], "NeoSCAD");
    assert_eq!(hello["protocol"], 1);
    assert_eq!(hello["documents"][0]["id"], 2);
    seen.wait("connected", |s| {
        s.clients.len() == 1 && s.clients[0].name.as_deref() == Some("Test Agent")
    });

    // A request without `document`: the focused one.
    let start = Instant::now();
    let r = a.request(1, "read", json!({}));
    eprintln!("read round trip: {:?}", start.elapsed());
    assert_eq!(r["result"]["document"], 2);
    assert_eq!(r["result"]["text"], "// document 2\ncube(1);");
    let r = a.request(2, "read", json!({"document": 1}));
    assert_eq!(r["result"]["path"], "/models/gear.scad");
    let r = a.request(3, "capture", json!({}));
    assert_eq!(r["error"]["message"], "no view");
    let r = a.request(
        4,
        "edit",
        json!({"version": 3, "edits": [{"from": [0, 0], "to": [0, 2], "insert": "#"}]}),
    );
    assert_eq!(r["result"]["version"], 4);

    // The app's documents change: every agent is told.
    link.document_focused(1);
    let note = a.next_of("documents");
    assert_eq!(note["documents"][0]["id"], 1);
    link.document_closed(2);
    assert_eq!(
        a.next_of("documents")["documents"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // The bridge's own tool shows as the agent's activity.
    a.send(json!({"type": "activity", "tool": "check", "document": 1}));
    seen.wait("checking", |s| {
        s.clients
            .first()
            .is_some_and(|c| c.activity.as_deref() == Some("is checking the model"))
    });
    a.send(json!({"type": "activity", "tool": null}));
    seen.wait("idle", |s| {
        s.clients.first().is_some_and(|c| c.activity.is_none())
    });

    // Disconnect: a `bye` asking it not to come back, then the end.
    let id = link.status().clients[0].id;
    link.disconnect(id);
    let bye = a.next_of("bye");
    assert_eq!(bye["reconnect"], false);
    assert!(a.next().is_none());
    seen.wait("disconnected", |s| s.clients.is_empty());

    // Stop: every agent told, the socket gone.
    let mut b = Agent::connect(&dir);
    b.next_of("hello");
    link.stop();
    assert_eq!(b.next_of("bye")["reconnect"], true);
    assert!(Rendezvous::at(&dir).scan().is_empty());
    assert!(!link.status().listening);
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn a_stale_socket_is_swept_and_a_live_one_kept() {
    let dir = scratch("sweep");
    let (first, _) = link(&dir);
    first.set_allowed(true);
    first.start().unwrap();
    // A socket left by an app that crashed: a file nothing listens at.
    let stale = dir.join("app-00000000deadbeef.sock");
    drop(std::os::unix::net::UnixListener::bind(&stale).unwrap());
    assert_eq!(Rendezvous::at(&dir).scan().len(), 2);
    let (second, _) = link(&dir);
    second.set_allowed(true);
    second.start().unwrap();
    let left = Rendezvous::at(&dir).scan();
    assert_eq!(left.len(), 2, "{left:?}");
    assert!(!left.contains(&stale));
    drop(first);
    drop(second);
    assert!(Rendezvous::at(&dir).scan().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}
