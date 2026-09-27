//! A client for the tests: a session over in-memory files (or the disk,
//! for BOSL2), a server, and requests by method name.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lang::loader::{FileSystem, LibraryPath};
use lang::vfs::MemFs;
use serde_json::{Value, json};
use session::{Config, Session};

pub struct Client {
    pub session: Session,
    pub server: lsp::Server,
    next: u64,
    /// Notifications the server sent, in order.
    pub notes: Vec<Value>,
}

pub fn uri(path: &str) -> String {
    lsp::uri::from_path(Path::new(path))
}

impl Client {
    /// Over in-memory `files`, with `/lib` as the library directory.
    pub fn mem(files: &[(&str, &str)]) -> Client {
        let fs = Arc::new(MemFs::new());
        for (p, t) in files {
            fs.insert(p, t.as_bytes().to_vec());
        }
        Client::over(fs, LibraryPath(vec![PathBuf::from("/lib")]))
    }

    pub fn over(fs: Arc<dyn FileSystem + Send + Sync>, libs: LibraryPath) -> Client {
        Client::with(fs, libs, false)
    }

    /// A server whose host supplies the diagnostics
    /// (`Options::host_diagnostics`), as the app's does.
    pub fn host_run(files: &[(&str, &str)]) -> Client {
        let fs = Arc::new(MemFs::new());
        for (p, t) in files {
            fs.insert(p, t.as_bytes().to_vec());
        }
        Client::with(fs, LibraryPath(vec![PathBuf::from("/lib")]), true)
    }

    fn with(
        fs: Arc<dyn FileSystem + Send + Sync>,
        libs: LibraryPath,
        host_diagnostics: bool,
    ) -> Client {
        let session = Session::new(Config::new(fs, libs));
        let server = lsp::Server::new(lsp::Options {
            sync_session: true,
            limits: None,
            host_diagnostics,
        });
        let mut c = Client {
            session,
            server,
            next: 0,
            notes: Vec::new(),
        };
        let r = c.request("initialize", json!({"capabilities": {}}));
        assert_eq!(r["capabilities"]["positionEncoding"], "utf-16");
        c.notify("initialized", json!({}));
        c
    }

    fn send(&mut self, msg: Value) -> Vec<Value> {
        self.server
            .handle(&self.session, &msg.to_string())
            .iter()
            .map(|m| serde_json::from_str(m).unwrap())
            .collect()
    }

    /// The whole response of a request.
    pub fn call(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        let out =
            self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        assert_eq!(out.len(), 1, "{out:?}");
        assert_eq!(out[0]["id"], id);
        out[0].clone()
    }

    /// A request's result (panics on an error).
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let r = self.call(method, params);
        assert!(r.get("error").is_none(), "{method}: {r}");
        r["result"].clone()
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        let out = self.notify_all(method, params);
        assert!(out.is_empty(), "{out:?}");
    }

    /// A notification, and what the server sent back (publications, for
    /// a host-supplied run of the text it brings).
    pub fn notify_all(&mut self, method: &str, params: Value) -> Vec<Value> {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    pub fn open(&mut self, path: &str, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri(path), "languageId": "openscad", "version": 1, "text": text}}),
        );
    }

    /// Publish due diagnostics; the notifications' params.
    pub fn diagnostics(&mut self) -> Vec<Value> {
        let out: Vec<Value> = self
            .server
            .publish_diagnostics(&self.session)
            .iter()
            .map(|m| serde_json::from_str::<Value>(m).unwrap())
            .collect();
        for m in &out {
            assert_eq!(m["method"], "textDocument/publishDiagnostics");
        }
        let params: Vec<Value> = out.iter().map(|m| m["params"].clone()).collect();
        self.notes.extend(out);
        params
    }

    /// The position of the `n`th `|` marker... simpler: of `needle` in
    /// `text` plus `delta` characters (ASCII texts).
    pub fn pos(text: &str, needle: &str, delta: usize) -> Value {
        let at = text
            .find(needle)
            .unwrap_or_else(|| panic!("{needle} not in text"))
            + delta;
        let before = &text[..at];
        let line = before.matches('\n').count();
        let col = before.rsplit('\n').next().unwrap().encode_utf16().count();
        json!({"line": line, "character": col})
    }

    pub fn at(
        &mut self,
        method: &str,
        path: &str,
        text: &str,
        needle: &str,
        delta: usize,
    ) -> Value {
        self.request(
            method,
            json!({"textDocument": {"uri": uri(path)}, "position": Client::pos(text, needle, delta)}),
        )
    }
}

/// `.reference/BOSL2`'s parent, when BOSL2 is checked out there.
pub fn bosl2_root() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.reference");
    let root = std::fs::canonicalize(root).ok()?;
    root.join("BOSL2/std.scad").exists().then_some(root)
}

/// Completion labels.
pub fn labels(result: &Value) -> Vec<String> {
    result["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["label"].as_str().unwrap().to_string())
        .collect()
}
