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
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{Value, json};

use super::app::{Activity, Apps};
use super::bridge::Bridge;
use super::roots::Roots;
use crate::serve::Local;

mod browser;
pub use browser::APP_NAMES;
pub use browser::NAMES as BROWSER_NAMES;
use browser::Surface;

std::thread_local! {
    /// The tool this thread is running (each `tools/call` has a thread of
    /// its own), for the app's activity line when a model tool runs on
    /// the app's document.
    static TOOL: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

/// The tool this thread is running.
fn current_tool() -> String {
    TOOL.with(|t| t.borrow().clone())
}

/// The tools, in the order `tools/list` gives them.
pub const NAMES: &[&str] = &[
    "evaluate", "render", "snapshot", "check", "measure", "test", "format", "docs",
];

/// Tools listed only when asked for (`neoscad mcp --tool NAME`). Every
/// listed tool is paid for in context by every session, and a modelling
/// session rarely runs model tests or the formatter (agents that
/// formatted did so after the finished export, then rendered again to
/// check it). `format` is
/// listed with `--browser` too, where it formats the page's text as one
/// undoable edit.
pub const OPT_IN: &[&str] = &["test", "format"];

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
            "Build the geometry; report bbox, volume, area, manifold and components, to verify dimensions. `export` also writes it (.stl is ASCII STL; .3mf .obj .off .svg .dxf .png) and reads a mesh back: triangles, watertight, z range.",
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
            "3D-printability check: manifold, thin walls, overhangs, floating or tiny pieces, bed fit, intersecting parts; each finding has a location and a fix. Pass the spec's minimum wall as min_wall. `export` and `sections` add those in the same call.",
            json!({
                "bed": {"type": "array", "items": {"type": "number"}, "description": "[w, d, h] mm"},
                "nozzle": num("mm, default 0.4"),
                "min_wall": num("mm, default 2 x nozzle"),
                "max_overhang": num("degrees from vertical, default 45"),
                "export": {"type": "string", "description": "Also write it, as render"},
                "overwrite": {"type": "boolean"},
                "sections": {"type": "array", "items": {"type": "string"}, "description": "Also measure these, e.g. [\"z=5\"]"},
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
                "sketch": {"type": "string", "description": "Sketch name: solved entities"},
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
            "description": "Reference for an OpenSCAD builtin (cube, rotate_extrude, $fn...), a printing recipe (snap_hook...) or a library module; no name gives the index. With `path`, that file's definitions and includes.",
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

/// `resources/list`: the builtins' index, and the printing recipes.
pub fn resources() -> Vec<Value> {
    vec![
        json!({
            "uri": "neoscad://docs",
            "name": "builtins",
            "title": "OpenSCAD builtins",
            "description": "Every builtin module, function and special variable, one line each",
            "mimeType": "text/plain",
        }),
        json!({
            "uri": "neoscad://recipes",
            "name": "recipes",
            "title": "Printing recipes",
            "description": "OpenSCAD modules for a countersink, rounded corners, a fillet, a thread and a snap hook",
            "mimeType": "text/plain",
        }),
    ]
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
    /// The web page's bridge (`--browser`), whose tools are listed and
    /// whose page is the model when a call gives neither path nor source.
    browser: Option<Arc<Bridge>>,
    /// The running NeoSCAD apps (plain `neoscad mcp`, unless `--no-app`):
    /// while one is connected, its tools are listed and its focused
    /// document is the model when a call gives neither path nor source.
    apps: Option<Arc<Apps>>,
    /// The [`OPT_IN`] tools this server lists.
    opted: Vec<&'static str>,
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
    /// For a mesh `path`, the `import()` it was rendered as; for the web
    /// page's text, its file and version.
    label: Label,
    /// The web page's customizer values (as `-D` assignments) and its
    /// `part()` switch, so a tool sees the model the user is looking at
    /// rather than the text's defaults.
    defines: Vec<String>,
    parts: bool,
    _turn: Option<MutexGuard<'a, ()>>,
    /// The app's "is checking the model" while a model tool runs on its
    /// document.
    _activity: Option<Activity>,
}

impl Tools {
    /// `opt_in` names the [`OPT_IN`] tools to list (`--tool`); an unknown
    /// name is an error naming the choices.
    pub fn new(
        local: Local,
        roots: Roots,
        browser: Option<Arc<Bridge>>,
        apps: Option<Arc<Apps>>,
        opt_in: &[String],
    ) -> Result<Tools, String> {
        let mut opted = Vec::new();
        for name in opt_in {
            match OPT_IN.iter().find(|t| **t == name.as_str()) {
                Some(t) => opted.push(*t),
                None => {
                    return Err(format!(
                        "--tool {name}: not an optional tool (choose from {})",
                        OPT_IN.join(", ")
                    ));
                }
            }
        }
        if browser.is_some() && !opted.contains(&"format") {
            opted.push("format");
        }
        Ok(Tools {
            local,
            roots,
            inline: Mutex::new(()),
            browser,
            apps,
            opted,
        })
    }

    /// Whether the app's tools are listed: an app is connected (and no
    /// web page bridge, which takes the same names).
    fn app_listed(&self) -> bool {
        self.browser.is_none() && self.apps.as_ref().is_some_and(|a| a.connected())
    }

    /// The tools this server lists: the opted-in ones and, with
    /// `--browser`, the browser's too; while a NeoSCAD app is connected,
    /// the app's (and `format`, which then formats its document).
    pub fn list(&self) -> Vec<Value> {
        let app = self.app_listed();
        let mut tools: Vec<Value> = list()
            .into_iter()
            .filter(|t| {
                let name = t["name"].as_str().unwrap_or("");
                !OPT_IN.contains(&name) || self.opted.contains(&name) || (app && name == "format")
            })
            .collect();
        if self.browser.is_some() {
            tools.extend(browser::list());
        } else if app {
            tools.extend(browser::app_list());
        }
        tools
    }

    /// Whether `name` is a tool here. The app's tools stay known once an
    /// app has connected in this session, listed or not: an agent that
    /// used them before the app restarted gets the wait for the app, not
    /// "Unknown tool". Before any app, they are as unknown as unlisted.
    pub fn knows(&self, name: &str) -> bool {
        let app = self.browser.is_none() && self.apps.as_ref().is_some_and(|a| a.seen());
        (NAMES.contains(&name)
            && (!OPT_IN.contains(&name) || self.opted.contains(&name) || (app && name == "format")))
            || (self.browser.is_some() && BROWSER_NAMES.contains(&name))
            || (app && APP_NAMES.contains(&name))
    }

    /// The schema `name`'s arguments are checked against.
    fn schemas(&self) -> Vec<Value> {
        let mut tools = list();
        if self.browser.is_none() && self.apps.is_some() {
            tools.extend(browser::app_list());
        } else {
            tools.extend(browser::list());
        }
        tools
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
        TOOL.with(|t| *t.borrow_mut() = name.to_string());
        let reply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            check_args(&self.schemas(), name, args)?;
            if BROWSER_NAMES.contains(&name) {
                return match (&self.browser, &self.apps) {
                    (Some(b), _) => self.browser_tool(&Surface::Page(b), name, args),
                    (None, Some(apps)) if APP_NAMES.contains(&name) => {
                        let doc = apps.target(args["document"].as_u64())?;
                        self.browser_tool(&Surface::App(apps, doc), name, args)
                    }
                    _ => Err(format!("unknown tool '{name}'")),
                };
            }
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

    /// Whether the server runs with NeoSCAD's sketch extension
    /// (`neoscad mcp --enable sketch`).
    fn sketch_on(&self) -> bool {
        self.local
            .session()
            .config()
            .extensions
            .has(session::Extension::Sketch)
    }

    pub fn read_resource(&self, uri: &str) -> Option<String> {
        if uri == "neoscad://recipes" {
            if self.sketch_on() {
                return Some(format!("{}\n{}", super::RECIPES, super::SKETCH_RECIPE));
            }
            return Some(super::RECIPES.to_string());
        }
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
            // With a web page connected, its text is the model: what the
            // user is looking at is what the agent asks about. The same
            // for a connected app's focused document; without an app,
            // nothing waits for one.
            (None, None) => match (&self.browser, &self.apps) {
                (Some(b), _) => self.page_model(&Surface::Page(b), base, inline),
                (None, Some(apps)) if apps.connected() => {
                    let doc = apps.target(None)?;
                    self.page_model(&Surface::App(apps, doc), base, inline)
                }
                _ => Err("give path (a .scad file) or source (OpenSCAD text)".into()),
            },
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
                    label: Label::Import(call),
                    defines: Vec::new(),
                    parts: false,
                    _turn: Some(turn),
                    _activity: None,
                })
            }
            (None, Some(path)) => Ok(Model {
                path,
                base,
                opened: Vec::new(),
                label: Label::None,
                defines: Vec::new(),
                parts: false,
                _turn: None,
                _activity: None,
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
                    label: Label::None,
                    defines: Vec::new(),
                    parts: false,
                    _turn: Some(turn),
                    _activity: None,
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
            "parts": bool_arg(args, "parts") || m.parts,
            "defines": m.defines,
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
        let imported = m.label.clone();
        let r = self.run(id, "evaluate", &self.params(&m, args));
        self.done(m);
        let r = r?;
        let mut text = status(&r);
        push_log(&mut text, &r);
        Ok(label(
            &imported,
            finish(args, text, terse_log(&r, &main), r),
        ))
    }

    fn render(&self, id: &Value, args: &Value) -> Reply {
        let m = self.model(args, INLINE)?;
        let main = m.path.clone();
        let imported = m.label.clone();
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
        let mut back = Value::Null;
        if let Some(out) = &export
            && r["exit_code"] == 0
        {
            text.push_str(&format!("\nwrote {} ({} bytes)", out.display(), r["bytes"]));
            back = read_back(out);
            if !back.is_null() {
                text.push_str(&format!("; {}", read_back_text(&back)));
            }
        }
        push_log(&mut text, &r);
        let mut s = terse_log(&r, &main);
        s["geometry"] = terse_geometry(&r["geometry"], &r["diagnostics"]);
        if export.is_some() {
            s["output"] = r["output"].clone();
            s["bytes"] = r["bytes"].clone();
            if !back.is_null() {
                s["read_back"] = back;
            }
        }
        Ok(label(&imported, finish(args, text, s, r)))
    }

    fn snapshot(&self, args: &Value) -> Reply {
        let mut m = self.model(args, INLINE)?;
        let main = m.path.clone();
        let imported = m.label.clone();
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
            "parts": bool_arg(args, "parts") || m.parts,
            "defines": m.defines,
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
        put_echo(&mut s, &log["echo"]);
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
        let mut out = label(&imported, finish(args, text, s, r));
        out.png = snap.png;
        Ok(out)
    }

    /// `check`, and with `export` or `sections` what `render` and
    /// `measure` would answer too, in one result: each turn an agent takes
    /// re-reads its whole context, while the calls take milliseconds on the
    /// warm session. A snapshot stays its own tool, so a picture is taken
    /// only when the agent decides it needs one. Without these arguments
    /// the result is exactly the plain check's.
    fn check(&self, id: &Value, args: &Value) -> Reply {
        // Refuse a bad export path before any work.
        if str_arg(args, "export").is_some() {
            let base = self.base(args)?;
            self.writable(&base, args, "export", EXPORT_FORMATS)?;
        }
        let (mut out, rendered) = self.check_only(id, args)?;
        if !rendered {
            return Ok(out);
        }
        if let Some(e) = str_arg(args, "export") {
            let mut a = self.sub_args(args);
            a["export"] = json!(e);
            a["overwrite"] = json!(bool_arg(args, "overwrite"));
            match self.render(id, &a) {
                Ok(r) => {
                    let s = &r.structured;
                    match s["output"].as_str() {
                        Some(o) => {
                            out.text
                                .push_str(&format!("\nwrote {o} ({} bytes)", s["bytes"]));
                            if !s["read_back"].is_null() {
                                out.text
                                    .push_str(&format!("; {}", read_back_text(&s["read_back"])));
                            }
                        }
                        None => out.text.push_str("\nexport failed: nothing was written"),
                    }
                    out.structured["export"] = json!({
                        "output": s["output"], "bytes": s["bytes"], "read_back": s["read_back"],
                    });
                }
                Err(e) => out.text.push_str(&format!("\nexport failed: {e}")),
            }
        }
        if let Some(planes) = args.get("sections").and_then(Value::as_array) {
            let mut all = Vec::new();
            for plane in planes {
                let mut a = self.sub_args(args);
                a["section"] = plane.clone();
                // The section alone: measure's text would repeat the
                // model's warnings once per plane.
                let r = self.measure(id, &a)?;
                let s = &r.structured;
                let (line, sec) = match (s.get("section"), s["error"].as_str()) {
                    (Some(sec), _) if sec.is_object() && sec["contours"] != 0 => {
                        (section_lines(sec), sec.clone())
                    }
                    (_, Some(e)) => (
                        format!("section {}: {e}", plane.as_str().unwrap_or("?")),
                        json!({"plane": plane, "error": e}),
                    ),
                    _ => (
                        format!("section {}: nothing to cut", plane.as_str().unwrap_or("?")),
                        json!({"plane": plane, "area": 0}),
                    ),
                };
                out.text.push('\n');
                out.text.push_str(&line);
                all.push(sec);
            }
            out.structured["sections"] = Value::Array(all);
        }
        Ok(out)
    }

    /// The model arguments of `args`, for a call [`Tools::check`] makes on
    /// the same model.
    fn sub_args(&self, args: &Value) -> Value {
        let mut a = json!({});
        for k in ["path", "source", "base_dir", "parts"] {
            if let Some(v) = args.get(k) {
                a[k] = v.clone();
            }
        }
        a
    }

    /// The plain check, and whether the model rendered.
    fn check_only(&self, id: &Value, args: &Value) -> Result<(Out, bool), String> {
        let m = self.model(args, INLINE)?;
        let main = m.path.clone();
        let imported = m.label.clone();
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
            return Ok((
                label(&imported, finish(args, text, terse_log(&log, &main), r)),
                false,
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
        for sk in r["sketches"].as_array().into_iter().flatten() {
            text.push('\n');
            text.push_str(&session::sketches::line_text(sk));
        }
        let log =
            json!({"diagnostics": r["diagnostics"]["items"], "echo": r["diagnostics"]["echo"]});
        push_log(&mut text, &log);
        // The model's own warnings and echo go in the structured content
        // too, not only the text: Claude Code shows an agent the structured
        // content in place of the text, so a `check` without them read as
        // warning-free, and agents ran `evaluate` or `render` as well after
        // every edit to see them.
        let mut s = json!({
            "exit_code": r["exit_code"],
            "ok": r["ok"],
            "counts": r["counts"],
            "model": model,
            "findings": terse_findings(&r["findings"]),
            "truncated": r["truncated"],
            "diagnostics": terse_diags(&log["diagnostics"], &main),
        });
        put_echo(&mut s, &log["echo"]);
        if let Some(sk) = terse_sketches(&r["sketches"], &main) {
            s["sketches"] = sk;
        }
        Ok((label(&imported, finish(args, text, s, r)), true))
    }

    fn measure(&self, id: &Value, args: &Value) -> Reply {
        let m = self.model(args, INLINE)?;
        let main = m.path.clone();
        let imported = m.label.clone();
        let mut p = self.params(&m, args);
        for k in [
            "part", "between", "section", "axis", "center", "profile", "sketch",
        ] {
            if let Some(v) = args.get(k) {
                p[k] = v.clone();
            }
        }
        // Asked for a section, a profile or a distance, the answer is that:
        // the model's own numbers (which `render` gives) are left out.
        let focused = ["between", "section", "profile", "sketch"]
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
            if let Some(sk) = r.get("sketch").filter(|v| v.is_object()) {
                lines.push(session::measure::sketch_text(sk).trim_end().to_string());
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
        put_echo(&mut s, &log["echo"]);
        if let Some(sk) = s.get("sketch").filter(|v| v.is_object()).cloned() {
            s["sketch"] = terse_place(&sk, &main);
        }
        if let Some(e) = s.get("error").and_then(Value::as_str) {
            s["error"] = json!(crate::serve::param_names(e));
        }
        Ok(label(&imported, finish(args, text, s, r)))
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
            if let Some(b) = &self.browser {
                return self.format_page(
                    &Surface::Page(b),
                    id,
                    &base,
                    check,
                    bool_arg(args, "diff"),
                );
            }
            if let Some(apps) = self.apps.as_ref().filter(|a| a.connected()) {
                let doc = apps.target(None)?;
                return self.format_page(
                    &Surface::App(apps, doc),
                    id,
                    &base,
                    check,
                    bool_arg(args, "diff"),
                );
            }
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
        let name = str_arg(args, "name");
        let r = self.run(
            id,
            "docs",
            &json!({"name": name, "file": file, "cwd": base,
                    "full": bool_arg(args, "full"), "brief": !bool_arg(args, "verbose"),
                    "file_arg": "`path` (the file that defines or includes it)"}),
        )?;
        let mut text = r["text"].as_str().unwrap_or("").trim_end().to_string();
        match name {
            // The index lists the recipes too: the instructions show them,
            // and an agent that asks for the index is looking for what to
            // ask about next.
            None => {
                text.push_str(&format!(
                    "\nPrinting recipes (ask for one by name): {}",
                    super::recipes::names()
                ));
                if self.sketch_on() {
                    text.push_str("\nConstrained sketches are on: `docs` for sketch has a recipe.");
                }
            }
            // `sketch` with the extension on: its reference, then a
            // whole sketch to adapt, which agents otherwise assemble
            // entity by entity from the vocabulary's entries.
            Some("sketch") if r["exit_code"] == 0 && self.sketch_on() => {
                text.push_str("\n\nRecipe (tested; adapt the numbers):\n");
                text.push_str(super::SKETCH_RECIPE.trim_end());
            }
            // Builtins and the file's own definitions come first (a model
            // may define its own `thread`); a name they don't know may be
            // a recipe the agent read in the instructions. (`entries` is
            // there when the name was not found, not when `path` could
            // not be read, an error the agent still needs to see.)
            Some(n) if r["exit_code"] != 0 && r["entries"].is_array() => {
                if let Some((how, found)) = super::recipes::find(n) {
                    text = super::recipes::answer(n, how, &found)
                        .trim_end()
                        .to_string();
                }
            }
            Some(_) => {}
        }
        Ok(Out {
            text,
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

/// What a written mesh file holds, read back from the disk as a slicer
/// would read it: its triangles, whether every edge joins exactly two of
/// them in opposite directions (watertight), and its height range. Null
/// for formats that are not meshes, or a file that cannot be read.
///
/// After every export, agents checked the file with Bash (`ls`, `head`,
/// `grep -c "facet normal"`, an `awk` for the lowest z), a turn each
/// time: the render's numbers are the model's, and only the file shows
/// what was written. So the answer comes with the export. The vertices
/// are matched as the file has them (an STL's 32-bit floats), so faces
/// that collapse at that precision show here as open edges.
fn read_back(path: &Path) -> Value {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let Ok(bytes) = std::fs::read(path) else {
        return Value::Null;
    };
    let mut msgs = Vec::new();
    let meshes = match ext.as_str() {
        "stl" => vec![io::stl::read(&bytes, "", &mut msgs)],
        "obj" => vec![io::obj::read(&bytes, "", &mut msgs)],
        "off" => vec![io::off::read(Some(&bytes), "", &mut msgs)],
        "3mf" => io::threemf::read(Some(&bytes), "", 0, &mut msgs),
        _ => return Value::Null,
    };
    let (mut triangles, mut open, mut over) = (0usize, 0usize, 0usize);
    let (mut zmin, mut zmax) = (f64::INFINITY, f64::NEG_INFINITY);
    for m in &meshes {
        // Directed edges, each face's in its winding: a closed, consistently
        // oriented surface has each one once, and its reverse once.
        let mut edges: std::collections::HashMap<(u32, u32), u32> =
            std::collections::HashMap::new();
        for f in &m.faces {
            triangles += f.len().saturating_sub(2);
            for k in 0..f.len() {
                let (a, b) = (f[k], f[(k + 1) % f.len()]);
                if a != b {
                    *edges.entry((a, b)).or_insert(0) += 1;
                }
            }
            for &v in f {
                if let Some(p) = m.vertices.get(v as usize) {
                    zmin = zmin.min(p[2]);
                    zmax = zmax.max(p[2]);
                }
            }
        }
        for (&(a, b), &n) in &edges {
            let back = edges.get(&(b, a)).copied().unwrap_or(0);
            if n > 1 || back > 1 {
                over += 1;
            } else if back == 0 {
                open += 1;
            }
        }
    }
    if triangles == 0 {
        return json!({"triangles": 0, "watertight": false});
    }
    let mut v = json!({
        "triangles": triangles,
        "watertight": open == 0 && over == 0,
        "z": [zmin, zmax],
    });
    if open > 0 {
        v["open_edges"] = json!(open);
    }
    if over > 0 {
        v["shared_edges"] = json!(over);
    }
    v
}

/// [`read_back`]'s result as words: "read back: 1240 triangles,
/// watertight, z 0 to 25".
fn read_back_text(v: &Value) -> String {
    let n = v["triangles"].as_u64().unwrap_or(0);
    if n == 0 {
        return "read back: no triangles in the file".into();
    }
    let tight = if v["watertight"] == true {
        "watertight".to_string()
    } else {
        let mut why = Vec::new();
        if let Some(o) = v["open_edges"].as_u64() {
            why.push(format!("{o} open edges"));
        }
        if let Some(o) = v["shared_edges"].as_u64() {
            why.push(format!("{o} edges shared by more than two faces"));
        }
        format!("NOT watertight ({})", why.join(", "))
    };
    format!(
        "read back: {n} triangles, {tight}, z {} to {}",
        num_of(&v["z"][0]),
        num_of(&v["z"][1])
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

/// What a result says about where its model came from, when that is not
/// simply the `path` or `source` given.
#[derive(Debug, Clone, Default)]
enum Label {
    #[default]
    None,
    /// A mesh `path`, rendered as this `import()`.
    Import(String),
    /// The connected web page's text (no `path` or `source`), at this
    /// version: the agent needs the version for `editor_edit`, and should
    /// never mistake the page's text for a file of its own. With a
    /// `number`, a connected app's document of that number.
    Page {
        file: String,
        version: u64,
        number: Option<u64>,
    },
}

/// A result labelled with where its model came from: first in the text,
/// and in the structured content (the full JSON's too, with `verbose`).
/// A mesh `path` is labelled with the `import()` it was rendered as.
fn label(l: &Label, mut out: Out) -> Out {
    match l {
        Label::None => {}
        Label::Import(call) => {
            out.text = format!("path is a mesh file: rendered as `{call}`\n{}", out.text);
            if let Some(o) = out.structured.as_object_mut() {
                o.insert("imported".into(), json!(call));
            }
        }
        Label::Page {
            file,
            version,
            number: None,
        } => {
            out.text = format!("the web page's {file} (version {version})\n{}", out.text);
            if let Some(o) = out.structured.as_object_mut() {
                o.insert("page".into(), json!({"file": file, "version": version}));
            }
        }
        Label::Page {
            file,
            version,
            number: Some(n),
        } => {
            out.text = format!(
                "{file} in NeoSCAD (document {n}, version {version})\n{}",
                out.text
            );
            if let Some(o) = out.structured.as_object_mut() {
                o.insert(
                    "document".into(),
                    json!({"number": n, "file": file, "version": version}),
                );
            }
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
fn check_args(tools: &[Value], name: &str, args: &Value) -> Result<(), String> {
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
        "integer" => v.is_u64(),
        "array" => v.is_array(),
        "object" => v.is_object(),
        _ => true,
    };
    let a = |ty: &str| match ty {
        "array" | "object" | "integer" => format!("an {ty}"),
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
/// the parts) is wrong there, and an agent that follows it goes in
/// circles. So when the diagnostics report such a polyhedron, the fix
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

/// A sketch (or one of its entities) as an agent reads it: the line, and
/// the file only when it is not the model; no byte spans.
fn terse_place(v: &Value, main: &Path) -> Value {
    let mut t = v.clone();
    if let Some(o) = t.as_object_mut() {
        o.remove("span");
        match o.get("file").and_then(Value::as_str).map(str::to_string) {
            Some(f) if Path::new(&f) != main => {
                o.insert("file".into(), json!(short(&f)));
            }
            _ => {
                o.remove("file");
            }
        }
        if let Some(Value::Array(es)) = o.get_mut("entities") {
            for e in es {
                *e = terse_place(e, main);
            }
        }
    }
    t
}

/// `check`'s sketches as an agent reads them: name, state, degrees of
/// freedom, the codes of their diagnostics (which come in full under
/// `diagnostics`) and where they are.
fn terse_sketches(v: &Value, main: &Path) -> Option<Value> {
    let list = v.as_array().filter(|l| !l.is_empty())?;
    Some(Value::Array(
        list.iter()
            .take(MAX_LINES)
            .map(|s| {
                let t = terse_place(s, main);
                let mut o = json!({});
                for k in ["name", "status", "dof", "unknowns", "codes", "file", "line"] {
                    if let Some(x) = t.get(k).filter(|x| !x.is_null()) {
                        o[k] = x.clone();
                    }
                }
                o
            })
            .collect(),
    ))
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

/// The echo lines (at most [`MAX_LINES`]) in a terse result that builds
/// geometry. Left out when there are none, so a model without `echo()`
/// pays nothing for it in every `check`, `snapshot` and `measure`.
fn put_echo(s: &mut Value, echo: &Value) {
    if let Some(e) = echo.as_array().filter(|e| !e.is_empty()) {
        s["echo"] = json!(e.iter().take(MAX_LINES).cloned().collect::<Vec<_>>());
    }
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
