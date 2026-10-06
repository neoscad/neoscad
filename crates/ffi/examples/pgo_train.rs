//! The training driver of the app core's profile-guided build
//! (`scripts/pgo.sh --ffi`, docs/release.md, "PGO builds").
//!
//! The macOS app links `neoscad-ffi` as a static library, which cannot be
//! run to train a profile. This example links the very same library
//! build: an example of the crate compiles against the `lib` unit that
//! `cargo build -p neoscad-ffi --lib` makes (building it leaves `--lib`
//! fresh), so its functions have the same symbol names, which is what a
//! profile is matched by. A profile trained through `neoscad` instead is
//! keyed by the CLI's crate graph, where every shared crate gets another
//! metadata hash and so other symbol names: under it, most of the core's
//! functions find no profile data and the core runs no faster.
//!
//! It reads jobs from standard input, one per line, tab-separated, and
//! runs them in order through the core's exported API, the calls the
//! app's windows make:
//!
//! ```text
//! app     INPUT [OUTPUT]  preview, then render, through `run_document`
//!                         with a viewport and the editor's language
//!                         server; then `export` to OUTPUT if given
//! preview INPUT           preview only (an echo-only test file)
//! render  INPUT           render only
//! export  INPUT OUTPUT    `export` (the format from OUTPUT's extension)
//! snapshot INPUT          `snapshot`, the default sheet
//! check   INPUT           `check` with the default options
//! measure INPUT           `measure`
//! edit    INPUT           an edit loop: eight updates of the text, each
//!                         previewed with the language server told, every
//!                         other one also snapshotted
//! ```
//!
//! Each request runs under fixed limits (2 GiB of measured process memory,
//! 120 s, the training's own `--limit`s), so a run of the training cannot
//! take the machine's memory. A failed request is counted, not fatal: a
//! model that stops early still exercises the core on the way, as in
//! `scripts/pgo-train.py`. Prints `ok N failed M` at the end.
//!
//! With `--time`, it also prints each job's wall time first
//! (`MS<tab>KIND<tab>INPUT`), not counting the core's and the GPU's
//! start: how the core's plain and PGO builds were compared
//! (docs/release.md, "PGO builds"). A job per process then measures a
//! cold session, as the command line's bench does.
//!
//! Without a GPU (`Viewport::new` fails), runs go without a viewport,
//! which skips only building and uploading the preview's scene.

use std::io::BufRead;
use std::sync::Arc;

use neoscad_ffi::{
    Core, CoreConfig, CoreError, DocumentRequest, LanguageServer, RenderMode, ResourceLimits,
    RunOptions, SnapshotOptions, Viewport, default_check_options,
};
use serde_json::json;

fn main() {
    let time = std::env::args().skip(1).any(|a| a == "--time");
    let core = Core::new(CoreConfig::default()).expect("core");
    core.set_limits(ResourceLimits {
        time_seconds: Some(120.0),
        memory_bytes: Some(2 << 30),
        ..core.limits().expect("limits")
    })
    .expect("limits");
    let viewport = Viewport::new("Cornfield".into())
        .inspect_err(|e| eprintln!("pgo_train: no viewport ({e}); previews build no scene"))
        .ok();
    let language = core.clone().language_server(true).expect("language server");
    lsp(
        &language,
        json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": {}}),
    );
    let mut t = Trainer {
        core,
        viewport,
        language,
        ok: 0,
        failed: 0,
        next_id: 1,
    };
    for line in std::io::stdin().lock().lines() {
        let line = line.expect("stdin");
        let mut f = line.split('\t');
        let (Some(kind), Some(input)) = (f.next(), f.next()) else {
            continue;
        };
        let output = f.next().filter(|o| !o.is_empty());
        let start = std::time::Instant::now();
        let r = t.job(kind, input, output);
        if time {
            let ms = start.elapsed().as_secs_f64() * 1000.0;
            println!("{ms:.3}\t{kind}\t{input}");
        }
        t.count(r);
        // Each document is forgotten afterwards, as a closed window's is,
        // so its buffer and products do not pile up over a thousand jobs.
        let _ = t.core.close(input.into());
    }
    println!("ok {} failed {}", t.ok, t.failed);
}

struct Trainer {
    core: Arc<Core>,
    viewport: Option<Arc<Viewport>>,
    language: Arc<LanguageServer>,
    ok: u32,
    failed: u32,
    next_id: u64,
}

/// A JSON-RPC message to the language server; its replies are dropped.
fn lsp(ls: &LanguageServer, message: serde_json::Value) {
    let _ = ls.handle(message.to_string());
}

fn uri(path: &str) -> String {
    format!("file://{path}")
}

impl Trainer {
    fn count<T>(&mut self, r: Result<T, CoreError>) {
        match r {
            Ok(_) => self.ok += 1,
            Err(_) => self.failed += 1,
        }
    }

    fn run(&self, path: &str, mode: RenderMode) -> Result<(), CoreError> {
        let request = DocumentRequest {
            mode,
            overrides: Vec::new(),
            parts: false,
            enable: Vec::new(),
        };
        self.core
            .run_document(
                path.into(),
                request,
                self.viewport.clone(),
                Some(self.language.clone()),
                None,
            )
            .map(|_| ())
    }

    /// The editor opening the document's text, then asking for what it
    /// shows beside it (the outline, the folds, a hover).
    fn open_in_editor(&mut self, path: &str, text: &str) {
        let doc = json!({"uri": uri(path)});
        lsp(
            &self.language,
            json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
                "textDocument": {"uri": uri(path), "languageId": "openscad", "version": 1, "text": text}}}),
        );
        for (method, extra) in [
            ("textDocument/documentSymbol", json!({})),
            ("textDocument/foldingRange", json!({})),
            (
                "textDocument/hover",
                json!({"position": {"line": 0, "character": 2}}),
            ),
        ] {
            let mut params = json!({"textDocument": doc});
            if let (Some(p), Some(e)) = (params.as_object_mut(), extra.as_object()) {
                p.extend(e.clone());
            }
            self.next_id += 1;
            lsp(
                &self.language,
                json!({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params}),
            );
        }
    }

    fn close_in_editor(&self, path: &str) {
        lsp(
            &self.language,
            json!({"jsonrpc": "2.0", "method": "textDocument/didClose",
                   "params": {"textDocument": {"uri": uri(path)}}}),
        );
    }

    fn job(&mut self, kind: &str, input: &str, output: Option<&str>) -> Result<(), CoreError> {
        match kind {
            "render" => {
                self.core.open(input.into(), None)?;
                self.run(input, RenderMode::Render)
            }
            "app" | "preview" => {
                let text = self.core.read_file(input.into())?;
                self.core.open(input.into(), Some(text.clone()))?;
                self.open_in_editor(input, &text);
                let r = self.run(input, RenderMode::Preview).and_then(|()| {
                    if kind == "app" {
                        self.run(input, RenderMode::Render)?;
                        if let Some(out) = output {
                            self.core.export(input.into(), out.into(), None)?;
                        }
                    }
                    Ok(())
                });
                self.close_in_editor(input);
                r
            }
            "export" => {
                let out = output.ok_or(CoreError::InvalidArgument {
                    message: "export needs an output".into(),
                })?;
                self.core.export(input.into(), out.into(), None).map(|_| ())
            }
            "snapshot" => self
                .core
                .snapshot(
                    input.into(),
                    SnapshotOptions {
                        width: 1024,
                        height: 1024,
                        views: Vec::new(),
                        dims: false,
                        preview: false,
                    },
                )
                .map(|_| ()),
            "check" => self
                .core
                .check(input.into(), default_check_options()?, run_options(), None)
                .map(|_| ()),
            "measure" => self
                .core
                .measure(input.into(), run_options(), None)
                .map(|_| ()),
            "edit" => self.edit(input),
            _ => Err(CoreError::InvalidArgument {
                message: format!("unknown job kind '{kind}'"),
            }),
        }
    }

    /// The window's edit loop: each change goes to the session and the
    /// editor, then is previewed (the app previews after each pause in
    /// typing); every other one is also snapshotted, as an agent checking
    /// its edit would. The edit changes the first `12]` to 13..20, so
    /// the geometry changes too (the training's model has one).
    fn edit(&mut self, input: &str) -> Result<(), CoreError> {
        let text = self.core.read_file(input.into())?;
        self.core.open(input.into(), Some(text.clone()))?;
        self.open_in_editor(input, &text);
        for k in 0..8u32 {
            let changed = text.replacen("12]", &format!("{}]", 13 + k), 1);
            self.core.update(input.into(), changed.clone())?;
            lsp(
                &self.language,
                json!({"jsonrpc": "2.0", "method": "textDocument/didChange", "params": {
                    "textDocument": {"uri": uri(input), "version": 2 + k},
                    "contentChanges": [{"text": changed}]}}),
            );
            let r = self.run(input, RenderMode::Preview);
            self.count(r);
            if k % 2 == 1 {
                let r = self.job("snapshot", input, None);
                self.count(r);
            }
        }
        self.close_in_editor(input);
        Ok(())
    }
}

fn run_options() -> RunOptions {
    RunOptions {
        overrides: Vec::new(),
        parts: false,
        enable: Vec::new(),
    }
}
