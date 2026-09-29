//! The tools of `neoscad mcp` and how their results read.
//!
//! Every description, schema and default here is paid for in context by
//! every agent session that loads the server, and every result by every
//! call, so both are terse: a result is a short text summary with the
//! numbers an agent acts on, plus the same facts as `structuredContent`.
//! `verbose: true` returns the full JSON of `docs/cli-json.md` instead.
//!
//! The structured content must stand on its own: Claude Code (2.1.283,
//! observed in the smoke test of `docs/mcp.md`) shows the model the JSON
//! of `structuredContent` *instead of* the text when a result has both.
//! So every fact in a summary is also in the structured content, and the
//! tools whose answer is text (`format`, `docs`) send none.
//!
//! The work is `neoscad serve`'s ([`crate::serve::Local`]): these
//! functions only resolve and police paths, translate arguments into the
//! server's parameters and summarise what comes back.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use serde_json::{Value, json};

use super::roots::Roots;
use crate::serve::Local;

/// The tools, in the order `tools/list` gives them.
pub const NAMES: &[&str] = &[
    "evaluate", "render", "snapshot", "check", "measure", "test", "format", "docs",
];

/// The snapshot's default size. Smaller than the command line's 1024x1024:
/// an image's cost in context grows with its pixels, and four 384-pixel
/// panels still show a model's shape and the grid's labels.
const SNAPSHOT_SIZE: &str = "768x768";

/// At most this many diagnostics and echo lines in a terse result.
const MAX_LINES: usize = 20;

/// The name inline `source` is evaluated under, in `base_dir`. Messages
/// name it, and a relative `include` resolves beside it.
const INLINE: &str = "inline.scad";
const INLINE_PREVIOUS: &str = "inline-previous.scad";
const INLINE_TEST: &str = "inline_test.scad";
/// The document a mesh `path` is imported through ([`Tools::model`]).
const INLINE_IMPORT: &str = "inline-import.scad";

/// Mesh files a model tool's `path` may name: they are imported, not
/// parsed as OpenSCAD.
const MESH_FORMATS: &[&str] = &["stl", "off", "obj", "3mf"];

fn model_props() -> Value {
    json!({
        "path": {"type": "string", "description": ".scad file"},
        "source": {"type": "string", "description": "OpenSCAD code"},
        "base_dir": {"type": "string"},
        "parts": {"type": "boolean", "description": "Enable part(\"name\"){}"},
        "verbose": {"type": "boolean", "description": "Full JSON"},
    })
}

fn tool(name: &str, description: &str, extra: Value, read_only: bool) -> Value {
    let mut props = model_props();
    if let (Some(p), Value::Object(e)) = (props.as_object_mut(), extra) {
        p.extend(e);
    }
    json!({
        "name": name,
        "description": description,
        "inputSchema": {"type": "object", "properties": props},
        "annotations": {"readOnlyHint": read_only},
    })
}

/// `tools/list`'s tools.
pub fn list() -> Vec<Value> {
    let num = |d: &str| json!({"type": "number", "description": d});
    vec![
        tool(
            "evaluate",
            "Parse and evaluate without building geometry: errors and warnings with fix hints, and echo() output. The fastest check after an edit.",
            json!({}),
            true,
        ),
        tool(
            "render",
            "Build the geometry; report bbox, volume, area, manifold and components, to verify dimensions. `export` also writes it (.stl is ASCII STL; .3mf .obj .off .svg .dxf .png).",
            json!({
                "export": {"type": "string", "description": "Output file"},
                "overwrite": {"type": "boolean"},
            }),
            false,
        ),
        tool(
            "snapshot",
            "See the model: a PNG of iso/front/top/right views on a mm grid, plus bbox and volume. Judge shape and placement with it; render and measure give exact numbers.",
            json!({
                "views": {"type": "array", "items": {"type": "string"}, "description": "iso front back left right top bottom"},
                "size": {"type": "string", "description": "WxH pixels, default 768x768"},
                "diff_against": {"type": "string", "description": "Model file: added green, removed red"},
                "diff_source": {"type": "string", "description": "Other version as source"},
                "highlight": {"type": "array", "items": {"type": "string"}, "description": "Parts in colour, rest ghosted"},
                "issues": {"type": "boolean", "description": "Mark check findings"},
                "dims": {"type": "boolean", "description": "Label bbox sizes"},
                "preview": {"type": "boolean", "description": "Show % and # modifiers"},
                "output": {"type": "string", "description": "Also save it here"},
                "overwrite": {"type": "boolean"},
            }),
            false,
        ),
        tool(
            "check",
            "3D-printability check: manifold, thin walls, overhangs, floating or tiny pieces, bed fit, intersecting parts; each finding has a location and a fix. Run before finishing; pass the spec's minimum wall as min_wall.",
            json!({
                "bed": {"type": "array", "items": {"type": "number"}, "description": "[w, d, h] mm"},
                "nozzle": num("mm, default 0.4"),
                "min_wall": num("mm, default 2 x nozzle"),
                "max_overhang": num("degrees from vertical, default 45"),
            }),
            true,
        ),
        tool(
            "measure",
            "Exact numbers: bbox, volume, centroid of model and parts; `between` two parts (part(\"a\"){...}): distance or overlap pieces; `section`: contours' area, bbox, radii about `axis`; `profile`: radii and crests (pitch) along it.",
            json!({
                "part": {"type": "string", "description": "Only this part"},
                "between": {"type": "array", "items": {"type": "string"}, "description": "[partA, partB]"},
                "section": {"type": "string", "description": "Plane, e.g. z=5"},
                "axis": {"type": "string", "description": "x, y or z"},
                "center": {"type": "array", "items": {"type": "number"}, "description": "Axis at [a, b]"},
                "profile": {"type": "array", "items": {"type": "number"}, "description": "[from, to, step]"},
            }),
            true,
        ),
        {
            let mut t = tool(
                "test",
                "Run model tests: each `module test_*()` of a *_test.scad file, checked by `// @expect` lines above it (volume 1000±1, bbox [x,y,z], manifold, components N, check clean) and assert(). Pin requirements first, then iterate until they pass.",
                json!({"filter": {"type": "string", "description": "Test ids containing this"}}),
                true,
            );
            t["inputSchema"]["properties"]["path"]["description"] = json!("Test file or directory");
            t["inputSchema"]["properties"]["source"]["description"] =
                json!("Test file source, instead of path");
            t
        },
        {
            let mut t = tool(
                "format",
                "Format OpenSCAD (whitespace only; the program is unchanged). `source` returns the text; `path` rewrites the file unless `check`.",
                json!({
                    "check": {"type": "boolean", "description": "Count changes, write nothing"},
                    "diff": {"type": "boolean", "description": "With check: the diff"},
                }),
                false,
            );
            if let Some(p) = t["inputSchema"]["properties"].as_object_mut() {
                for k in ["parts", "verbose"] {
                    p.remove(k);
                }
            }
            t
        },
        json!({
            "name": "docs",
            "description": "Reference for an OpenSCAD builtin (cube, rotate_extrude, $fn...) or a library module; no name gives the index. With `path`, that file's definitions and includes.",
            "inputSchema": {"type": "object", "properties": {
                "name": {"type": "string"},
                "path": {"type": "string", "description": "File whose definitions to search"},
                "base_dir": {"type": "string"},
                "full": {"type": "boolean", "description": "Whole comment block"},
                "verbose": {"type": "boolean", "description": "Full index"},
            }},
            "annotations": {"readOnlyHint": true},
        }),
    ]
}

/// `resources/list`: the builtins' index.
pub fn resources() -> Vec<Value> {
    vec![json!({
        "uri": "neoscad://docs",
        "name": "builtins",
        "title": "OpenSCAD builtins",
        "description": "Every builtin module, function and special variable, one line each",
        "mimeType": "text/plain",
    })]
}

/// `resources/templates/list`: one builtin's reference.
pub fn resource_templates() -> Vec<Value> {
    vec![json!({
        "uriTemplate": "neoscad://docs/{name}",
        "name": "builtin",
        "description": "Reference for one OpenSCAD builtin",
        "mimeType": "text/plain",
    })]
}

/// One tool's answer before it becomes a `CallToolResult`.
#[derive(Debug, Default)]
struct Out {
    text: String,
    structured: Value,
    png: Option<Vec<u8>>,
}

type Reply = Result<Out, String>;

pub struct Tools {
    local: Local,
    roots: Roots,
    /// Inline source is one document per base directory; calls using it
    /// take turns, so one call's text never replaces another's mid-run.
    inline: Mutex<()>,
}

impl std::fmt::Debug for Tools {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tools").field("roots", &self.roots).finish()
    }
}

/// A model argument, resolved: the file the server runs, the directory
/// it runs in, and the inline documents to close afterwards.
struct Model<'a> {
    path: PathBuf,
    base: PathBuf,
    opened: Vec<PathBuf>,
    /// For a mesh `path`, the `import()` it was rendered as.
    imported: Option<String>,
    _turn: Option<MutexGuard<'a, ()>>,
}

impl Tools {
    pub fn new(local: Local, roots: Roots) -> Tools {
        Tools {
            local,
            roots,
            inline: Mutex::new(()),
        }
    }

    pub fn cancel(&self, id: &Value) {
        self.local.cancel(id);
    }

    /// Stop every call in flight (the client is gone).
    pub fn cancel_all(&self) {
        self.local.session().cancel_all();
    }

    /// `tools/call`'s result for a known tool. Failures of the request
    /// itself (a bad argument, a path outside the roots) are results with
    /// `isError`, which MCP gives the model so it can correct the call; a
    /// model that fails to evaluate is an ordinary result that says so.
    pub fn call(&self, id: &Value, name: &str, args: &Value) -> Value {
        // A tool that panics (a bug) answers like a failed call, and the
        // server keeps its caches, as `neoscad serve` does.
        let reply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            check_args(name, args)?;
            match name {
                "evaluate" => self.evaluate(id, args),
                "render" => self.render(id, args),
                "snapshot" => self.snapshot(args),
                "check" => self.check(id, args),
                "measure" => self.measure(id, args),
                "test" => self.test(id, args),
                "format" => self.format(id, args),
                "docs" => self.docs(id, args),
                _ => Err(format!("unknown tool '{name}'")),
            }
        }))
        .unwrap_or_else(|p| {
            let what = p
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| p.downcast_ref::<String>().cloned())
                .unwrap_or_default();
            Err(format!("internal error: the tool panicked: {what}"))
        })
        // The shared parsers speak the command line's flags; an agent
        // knows only its arguments.
        .map_err(|e| crate::serve::param_names(&e));
        match reply {
            Ok(out) => {
                let mut content = vec![json!({"type": "text", "text": out.text})];
                if let Some(png) = out.png {
                    content.push(
                        json!({"type": "image", "data": base64(&png), "mimeType": "image/png"}),
                    );
                }
                let mut r = json!({"content": content, "isError": false});
                if !out.structured.is_null() {
                    r["structuredContent"] = out.structured;
                }
                r
            }
            Err(m) => json!({"content": [{"type": "text", "text": m}], "isError": true}),
        }
    }

    pub fn read_resource(&self, uri: &str) -> Option<String> {
        let name = match uri.strip_prefix("neoscad://docs") {
            Some("") => None,
            Some(rest) => Some(rest.strip_prefix('/')?.to_string()),
            None => return None,
        };
        let r = self
            .local
            .call(&json!("resource"), "docs", &json!({"name": name}))
            .ok()?;
        (r["exit_code"] == 0).then(|| r["text"].as_str().unwrap_or("").to_string())
    }

    // --- Arguments -----------------------------------------------------------

    fn base(&self, args: &Value) -> Result<PathBuf, String> {
        let home = self.roots.home().to_path_buf();
        let Some(b) = str_arg(args, "base_dir") else {
            return Ok(home);
        };
        let b = session::normal(&home.join(b));
        if !self.roots.can_read(&b) {
            return Err(self.roots.refusal("base_dir", &b));
        }
        Ok(b)
    }

    /// `key`'s path, absolute, if it may be read.
    fn readable(&self, base: &Path, args: &Value, key: &str) -> Result<Option<PathBuf>, String> {
        let Some(p) = str_arg(args, key) else {
            return Ok(None);
        };
        let p = session::normal(&base.join(p));
        if !self.roots.can_read(&p) {
            return Err(self.roots.refusal(key, &p));
        }
        Ok(Some(p))
    }

    /// `key`'s path, absolute and with symlinks resolved, if it may be
    /// written as a file of one of `formats` (lower-case extensions).
    ///
    /// The fence is where the path *resolves* ([`super::roots::resolve`]),
    /// so a planted link cannot lead a write out of the roots. And an
    /// output never replaces a file of another type: an agent that
    /// confuses `output` with `path` must not turn the user's model into
    /// a PNG (the agent-surface audit's finding 3). An existing file of
    /// the same type is replaced only with `overwrite: true`. Nothing is
    /// created here; [`make_dir`] does that once every argument is valid.
    fn writable(
        &self,
        base: &Path,
        args: &Value,
        key: &str,
        formats: &[&str],
    ) -> Result<Option<PathBuf>, String> {
        let Some(p) = str_arg(args, key) else {
            return Ok(None);
        };
        let p = session::normal(&base.join(p));
        if !self.roots.can_write(&p) {
            return Err(self.roots.refusal(key, &p));
        }
        let real = super::roots::resolve(&p);
        let ext = |q: &Path| {
            q.extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default()
        };
        let want = ext(&p);
        if !formats.contains(&want.as_str()) {
            return Err(format!(
                "{key} '{}' must end in {}",
                p.display(),
                formats
                    .iter()
                    .map(|f| format!(".{f}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if let Ok(meta) = std::fs::metadata(&real) {
            if meta.is_dir() {
                return Err(format!("{key} '{}' is a directory", p.display()));
            }
            if ext(&real) != want {
                return Err(format!(
                    "{key} '{}' is an existing {} file; a .{want} output never replaces a file of another type (choose another name)",
                    p.display(),
                    match ext(&real).as_str() {
                        "" => "extension-less".to_string(),
                        e => format!(".{e}"),
                    }
                ));
            }
            if !bool_arg(args, "overwrite") {
                return Err(format!(
                    "{key} '{}' exists; pass overwrite: true to replace it, or choose another name",
                    p.display()
                ));
            }
        }
        Ok(Some(real))
    }

    /// The model of `path` or `source` (`inline` names the document inline
    /// source is opened as).
    fn model(&self, args: &Value, inline: &str) -> Result<Model<'_>, String> {
        let base = self.base(args)?;
        match (str_arg(args, "source"), self.readable(&base, args, "path")?) {
            (Some(_), Some(_)) => Err("give either path or source, not both".into()),
            (None, None) => Err("give path (a .scad file) or source (OpenSCAD text)".into()),
            // A mesh file is rendered as its import. Given to the parser,
            // `check out/base.stl` failed with a syntax error on the STL's
            // first line (the T2 transcript audit), which reads as a bug in
            // the model rather than the wrong kind of file.
            (None, Some(path)) if is_mesh(&path) => {
                // As given: the import resolves beside the document, in
                // `base_dir`, where `path` was resolved too.
                let given = str_arg(args, "path").unwrap_or_default();
                let call = format!("import(\"{}\");", scad_string(given));
                let turn = self
                    .inline
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let doc = base.join(INLINE_IMPORT);
                self.local
                    .session()
                    .open(&doc, Some(call.clone().into_bytes()));
                Ok(Model {
                    opened: vec![doc.clone()],
                    path: doc,
                    base,
                    imported: Some(call),
                    _turn: Some(turn),
                })
            }
            (None, Some(path)) => Ok(Model {
                path,
                base,
                opened: Vec::new(),
                imported: None,
                _turn: None,
            }),
            (Some(src), None) => {
                let turn = self
                    .inline
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let path = base.join(inline);
                self.local
                    .session()
                    .open(&path, Some(src.as_bytes().to_vec()));
                Ok(Model {
                    opened: vec![path.clone()],
                    path,
                    base,
                    imported: None,
                    _turn: Some(turn),
                })
            }
        }
    }

    fn done(&self, m: Model<'_>) {
        for p in &m.opened {
            self.local.session().close(p);
        }
    }

    /// The server's common parameters for a model.
    fn params(&self, m: &Model<'_>, args: &Value) -> Value {
        json!({
            "path": m.path,
            "cwd": m.base,
            "parts": bool_arg(args, "parts"),
            "supersede": false,
        })
    }

    fn run(&self, id: &Value, method: &str, params: &Value) -> Result<Value, String> {
        self.local
            .call(id, method, params)
            .map_err(|(_, m)| format!("{method} failed: {m}"))
    }

    // --- Tools -----------------------------------------------------------------

    fn evaluate(&self, id: &Value, args: &Value) -> Reply {
        let m = self.model(args, INLINE)?;
        let main = m.path.clone();
        let imported = m.imported.clone();
        let r = self.run(id, "evaluate", &self.params(&m, args));
        self.done(m);
        let r = r?;
        let mut text = status(&r);
        push_log(&mut text, &r);
        Ok(label_import(
            imported.as_deref(),
            finish(args, text, terse_log(&r, &main), r),
        ))
    }

    fn render(&self, id: &Value, args: &Value) -> Reply {
        let m = self.model(args, INLINE)?;
        let main = m.path.clone();
        let imported = m.imported.clone();
        let export = match self.writable(&m.base, args, "export", EXPORT_FORMATS) {
            Ok(e) => e,
            Err(e) => {
                self.done(m);
                return Err(e);
            }
        };
        if let Some(out) = &export
            && let Err(e) = make_dir(out)
        {
            self.done(m);
            return Err(e);
        }
        let mut p = self.params(&m, args);
        let method = match &export {
            Some(out) => {
                p["output"] = json!(out);
                "export"
            }
            None => "render",
        };
        let r = self.run(id, method, &p);
        self.done(m);
        let r = r?;
        let mut text = status(&r);
        text.push('\n');
        text.push_str(&geometry_line_of(&r["geometry"], &r["diagnostics"]));
        if let Some(out) = &export
            && r["exit_code"] == 0
        {
            text.push_str(&format!("\nwrote {} ({} bytes)", out.display(), r["bytes"]));
        }
        push_log(&mut text, &r);
        let mut s = terse_log(&r, &main);
        s["geometry"] = terse_geometry(&r["geometry"], &r["diagnostics"]);
        if export.is_some() {
            s["output"] = r["output"].clone();
            s["bytes"] = r["bytes"].clone();
        }
        Ok(label_import(imported.as_deref(), finish(args, text, s, r)))
    }

    fn snapshot(&self, args: &Value) -> Reply {
        let mut m = self.model(args, INLINE)?;
        let main = m.path.clone();
        let imported = m.imported.clone();
        let output = match self.writable(&m.base, args, "output", &["png"]) {
            Ok(o) => o,
            Err(e) => {
                self.done(m);
                return Err(e);
            }
        };
        let diff = match (
            str_arg(args, "diff_source"),
            self.readable(&m.base, args, "diff_against")?,
        ) {
            (Some(_), Some(_)) => {
                self.done(m);
                return Err("give diff_against or diff_source, not both".into());
            }
            (Some(src), None) => {
                let p = m.base.join(INLINE_PREVIOUS);
                self.local.session().open(&p, Some(src.as_bytes().to_vec()));
                m.opened.push(p.clone());
                Some(p)
            }
            (None, d) => d,
        };
        let mut p = json!({
            "model": m.path,
            "views": args.get("views").cloned().unwrap_or(json!([])),
            "size": str_arg(args, "size").unwrap_or(SNAPSHOT_SIZE),
            "dims": bool_arg(args, "dims"),
            "preview": bool_arg(args, "preview"),
            "diff": diff,
            "highlight": args.get("highlight").cloned().unwrap_or(json!([])),
            "issues": bool_arg(args, "issues"),
            "parts": bool_arg(args, "parts"),
            "supersede": false,
        });
        if let Some(o) = &output {
            p["output"] = json!(o);
        }
        let snap = crate::snapshot::request(&p, &m.base, None).and_then(|req| {
            self.local
                .session()
                .snapshot(&req)
                .map_err(|e| e.to_string())
        });
        self.done(m);
        let snap = snap?;
        let r = snap.summary;
        let mut text = if snap.exit_code == 0 {
            "ok".to_string()
        } else {
            format!("failed (exit {})", snap.exit_code)
        };
        if let Some(png) = &snap.png {
            if let Some(o) = &output {
                make_dir(o)?;
                std::fs::write(o, png)
                    .map_err(|e| format!("cannot write '{}': {e}", o.display()))?;
                text.push_str(&format!("; saved {}", o.display()));
            }
            text.push('\n');
            text.push_str(&geometry_line_of(
                &r["geometry"],
                &r["diagnostics"]["items"],
            ));
        }
        if let Some(d) = r.get("diff") {
            text.push_str(&format!(
                "\ndiff: added {} mm³ (green), removed {} mm³ (red), unchanged {} mm³",
                num_of(&d["added_volume"]),
                num_of(&d["removed_volume"]),
                num_of(&d["unchanged_volume"])
            ));
        }
        if let Some(parts) = r.get("parts").and_then(Value::as_array) {
            text.push_str(&format!("\nparts: {}", join_strs(parts)));
        }
        if let Some(i) = r.get("issues") {
            text.push('\n');
            text.push_str(&findings_text(&i["counts"], &i["findings"]));
        }
        // The snapshot's diagnostics are its own shape (`docs/cli-json.md`):
        // `items` is the structured list.
        let log = json!({
            "diagnostics": r["diagnostics"]["items"],
            "echo": r["diagnostics"]["echo"],
        });
        push_log(&mut text, &log);
        let mut s = json!({
            "exit_code": snap.exit_code,
            "geometry": terse_geometry(&r["geometry"], &r["diagnostics"]["items"]),
            "views": r["views"],
            "size": r["size"],
            "diagnostics": terse_diags(&log["diagnostics"], &main),
        });
        for k in ["diff", "parts"] {
            if let Some(v) = r.get(k) {
                s[k] = v.clone();
            }
        }
        if let Some(i) = r.get("issues") {
            s["issues"] = json!({
                "counts": i["counts"],
                "findings": terse_findings(&i["findings"]),
            });
        }
        let mut out = label_import(imported.as_deref(), finish(args, text, s, r));
        out.png = snap.png;
        Ok(out)
    }

    fn check(&self, id: &Value, args: &Value) -> Reply {
        let m = self.model(args, INLINE)?;
        let main = m.path.clone();
        let imported = m.imported.clone();
        let mut p = self.params(&m, args);
        for k in ["bed", "nozzle", "min_wall", "max_overhang"] {
            if let Some(v) = args.get(k) {
                p[k] = v.clone();
            }
        }
        let r = self.run(id, "check", &p);
        self.done(m);
        let r = r?;
        if r.get("failed").and_then(Value::as_bool) == Some(true) || r["model"].is_null() {
            let mut text = format!(
                "failed (exit {}): the model did not render; nothing to check",
                r["exit_code"]
            );
            let d = &r["diagnostics"];
            let log = json!({
                "exit_code": r["exit_code"],
                "counts": {"errors": d["errors"], "warnings": d["warnings"], "echoes": d["echoes"]},
                "diagnostics": d["items"],
                "echo": d["echo"],
            });
            push_log(&mut text, &log);
            return Ok(label_import(
                imported.as_deref(),
                finish(args, text, terse_log(&log, &main), r),
            ));
        }
        let mut text = String::new();
        let model = &r["model"];
        if model["dimensions"] == 3 {
            text.push_str(&format!(
                "{}, {} component{}, bbox {}, thinnest wall {}\n",
                if model["manifold"] == true {
                    "manifold"
                } else {
                    "NOT manifold"
                },
                model["components"],
                if model["components"] == 1 { "" } else { "s" },
                size_of(&model["bbox"]),
                model["min_wall"]["thickness"]
                    .as_f64()
                    .map_or("-".into(), |t| format!("about {} mm (sampled)", num(t)))
            ));
        }
        text.push_str(&findings_text(&r["counts"], &r["findings"]));
        if let Some(t) = r["truncated"].as_object().filter(|t| !t.is_empty()) {
            let more: Vec<String> = t.iter().map(|(k, v)| format!("{v} more {k}")).collect();
            text.push_str(&format!("\n(and {})", more.join(", ")));
        }
        let log =
            json!({"diagnostics": r["diagnostics"]["items"], "echo": r["diagnostics"]["echo"]});
        push_log(&mut text, &log);
        let s = json!({
            "exit_code": r["exit_code"],
            "ok": r["ok"],
            "counts": r["counts"],
            "model": model,
            "findings": terse_findings(&r["findings"]),
            "truncated": r["truncated"],
        });
        Ok(label_import(imported.as_deref(), finish(args, text, s, r)))
    }

    fn measure(&self, id: &Value, args: &Value) -> Reply {
        let m = self.model(args, INLINE)?;
        let main = m.path.clone();
        let imported = m.imported.clone();
        let mut p = self.params(&m, args);
        for k in ["part", "between", "section", "axis", "center", "profile"] {
            if let Some(v) = args.get(k) {
                p[k] = v.clone();
            }
        }
        // Asked for a section, a profile or a distance, the answer is that:
        // the model's own numbers (which `render` gives) are left out.
        let focused = ["between", "section", "profile"]
            .iter()
            .any(|k| args.get(*k).is_some_and(|v| !v.is_null()));
        let r = self.run(id, "measure", &p);
        self.done(m);
        let r = r?;
        let mut lines: Vec<String> = Vec::new();
        if let Some(e) = r.get("error").and_then(Value::as_str) {
            lines.push(crate::serve::param_names(e));
        } else {
            if !focused {
                lines.push(solid_line("model", &r["model"]));
            }
            for part in r["parts"].as_array().into_iter().flatten() {
                lines.push(solid_line(
                    &format!("part {}", part["name"].as_str().unwrap_or("?")),
                    part,
                ));
            }
            let b = &r["between"];
            if b.is_object() {
                lines.push(between_line(b));
            }
            let sec = &r["section"];
            if sec.is_object() {
                lines.push(section_lines(sec));
            } else if args.get("section").is_some() {
                lines.push("section: nothing to cut".into());
            }
            let prof = &r["profile"];
            if prof.is_object() {
                lines.push(profile_lines(prof));
            } else if args.get("profile").is_some() {
                lines.push("profile: nothing to measure".into());
            }
        }
        let mut text = lines.join("\n");
        let log =
            json!({"diagnostics": r["diagnostics"]["items"], "echo": r["diagnostics"]["echo"]});
        push_log(&mut text, &log);
        let mut s = r.clone();
        if let Some(o) = s.as_object_mut() {
            for k in ["schema", "input", "timings_ms", "diagnostics"] {
                o.remove(k);
            }
            if focused {
                o.remove("model");
            }
        }
        s["diagnostics"] = terse_diags(&log["diagnostics"], &main);
        if let Some(e) = s.get("error").and_then(Value::as_str) {
            s["error"] = json!(crate::serve::param_names(e));
        }
        Ok(label_import(imported.as_deref(), finish(args, text, s, r)))
    }

    fn test(&self, id: &Value, args: &Value) -> Reply {
        let base = self.base(args)?;
        let (m, paths) = if str_arg(args, "source").is_some() {
            let m = self.model(args, INLINE_TEST)?;
            let p = vec![m.path.clone()];
            (Some(m), p)
        } else {
            let p = self.readable(&base, args, "path")?.unwrap_or(base.clone());
            (None, vec![p])
        };
        let p = json!({
            "paths": paths,
            "cwd": base,
            "filter": str_arg(args, "filter"),
            "parts": bool_arg(args, "parts"),
        });
        let r = self.run(id, "test", &p);
        if let Some(m) = m {
            self.done(m);
        }
        let r = r?;
        if let Some(e) = r.get("error").and_then(Value::as_str) {
            return Ok(finish(args, e.to_string(), json!({"error": e}), r));
        }
        let c = &r["counts"];
        let mut text = format!(
            "{} passed, {} failed ({} tests in {} files)",
            c["passed"], c["failed"], c["tests"], c["files"]
        );
        // Ids relative to the directory tested (or `base_dir`), not the
        // absolute paths the server reports: shorter, and what the agent
        // passed.
        let top = match paths.first() {
            Some(p) if p.is_dir() => p.clone(),
            _ => base.clone(),
        };
        let rel = |f: &Value| -> String {
            let f = f.as_str().unwrap_or("");
            Path::new(f)
                .strip_prefix(&top)
                .map_or(f.to_string(), |p| p.display().to_string())
        };
        let mut failed = Vec::new();
        let mut passed = Vec::new();
        for t in r["tests"].as_array().into_iter().flatten() {
            let name = format!(
                "{}::{}",
                rel(&t["file"]),
                t["name"].as_str().unwrap_or("(file)")
            );
            if t["ok"] == true {
                passed.push(name);
                continue;
            }
            let msgs: Vec<String> = t["failures"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|f| f["message"].as_str().unwrap_or("").to_string())
                .collect();
            text.push_str(&format!("\nFAIL {name}"));
            for m in &msgs {
                text.push_str(&format!("\n    {m}"));
            }
            failed.push(json!({"test": name, "line": t["line"], "failures": msgs}));
        }
        if !passed.is_empty() {
            text.push_str(&format!("\nok: {}", passed.join(", ")));
        }
        let s = json!({"counts": c, "failed": failed, "passed": passed});
        Ok(finish(args, text, s, r))
    }

    fn format(&self, id: &Value, args: &Value) -> Reply {
        let base = self.base(args)?;
        let check = bool_arg(args, "check");
        if let Some(src) = str_arg(args, "source") {
            let r = self.run(
                id,
                "format",
                &json!({"text": src, "cwd": base, "diff": check}),
            )?;
            if !r["error"].is_null() {
                return Err(format!(
                    "not formatted: {}",
                    r["error"]["message"].as_str().unwrap_or("")
                ));
            }
            let text = if check {
                check_text(r["diff"].as_str().unwrap_or(""), bool_arg(args, "diff"))
            } else {
                r["text"].as_str().unwrap_or("").to_string()
            };
            return Ok(Out {
                text: if text.is_empty() {
                    "already formatted".into()
                } else {
                    text
                },
                structured: Value::Null,
                png: None,
            });
        }
        let Some(path) = self.readable(&base, args, "path")? else {
            return Err("give path (a .scad file) or source".into());
        };
        if !check && !self.roots.can_write(&path) {
            return Err(self.roots.refusal("path", &path));
        }
        let r = self.run(
            id,
            "format",
            &json!({"path": path, "cwd": base, "diff": true}),
        )?;
        if !r["error"].is_null() {
            return Err(format!(
                "not formatted: {}",
                r["error"]["message"].as_str().unwrap_or("")
            ));
        }
        let changed = r["changed"] == true;
        let text = match (changed, check) {
            (false, _) => "already formatted".to_string(),
            (true, true) => check_text(r["diff"].as_str().unwrap_or(""), bool_arg(args, "diff")),
            (true, false) => {
                std::fs::write(&path, r["text"].as_str().unwrap_or(""))
                    .map_err(|e| format!("cannot write '{}': {e}", path.display()))?;
                format!("reformatted {}", path.display())
            }
        };
        Ok(Out {
            text,
            structured: Value::Null,
            png: None,
        })
    }

    fn docs(&self, id: &Value, args: &Value) -> Reply {
        let base = self.base(args)?;
        let file = self.readable(&base, args, "path")?;
        let r = self.run(
            id,
            "docs",
            &json!({"name": str_arg(args, "name"), "file": file, "cwd": base,
                    "full": bool_arg(args, "full"), "brief": !bool_arg(args, "verbose"),
                    "file_arg": "`path` (the file that defines or includes it)"}),
        )?;
        Ok(Out {
            text: r["text"].as_str().unwrap_or("").trim_end().to_string(),
            structured: Value::Null,
            png: None,
        })
    }
}

/// What `render`'s `export` can write: `neoscad serve`'s export formats
/// by extension (`binstl` is a format name, not an extension).
const EXPORT_FORMATS: &[&str] = &[
    "stl", "off", "obj", "3mf", "wrl", "pov", "svg", "dxf", "pdf", "png", "echo", "ast", "csg",
];

/// What `format` with `check` says: how many lines would change, or the
/// diff itself with `diff: true`. A whole diff of a file that only needs
/// its indentation fixed is long, and the agent's next step is the same
/// either way (format it).
fn check_text(diff: &str, full: bool) -> String {
    if diff.is_empty() {
        return String::new();
    }
    if full {
        return diff.to_string();
    }
    let n = diff
        .lines()
        .skip_while(|l| !l.starts_with("@@"))
        .filter(|l| l.starts_with('-'))
        .count();
    format!(
        "not formatted: {n} line{} would change (diff: true shows them)",
        if n == 1 { "" } else { "s" }
    )
}

/// Whether `p` names a mesh file ([`MESH_FORMATS`]).
fn is_mesh(p: &Path) -> bool {
    p.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .is_some_and(|e| MESH_FORMATS.contains(&e.as_str()))
}

/// `s` as the inside of an OpenSCAD string literal.
fn scad_string(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

/// A result for a mesh `path`, labelled with the `import()` it was
/// rendered as: first in the text, and as `imported` in the structured
/// content (the full JSON's too, with `verbose`).
fn label_import(imported: Option<&str>, mut out: Out) -> Out {
    if let Some(call) = imported {
        out.text = format!("path is a mesh file: rendered as `{call}`\n{}", out.text);
        if let Some(o) = out.structured.as_object_mut() {
            o.insert("imported".into(), json!(call));
        }
    }
    out
}

/// Create an output's directory (inside the roots: `writable` resolved
/// it), so "write it to out/x.stl" just works.
fn make_dir(p: &Path) -> Result<(), String> {
    match p.parent() {
        Some(dir) => std::fs::create_dir_all(dir)
            .map_err(|e| format!("cannot create '{}': {e}", dir.display())),
        None => Ok(()),
    }
}

/// Refuse arguments of the wrong JSON type, and arguments the tool does
/// not take, naming the argument and what it should be. A wrong type was
/// silently treated as absent (`"parts": "true"` gave "no part 'a'" with
/// no reason), which is the least actionable answer an agent can get.
fn check_args(name: &str, args: &Value) -> Result<(), String> {
    let tools = list();
    let Some(tool) = tools.iter().find(|t| t["name"] == name) else {
        return Ok(());
    };
    let props = &tool["inputSchema"]["properties"];
    let obj = match args {
        Value::Null => return Ok(()),
        Value::Object(o) => o,
        _ => return Err(format!("the arguments of {name} must be an object")),
    };
    let kind = |v: &Value| match v {
        Value::Null => "null",
        Value::Bool(_) => "a boolean",
        Value::Number(_) => "a number",
        Value::String(_) => "a string",
        Value::Array(_) => "an array",
        Value::Object(_) => "an object",
    };
    let fits = |ty: &str, v: &Value| match ty {
        "string" => v.is_string(),
        "boolean" => v.is_boolean(),
        "number" => v.is_number(),
        "array" => v.is_array(),
        _ => true,
    };
    let a = |ty: &str| match ty {
        "array" | "object" => format!("an {ty}"),
        t => format!("a {t}"),
    };
    for (k, v) in obj {
        let Some(schema) = props.get(k) else {
            let known: Vec<&str> = props
                .as_object()
                .map(|o| o.keys().map(String::as_str).collect())
                .unwrap_or_default();
            return Err(format!(
                "{name} has no argument `{k}`; it takes {}",
                known.join(", ")
            ));
        };
        if v.is_null() {
            continue;
        }
        let ty = schema["type"].as_str().unwrap_or("");
        if !fits(ty, v) {
            return Err(format!(
                "argument `{k}` of {name} must be {}, not {} ({v})",
                a(ty),
                kind(v)
            ));
        }
        if let (Some(items), Some(item_ty)) = (v.as_array(), schema["items"]["type"].as_str())
            && let Some(bad) = items.iter().find(|x| !fits(item_ty, x))
        {
            return Err(format!(
                "argument `{k}` of {name} must be an array of {item_ty}s, but has {} ({bad})",
                kind(bad)
            ));
        }
    }
    Ok(())
}

// --- Summaries ---------------------------------------------------------------

fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

fn bool_arg(args: &Value, key: &str) -> bool {
    args.get(key).and_then(Value::as_bool).unwrap_or(false)
}

/// The terse result, or with `verbose` the server's whole result (in the
/// text too: a client may show the model only the text).
fn finish(args: &Value, text: String, terse: Value, full: Value) -> Out {
    if bool_arg(args, "verbose") {
        return Out {
            text: format!("{text}\n{full}"),
            structured: full,
            png: None,
        };
    }
    let mut terse = terse;
    round_json(&mut terse);
    Out {
        text,
        structured: terse,
        png: None,
    }
}

/// A number as an agent reads it: six significant digits, no trailing
/// zeros (Rust's shortest form of the rounded value), as in the structured
/// content ([`round_json`]).
fn num(x: f64) -> String {
    let r = session::stats::round6(x);
    if r == 0.0 { "0".into() } else { format!("{r}") }
}

/// Every non-integer number in a terse result to six significant digits.
/// Render and snapshot gave Manifold's volumes and boxes at full
/// precision (17 digits), check at four decimals and measure at six: one
/// rule reads the same everywhere and is short. `verbose` keeps the full
/// numbers.
fn round_json(v: &mut Value) {
    match v {
        Value::Number(n) if n.is_f64() => {
            if let Some(x) = n.as_f64() {
                *v = json!(session::stats::round6(x));
            }
        }
        Value::Array(a) => a.iter_mut().for_each(round_json),
        Value::Object(o) => o.values_mut().for_each(round_json),
        _ => {}
    }
}

fn num_of(v: &Value) -> String {
    v.as_f64().map_or("-".into(), num)
}

fn vec_of(v: &Value) -> String {
    let items: Vec<String> = v.as_array().into_iter().flatten().map(num_of).collect();
    format!("[{}]", items.join(", "))
}

fn size_of(bbox: &Value) -> String {
    let items: Vec<String> = bbox["size"]
        .as_array()
        .into_iter()
        .flatten()
        .map(num_of)
        .collect();
    items.join(" x ")
}

fn corners_of(bbox: &Value) -> String {
    format!("{}..{}", vec_of(&bbox["min"]), vec_of(&bbox["max"]))
}

fn join_strs(v: &[Value]) -> String {
    v.iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join(", ")
}

/// `ok`, or why not, and the counts.
fn status(r: &Value) -> String {
    let c = &r["counts"];
    let n = |k: &str| c[k].as_u64().unwrap_or(0);
    let head = if r["exit_code"] == 0 {
        "ok".to_string()
    } else {
        format!("failed (exit {})", r["exit_code"])
    };
    let mut parts = Vec::new();
    for (k, one) in [("errors", "error"), ("warnings", "warning")] {
        match n(k) {
            0 => {}
            1 => parts.push(format!("1 {one}")),
            x => parts.push(format!("{x} {k}")),
        }
    }
    if parts.is_empty() {
        head
    } else {
        format!("{head}: {}", parts.join(", "))
    }
}

/// The geometry object as a terse result carries it: a pinched edge gets
/// the fix, so the structured content says what to do on its own.
fn terse_geometry(g: &Value, diags: &Value) -> Value {
    let mut g = g.clone();
    if g["pinched"].is_object() {
        let fix = pinch_fix(&g, diags);
        g["pinched"]["fix"] = json!(fix);
    }
    if g["stl_precision"].is_object() {
        g["stl_precision"]["fix"] = json!(stl_fix(&g["stl_precision"]));
    }
    g
}

/// What to do about edges that break only at an STL's 32-bit precision.
fn stl_fix(p: &Value) -> String {
    session::check::stl_precision_fix(p["spacing"].as_f64().unwrap_or(0.0))
}

/// What to do about pinched edges. Booleans with an inside-out or partly
/// flipped polyhedron leave them too, and there the usual advice (overlap
/// the parts) is wrong: an agent in the CAD pilot followed it for about
/// 90 turns. So when the diagnostics report such a polyhedron, the fix
/// points to that warning first. A result with no volume is parts that
/// only touch (`touch_only`), where there is nothing to overlap.
fn pinch_fix(g: &Value, diags: &Value) -> String {
    let winding = diags.as_array().into_iter().flatten().find(|d| {
        matches!(
            d["code"].as_str(),
            Some("polyhedron-inside-out" | "polyhedron-flipped-faces")
        )
    });
    match winding {
        None if g["pinched"]["touch_only"] == true => session::check::TOUCH_FIX.to_string(),
        Some(d) => session::check::pinch_from_winding(&format!(
            "the {} warning{}",
            d["code"].as_str().unwrap_or(""),
            d["line"]
                .as_u64()
                .map_or(String::new(), |l| format!(" (line {l})"))
        )),
        None => session::check::PINCH_FIX.to_string(),
    }
}

fn geometry_line(g: &Value) -> String {
    geometry_line_of(g, &Value::Null)
}

/// [`geometry_line`], with the diagnostics a pinched edge's fix may point
/// to.
fn geometry_line_of(g: &Value, diags: &Value) -> String {
    let mut line = geometry_summary(g);
    // Manifold's status cannot see two pieces touching along an edge; an
    // STL of it can (`session::stats::pinched`).
    if g["pinched"].is_object() {
        let n = g["pinched"]["edges"].as_u64().unwrap_or(0);
        line.push_str(&format!(
            "\nnot manifold as a file: {n} edge{} shared by more than two faces, the first at {}: {}",
            if n == 1 { "" } else { "s" },
            vec_of(&g["pinched"]["point"]),
            pinch_fix(g, diags)
        ));
    }
    // Slicers read an STL as 32-bit floats, which can merge vertices that
    // are distinct in the solid (`session::mesh::weld`).
    let p = &g["stl_precision"];
    if p.is_object() {
        let e = p["nonmanifold_edges"].as_u64().unwrap_or(0);
        let what = match p["collapsed_faces"].as_u64().unwrap_or(0) {
            0 => "vertices a hair apart merge".to_string(),
            1 => "1 triangle collapses".to_string(),
            n => format!("{n} triangles collapse"),
        };
        line.push_str(&format!(
            "\nnot manifold as an STL: {what} at 32-bit precision (as slicers read it), leaving \
             {e} edge{} shared by other than two faces, the first at {}: {}",
            if e == 1 { "" } else { "s" },
            vec_of(&p["point"]),
            stl_fix(p)
        ));
    }
    line
}

fn geometry_summary(g: &Value) -> String {
    match g["dimensions"].as_u64() {
        Some(3) => format!(
            "3D bbox {} mm at {}, volume {} mm³, area {} mm², {}, {} component{}, {} triangles",
            size_of(&g["bbox"]),
            corners_of(&g["bbox"]),
            num_of(&g["volume"]),
            num_of(&g["area"]),
            if g["manifold"] == true {
                "manifold"
            } else {
                "NOT manifold"
            },
            g["components"],
            if g["components"] == 1 { "" } else { "s" },
            g["triangles"],
        ),
        Some(2) => format!(
            "2D bbox {} mm at {}, area {} mm², {} contour(s)",
            size_of(&g["bbox"]),
            corners_of(&g["bbox"]),
            num_of(&g["area"]),
            g["contours"],
        ),
        _ => "empty: no geometry".into(),
    }
}

fn solid_line(label: &str, s: &Value) -> String {
    if s.is_null() {
        return format!("{label}: empty");
    }
    if s["dimensions"] == 2 {
        return format!("{label}: {}", geometry_line(s));
    }
    format!(
        "{label}: bbox {} mm at {}, volume {} mm³, area {} mm², centroid {}",
        size_of(&s["bbox"]),
        corners_of(&s["bbox"]),
        num_of(&s["volume"]),
        num_of(&s["area"]),
        vec_of(&s["centroid"])
    )
}

/// Distance, touch and overlap of two parts, with the overlap's pieces.
fn between_line(b: &Value) -> String {
    let mut t = format!(
        "{} to {}: distance {} mm{}",
        b["a"].as_str().unwrap_or("?"),
        b["b"].as_str().unwrap_or("?"),
        num_of(&b["distance"]),
        if b["touching"] == true {
            ", touching"
        } else {
            ""
        },
    );
    if b["overlapping"] == true {
        t.push_str(&format!(", overlap {} mm³", num_of(&b["overlap_volume"])));
        let pieces = b["pieces"].as_array().map_or(&[][..], Vec::as_slice);
        let n = b["overlap_pieces"].as_u64().unwrap_or(pieces.len() as u64);
        if n > 1 {
            t.push_str(&format!(" in {n} pieces:"));
            for p in pieces {
                t.push_str(&format!(
                    "\n  {} mm³ at {}",
                    num_of(&p["volume"]),
                    corners_of(&p["bbox"])
                ));
            }
        } else {
            t.push_str(&format!(" at {}", corners_of(&b["overlap_bbox"])));
        }
    }
    t
}

/// A section, and each contour with its radii about the axis.
fn section_lines(sec: &Value) -> String {
    let mut t = format!(
        "section {}: area {} mm², perimeter {} mm, {} contour(s), bbox {}",
        sec["plane"].as_str().unwrap_or("?"),
        num_of(&sec["area"]),
        num_of(&sec["perimeter"]),
        sec["contours"],
        corners_of(&sec["bbox"])
    );
    let axis = sec["axis"].as_str().unwrap_or("z");
    for o in sec["outlines"].as_array().into_iter().flatten() {
        t.push_str(&format!(
            "\n  {} {} mm² at {}, radius {}..{} about {axis} at {}",
            if o["hole"] == true { "hole" } else { "outline" },
            num_of(&o["area"]),
            corners_of(&o["bbox"]),
            num_of(&o["radius"][0]),
            num_of(&o["radius"][1]),
            vec_of(&sec["center"]),
        ));
    }
    t
}

/// A radius profile: the range, the crests and pitch, then each band.
fn profile_lines(p: &Value) -> String {
    let axis = p["axis"].as_str().unwrap_or("z");
    let mut t = format!(
        "profile along {axis} at {}, {}..{} every {}: outer radius {}..{}",
        vec_of(&p["center"]),
        num_of(&p["from"]),
        num_of(&p["to"]),
        num_of(&p["step"]),
        num_of(&p["radius"][0]),
        num_of(&p["radius"][1]),
    );
    if !p["pitch"].is_null() {
        t.push_str(&format!(
            ", pitch {} (crests {}..{})",
            num_of(&p["pitch"]),
            num_of(&p["pitch_span"][0]),
            num_of(&p["pitch_span"][1])
        ));
    }
    t.push_str(&format!("\ncrests at {axis} = {}", vec_of(&p["crests"])));
    t.push_str(&format!("\nbands [{axis}, rmin, rmax]:"));
    for b in p["bands"].as_array().into_iter().flatten() {
        t.push_str(&format!(" {}", vec_of(b)));
    }
    t
}

/// A file as a diagnostic names it to an agent: its name.
fn short(f: &str) -> String {
    Path::new(f)
        .file_name()
        .map_or(f.into(), |n| n.to_string_lossy().into_owned())
}

/// A diagnostic without its span and full text; `file` only when it is
/// not the model's own (an included file), by its name alone.
fn terse_diag(d: &Value, main: &Path) -> Value {
    let mut t = json!({"severity": d["severity"], "code": d["code"], "message": d["message"]});
    if let Some(f) = d["file"].as_str()
        && Path::new(f) != main
    {
        t["file"] = json!(short(f));
    }
    if let Some(v) = d.get("line") {
        t["line"] = v.clone();
    }
    // The column says where on the line: an agent iterating on one-line
    // inline source learns nothing from the line alone.
    if let Some(c) = d["span"]["start"].get("column") {
        t["column"] = c.clone();
    }
    if let Some(h) = d["hints"].get(0).and_then(|h| h.get("message")) {
        t["hint"] = h.clone();
    }
    t
}

fn terse_diags(v: &Value, main: &Path) -> Value {
    Value::Array(
        v.as_array()
            .into_iter()
            .flatten()
            .take(MAX_LINES)
            .map(|d| terse_diag(d, main))
            .collect(),
    )
}

fn terse_log(r: &Value, main: &Path) -> Value {
    json!({
        "exit_code": r["exit_code"],
        "counts": r["counts"],
        "diagnostics": terse_diags(&r["diagnostics"], main),
        "echo": r["echo"].as_array().map(|e| e.iter().take(MAX_LINES).cloned().collect::<Vec<_>>()),
    })
}

/// The diagnostics and echo lines under a summary, at most
/// [`MAX_LINES`] of each.
fn push_log(text: &mut String, r: &Value) {
    let diags = r["diagnostics"].as_array().map_or(&[][..], Vec::as_slice);
    for d in diags.iter().take(MAX_LINES) {
        let col = d["span"]["start"]["column"]
            .as_u64()
            .or_else(|| d["column"].as_u64())
            .map_or(String::new(), |c| format!(":{c}"));
        let at = match (d["file"].as_str(), d["line"].as_u64()) {
            (Some(f), Some(l)) => format!(
                " {}:{l}{col}",
                Path::new(f)
                    .file_name()
                    .map_or(f.into(), |n| n.to_string_lossy())
            ),
            (None, Some(l)) => format!(" line {l}{col}"),
            _ => String::new(),
        };
        text.push_str(&format!(
            "\n{}{at}: {}",
            d["severity"].as_str().unwrap_or("error"),
            d["message"].as_str().unwrap_or("")
        ));
        if let Some(h) = d["hints"]
            .get(0)
            .and_then(|h| h.get("message"))
            .and_then(Value::as_str)
        {
            text.push_str(&format!(" ({h})"));
        }
    }
    if diags.len() > MAX_LINES {
        text.push_str(&format!(
            "\n... {} more diagnostics (verbose: true)",
            diags.len() - MAX_LINES
        ));
    }
    let echo = r["echo"].as_array().map_or(&[][..], Vec::as_slice);
    for e in echo.iter().take(MAX_LINES) {
        text.push('\n');
        text.push_str(e.as_str().unwrap_or(""));
    }
    if echo.len() > MAX_LINES {
        text.push_str(&format!(
            "\n... {} more echo lines (verbose: true)",
            echo.len() - MAX_LINES
        ));
    }
}

/// The finding whose fix a later one repeats: the fixes of one code are
/// mostly the same text (every overhang's "add support, chamfer it..."),
/// so each is given once and later findings point back to it.
fn same_fix_as(findings: &[Value], i: usize) -> Option<&Value> {
    let f = &findings[i];
    findings[..i]
        .iter()
        .find(|g| g["code"] == f["code"] && g["fix"] == f["fix"])
        .map(|g| &g["id"])
}

fn terse_findings(findings: &Value) -> Value {
    let list = findings.as_array().map_or(&[][..], Vec::as_slice);
    Value::Array(
        (0..list.len())
            .map(|i| {
                let f = &list[i];
                let mut t = json!({
                    "id": f["id"], "severity": f["severity"], "code": f["code"],
                    "message": f["message"], "part": f["part"],
                    "point": f["location"]["point"],
                });
                if !session::check::fix_shown(f) {
                    return t;
                }
                match same_fix_as(list, i) {
                    Some(id) => t["fix_as"] = id.clone(),
                    None => t["fix"] = f["fix"].clone(),
                }
                t
            })
            .collect(),
    )
}

fn findings_text(counts: &Value, findings: &Value) -> String {
    let n = |k: &str| counts[k].as_u64().unwrap_or(0);
    let mut text = if n("errors") + n("warnings") + n("info") == 0 {
        "printable: no findings".to_string()
    } else {
        format!(
            "{} error(s), {} warning(s), {} info",
            n("errors"),
            n("warnings"),
            n("info")
        )
    };
    let list = findings.as_array().map_or(&[][..], Vec::as_slice);
    for (i, f) in list.iter().enumerate() {
        let fix = match same_fix_as(list, i) {
            _ if !session::check::fix_shown(f) => String::new(),
            Some(id) => format!(" Fix: as #{id}"),
            None => format!(" Fix: {}", f["fix"].as_str().unwrap_or("")),
        };
        text.push_str(&format!(
            "\n#{} {} {}{}: {} at {}.{fix}",
            f["id"],
            f["severity"].as_str().unwrap_or(""),
            f["code"].as_str().unwrap_or(""),
            f["part"]
                .as_str()
                .map_or(String::new(), |p| format!(" (part {p})")),
            f["message"].as_str().unwrap_or(""),
            vec_of(&f["location"]["point"]),
        ));
    }
    text
}

/// Standard base64 with padding (RFC 4648 §4), for image content.
fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let b = [c[0], *c.get(1).unwrap_or(&0), *c.get(2).unwrap_or(&0)];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        for (i, shift) in [18, 12, 6, 0].into_iter().enumerate() {
            if i <= c.len() {
                out.push(T[(n >> shift) as usize & 63] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_rfc4648_vectors() {
        // RFC 4648 §10.
        for (i, o) in [
            ("", ""),
            ("f", "Zg=="),
            ("fo", "Zm8="),
            ("foo", "Zm9v"),
            ("foob", "Zm9vYg=="),
            ("fooba", "Zm9vYmE="),
            ("foobar", "Zm9vYmFy"),
        ] {
            assert_eq!(base64(i.as_bytes()), o);
        }
    }

    #[test]
    fn numbers_read_short() {
        assert_eq!(num(15552.0), "15552");
        assert_eq!(num(0.1 + 0.2), "0.3");
        assert_eq!(num(-1e-12), "0");
        assert_eq!(num(1.2345678), "1.23457");
        assert_eq!(num(13105.098123), "13105.1");
        let mut v =
            json!({"volume": 13105.098123456, "triangles": 23152, "bbox": [17.32050807568877]});
        round_json(&mut v);
        assert_eq!(
            v,
            json!({"volume": 13105.1, "triangles": 23152, "bbox": [17.3205]})
        );
    }

    #[test]
    fn tools_have_unique_names_in_order() {
        let names: Vec<String> = list()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(names, NAMES);
    }
}
