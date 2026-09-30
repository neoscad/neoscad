//! The whole pipeline (parse, evaluate, render) with nothing from the host
//! machine: files come from a [`MemFs`], fonts and MCAD from
//! `neoscad-assets`, the `rands()` seed from the caller. Built for
//! wasm32-unknown-unknown and run in node by `scripts/wasm-check.sh`
//! (`run.js`); the native tests run the same cases (`cases.json`) through
//! the same [`run`], so a difference between the two is a WASM problem.
//!
//! The document is `/doc/main.scad` and the working directory `/doc`; the
//! bundled libraries are mounted at `/neoscad/libraries`.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use lang::loader::{FileSystem, LibraryPath};
use lang::vfs::MemFs;

const DOC_DIR: &str = "/doc";
const LIBRARY_DIR: &str = "/neoscad/libraries";

/// Run `src` as `/doc/main.scad` over `files` and return what the command
/// line would print (messages, then the geometry summary), one line each.
pub fn run(files: Arc<MemFs>, src: &[u8], seed: u32) -> String {
    run_with(
        files,
        src,
        seed,
        eval::recursion::DEFAULT_FRAME_LIMIT,
        false,
    )
}

/// [`run`] with another frame budget (for calibrating the default), and
/// with `preview`, OpenSCAD's preview instead of the render: the CSG
/// products, their booleans and the preview scene.
pub fn run_with(
    files: Arc<MemFs>,
    src: &[u8],
    seed: u32,
    frame_limit: u32,
    preview: bool,
) -> String {
    let base: Arc<dyn FileSystem + Send + Sync> = files;
    let fs: Arc<dyn FileSystem + Send + Sync> = Arc::new(assets::libraries(base, LIBRARY_DIR));
    let libs = LibraryPath(vec![PathBuf::from(LIBRARY_DIR)]);
    let doc = PathBuf::from(DOC_DIR);

    let mut text = src.to_vec();
    text.extend_from_slice(b"\n\x03\n");
    let program = lang::parse_program(doc.join("main.scad"), text, &*fs, &libs);
    let mut out: Vec<u8> = Vec::new();
    let mut con = eval::Console::new(&mut out, doc.clone(), false);
    for d in program.openscad_diags() {
        con.diagnostic(d, &program.sources, &doc);
    }
    if program.has_syntax_errors() {
        drop(con);
        return String::from_utf8_lossy(&out).into_owned();
    }
    let libraries = lang::deps::load_dependencies(&program, b"\n\x03\n", &*fs, &libs);
    let uses = lang::deps::resolve_uses(&program, &*fs, &libs);
    let elibs: Vec<eval::Library<'_>> = libraries
        .iter()
        .map(|l| eval::Library {
            path: &l.path,
            program: l.program.as_ref(),
            uses: &l.uses,
        })
        .collect();
    let opts = eval::Options {
        rng_seed: seed,
        frame_limit,
        fs: fs.clone(),
        preview,
        ..Default::default()
    };
    let ev = eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
        eval::evaluate(&program, &uses, &elibs, doc.clone(), &opts, &mut con)
    });
    let keys = eval::dump::Keys::new(&ev.root, &*fs);
    let mut fonts = text::FontDb::with_fs(fs.clone());
    assets::add_fonts(&mut fonts);
    let ro = geom::RenderOptions {
        fs: fs.clone(),
        work_dir: doc.clone(),
        fonts: Arc::new(fonts),
        ..Default::default()
    };
    let top = ev.root.find_root_tag().0.unwrap_or(&ev.root);
    if preview {
        let limit = geom::csg::DEFAULT_TERM_LIMIT;
        match geom::csg::CsgTree::build(top, &geom::Renderer::new(), &keys, ro, limit) {
            Err(u) => con.print(None, format!("{}() is not implemented", u.what).as_bytes()),
            Ok(t) => {
                for m in &t.messages {
                    let label = m.severity.map_or("", |s| s.openscad_label());
                    let sep = if label.is_empty() { "" } else { ": " };
                    con.print(m.severity, format!("{label}{sep}{}", m.text).as_bytes());
                }
                con.print(None, preview_line(&t).as_bytes());
            }
        }
        drop(con);
        return String::from_utf8_lossy(&out).into_owned();
    }
    match geom::Renderer::new().render(top, &keys, ro) {
        Err(u) => con.print(None, format!("{}() is not implemented", u.what).as_bytes()),
        Ok(r) => {
            for m in &r.messages {
                let label = m.severity.map_or("", |s| s.openscad_label());
                let sep = if label.is_empty() { "" } else { ": " };
                con.print(m.severity, format!("{label}{sep}{}", m.text).as_bytes());
            }
            match r.geometry.as_ref().filter(|g| !g.is_empty()) {
                Some(g) => {
                    for l in geom::export::summary(g) {
                        con.print(None, l.as_bytes());
                    }
                    con.print(None, scene_line(g).as_bytes());
                }
                None => con.print(None, b"Current top level object is empty."),
            }
        }
    }
    drop(con);
    String::from_utf8_lossy(&out).into_owned()
}

/// Separates the versions of the document in a session case's source.
pub const EDIT_MARK: &str = "\n//--edit--\n";

/// The session, as an app or a web worker drives it: `src` holds versions
/// of `/doc/main.scad` separated by [`EDIT_MARK`]; each is sent as an
/// edit to the open document and rendered, and the lines report the
/// messages, the geometry and how much the caches answered. On wasm32 the
/// session runs every request synchronously on the calling thread.
pub fn run_session(files: Arc<MemFs>, src: &[u8]) -> String {
    let base: Arc<dyn FileSystem + Send + Sync> = files;
    let fs: Arc<dyn FileSystem + Send + Sync> = Arc::new(assets::libraries(base, LIBRARY_DIR));
    let mut cfg = session::Config::new(fs, LibraryPath(vec![PathBuf::from(LIBRARY_DIR)]));
    cfg.work_dir = PathBuf::from(DOC_DIR);
    cfg.fonts = Arc::new(|_used: &[String]| {
        let mut db = text::FontDb::new();
        assets::add_fonts(&mut db);
        db
    });
    // The web app runs agents' and users' models as the native app does,
    // under the agent limits (no clock here, so no time limit).
    cfg.limits = session::Limits::AGENT;
    let s = session::Session::new(cfg);
    let scheme = render::ColorScheme::cornfield();
    let doc = std::path::Path::new("main.scad");
    let mut out = String::new();
    let text = String::from_utf8_lossy(src);
    for (i, version) in text.split(EDIT_MARK).enumerate() {
        s.update(doc, version.as_bytes().to_vec());
        let before = s.stats().geometry;
        let r = match s.render(
            &session::Run::new("main.scad"),
            session::Mode::Render,
            &scheme,
        ) {
            Ok(r) => r,
            Err(c) => {
                out.push_str(&format!("{c}\n"));
                continue;
            }
        };
        out.push_str(&String::from_utf8_lossy(&r.log.stderr));
        let g = r.geometry_json(&scheme.geometry_scheme());
        let after = s.stats().geometry;
        out.push_str(&format!(
            "Session {}: exit {}, volume {:.3}, {} triangles, {} nodes built, {} from the cache\n",
            i + 1,
            r.exit_code,
            g["volume"].as_f64().unwrap_or(0.0),
            g["triangles"].as_u64().unwrap_or(0),
            after.misses - before.misses,
            after.hits - before.hits,
        ));
    }
    out
}

/// `check` and `measure` with named parts, as an agent's web worker runs
/// them: `src` is `/doc/main.scad`, rendered with `part()` on; the lines
/// report the check's findings (code, severity, part, value) and each
/// part's measured volume.
pub fn run_check(files: Arc<MemFs>, src: &[u8]) -> String {
    let base: Arc<dyn FileSystem + Send + Sync> = files;
    let fs: Arc<dyn FileSystem + Send + Sync> = Arc::new(assets::libraries(base, LIBRARY_DIR));
    let mut cfg = session::Config::new(fs, LibraryPath(vec![PathBuf::from(LIBRARY_DIR)]));
    cfg.work_dir = PathBuf::from(DOC_DIR);
    cfg.parts = true;
    let s = session::Session::new(cfg);
    s.update(std::path::Path::new("main.scad"), src.to_vec());
    let mut out = String::new();
    let req = session::check::CheckRequest {
        run: session::Run::new("main.scad"),
        settings: session::check::CheckSettings::default(),
    };
    let c = match s.check(&req) {
        Ok(c) => c,
        Err(e) => return format!("{e}\n"),
    };
    out.push_str(&String::from_utf8_lossy(&c.log.stderr));
    let v = &c.summary;
    out.push_str(&format!(
        "Check: exit {}, {} errors, {} warnings, {} components\n",
        c.exit_code, v["counts"]["errors"], v["counts"]["warnings"], v["model"]["components"]
    ));
    for f in v["findings"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "Finding: {} {} {} {}\n",
            f["code"].as_str().unwrap_or(""),
            f["severity"].as_str().unwrap_or(""),
            f["part"].as_str().unwrap_or("-"),
            f["value"]
        ));
    }
    let m = match s.measure(&session::measure::MeasureRequest::new(session::Run::new(
        "main.scad",
    ))) {
        Ok(m) => m,
        Err(e) => return format!("{out}{e}\n"),
    };
    for p in m.summary["parts"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "Part {}: volume {:.3}\n",
            p["name"].as_str().unwrap_or(""),
            p["volume"].as_f64().unwrap_or(0.0)
        ));
    }
    out
}

/// `neoscad fmt` and `neoscad test` as a web worker runs them: with
/// `test` false, `src` (an open document) formatted, then its formatted
/// text formatted again (it must not change); with `test` true, `src` is
/// `/doc/main_test.scad` and its tests run (in turn: wasm32 has no
/// threads), each reported as `Test <id>: ok|FAILED`.
pub fn run_tooling(files: Arc<MemFs>, src: &[u8], test: bool) -> String {
    let base: Arc<dyn FileSystem + Send + Sync> = files;
    let fs: Arc<dyn FileSystem + Send + Sync> = Arc::new(assets::libraries(base, LIBRARY_DIR));
    let mut cfg = session::Config::new(fs, LibraryPath(vec![PathBuf::from(LIBRARY_DIR)]));
    cfg.work_dir = PathBuf::from(DOC_DIR);
    let s = session::Session::new(cfg);
    let mut out = String::new();
    if test {
        s.update(std::path::Path::new("main_test.scad"), src.to_vec());
        let req = session::modeltest::TestRequest {
            paths: vec!["main_test.scad".into()],
            jobs: 4,
            ..Default::default()
        };
        let r = match s.test(&req) {
            Ok(r) => r,
            Err(e) => return format!("{e}\n"),
        };
        for t in r.json["tests"].as_array().into_iter().flatten() {
            let ok = t["ok"] == serde_json::json!(true);
            out.push_str(&format!(
                "Test {}: {}\n",
                t["id"].as_str().unwrap_or(""),
                if ok { "ok" } else { "FAILED" }
            ));
            for f in t["failures"].as_array().into_iter().flatten() {
                out.push_str(&format!("  {}\n", f["message"].as_str().unwrap_or("")));
            }
        }
        out.push_str(&format!("Tests: exit {}\n", r.exit_code));
        return out;
    }
    s.update(std::path::Path::new("main.scad"), src.to_vec());
    let req = session::format::FormatRequest {
        input: Some("main.scad".into()),
        ..Default::default()
    };
    let f = s.format(&req);
    let text = match &f.result {
        Ok(t) => t.clone(),
        Err(e) => return format!("Format: {e}\n"),
    };
    out.push_str(&String::from_utf8_lossy(&text));
    let again = s.format(&session::format::FormatRequest {
        text: Some(text.clone()),
        ..req
    });
    out.push_str(&format!(
        "Format: changed {}, idempotent {}\n",
        f.changed(),
        again.result.as_ref().is_ok_and(|t| *t == text)
    ));
    out
}

/// The language server as a web worker would run it: `src` is
/// `/doc/main.scad` with three markers removed first, `^` where to hover,
/// `@` where to go to the definition and `|` where to complete. Prints
/// the hover's signature line, the definition's file (under the library
/// directory), the first completion labels, and the published
/// diagnostics (code, line and column).
pub fn run_lsp(files: Arc<MemFs>, src: &[u8]) -> String {
    let base: Arc<dyn FileSystem + Send + Sync> = files;
    let fs: Arc<dyn FileSystem + Send + Sync> = Arc::new(assets::libraries(base, LIBRARY_DIR));
    let mut cfg = session::Config::new(fs, LibraryPath(vec![PathBuf::from(LIBRARY_DIR)]));
    cfg.work_dir = PathBuf::from(DOC_DIR);
    let s = session::Session::new(cfg);
    let server = lsp::Server::new(lsp::Options {
        sync_session: true,
        limits: None,
        host_diagnostics: false,
    });
    let text = String::from_utf8_lossy(src).into_owned();
    // Each marker's position in the text without the markers (ASCII
    // texts: characters are UTF-16 units).
    let mut clean = String::new();
    let mut at = std::collections::HashMap::new();
    let (mut line, mut col) = (0u32, 0u32);
    for ch in text.chars() {
        if matches!(ch, '^' | '@' | '|') {
            at.insert(ch, serde_json::json!({"line": line, "character": col}));
            continue;
        }
        clean.push(ch);
        if ch == '\n' {
            line += 1;
            col = 0;
        } else {
            col += ch.len_utf16() as u32;
        }
    }
    let uri = "file:///doc/main.scad";
    let mut id = 0;
    let mut ask = |method: &str, params: serde_json::Value| -> serde_json::Value {
        id += 1;
        let msg =
            serde_json::json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let out = server.handle(&s, &msg.to_string());
        out.first()
            .and_then(|m| serde_json::from_str::<serde_json::Value>(m).ok())
            .map_or(serde_json::Value::Null, |v| v["result"].clone())
    };
    ask("initialize", serde_json::json!({"capabilities": {}}));
    server.handle(
        &s,
        &serde_json::json!({"jsonrpc": "2.0", "method": "textDocument/didOpen", "params": {
            "textDocument": {"uri": uri, "languageId": "openscad", "version": 1, "text": clean}}})
        .to_string(),
    );
    let mut out = String::new();
    let pos = |c: char| at.get(&c).cloned().unwrap_or(serde_json::Value::Null);
    let h = ask(
        "textDocument/hover",
        serde_json::json!({"textDocument": {"uri": uri}, "position": pos('^')}),
    );
    let sig = h["contents"]["value"]
        .as_str()
        .and_then(|v| v.lines().nth(1))
        .unwrap_or("none");
    out.push_str(&format!("Hover: {sig}\n"));
    let d = ask(
        "textDocument/definition",
        serde_json::json!({"textDocument": {"uri": uri}, "position": pos('@')}),
    );
    let target = d["uri"].as_str().unwrap_or("none");
    let target = target
        .strip_prefix(&format!("file://{LIBRARY_DIR}/"))
        .unwrap_or(target);
    out.push_str(&format!(
        "Definition: {target} line {}\n",
        d["range"]["start"]["line"]
    ));
    let c = ask(
        "textDocument/completion",
        serde_json::json!({"textDocument": {"uri": uri}, "position": pos('|')}),
    );
    let labels: Vec<&str> = c["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| i["label"].as_str())
        .take(3)
        .collect();
    out.push_str(&format!("Completion: {}\n", labels.join(" ")));
    for m in server.publish_diagnostics(&s) {
        let v: serde_json::Value = serde_json::from_str(&m).unwrap_or_default();
        for d in v["params"]["diagnostics"].as_array().into_iter().flatten() {
            out.push_str(&format!(
                "Diagnostic: {} {}:{}\n",
                d["code"].as_str().unwrap_or(""),
                d["range"]["start"]["line"],
                d["range"]["start"]["character"]
            ));
        }
    }
    out
}

/// What the renderer would draw, without a GPU: the scene's triangles and
/// outline segments, and the viewer distance `--viewall` fits (the
/// default camera's). This runs the renderer's CPU side (scene building,
/// colour schemes, camera maths) on wasm32.
fn scene_line(g: &geom::Geometry) -> String {
    let scheme = render::ColorScheme::cornfield();
    let scene = render::Scene::new(Some(g), &scheme);
    let mut camera = render::Camera {
        viewall: true,
        autocenter: true,
        ..Default::default()
    };
    render::fit_camera(&mut camera, &scene);
    format!(
        "Scene: {} triangles, {} outline segments, viewall distance {:.4}",
        scene.face_vertex_count() / 3,
        scene.edge_segment_count(),
        camera.viewer_distance
    )
}

/// What the preview would draw: products, the triangles of its scene
/// (booleans included) and the distance `--viewall` fits.
fn preview_line(t: &geom::csg::CsgTree) -> String {
    let scheme = render::ColorScheme::cornfield();
    let scene = render::preview::scene(t, &scheme, render::Previewer::OpenCsg);
    let mut camera = render::Camera {
        viewall: true,
        autocenter: true,
        ..Default::default()
    };
    render::fit_camera(&mut camera, &scene);
    let count = |p: &Option<geom::csg::Products>| p.as_ref().map_or(0, |p| p.products.len());
    format!(
        "Preview: {} products, {} highlighted, {} background, {} triangles, viewall distance {:.4}",
        count(&t.root),
        count(&t.highlights),
        count(&t.background),
        scene.face_vertex_count() / 3,
        camera.viewer_distance
    )
}

// --- The module's interface to JavaScript ---------------------------------
//
// JavaScript writes into `INPUT` (sized by `input`), then calls `add_file`
// or `run_input`; the result is read from `OUTPUT`. All of it is safe Rust: the
// exports hand out pointers into vectors they own and never read through
// raw pointers.

static INPUT: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static OUTPUT: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static FILES: Mutex<Option<Arc<MemFs>>> = Mutex::new(None);

/// Size the input buffer to `len` bytes and return where to write them.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn input(len: usize) -> *mut u8 {
    let mut b = INPUT.lock().expect("input");
    b.clear();
    b.resize(len, 0);
    b.as_mut_ptr()
}

/// Add a file: the input holds its absolute path (`name_len` bytes), then
/// its contents.
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn add_file(name_len: usize) {
    let b = INPUT.lock().expect("input");
    let (name, data) = b.split_at(name_len.min(b.len()));
    let name = String::from_utf8_lossy(name).into_owned();
    FILES
        .lock()
        .expect("files")
        .get_or_insert_with(Default::default)
        .insert(name, data.to_vec());
}

/// Run the input as the main file with `seed` for unseeded `rands()`,
/// `frame_limit` as the frame budget (0 for the default), and as a preview
/// when `preview` is 1; with `preview` 2, as a session case
/// ([`run_session`]); with 3, as a check case ([`run_check`]); with 4
/// formatted and with 5 as a test file ([`run_tooling`]); with 6 through
/// the language server ([`run_lsp`]).
#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn run_input(seed: u32, frame_limit: u32, preview: u32) {
    // A panic aborts the module (wasm32-unknown-unknown cannot unwind);
    // leave its message where the caller reads the output.
    std::panic::set_hook(Box::new(|info| {
        if let Ok(mut out) = OUTPUT.try_lock() {
            *out = format!("PANIC: {info}\n").into_bytes();
        }
    }));
    let src = INPUT.lock().expect("input").clone();
    let files = FILES
        .lock()
        .expect("files")
        .get_or_insert_with(Default::default)
        .clone();
    let limit = match frame_limit {
        0 => eval::recursion::DEFAULT_FRAME_LIMIT,
        n => n,
    };
    OUTPUT.lock().expect("output").clear();
    let out = if preview == 6 {
        run_lsp(files, &src)
    } else if preview == 4 || preview == 5 {
        run_tooling(files, &src, preview == 5)
    } else if preview == 3 {
        run_check(files, &src)
    } else if preview == 2 {
        run_session(files, &src)
    } else {
        run_with(files, &src, seed, limit, preview != 0)
    };
    *OUTPUT.lock().expect("output") = out.into_bytes();
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn output_ptr() -> *const u8 {
    OUTPUT.lock().expect("output").as_ptr()
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
pub extern "C" fn output_len() -> usize {
    OUTPUT.lock().expect("output").len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// `line` against `pattern`, where `*` stands for any text (the
    /// recursion traces name a depth, which differs between targets).
    /// `run.js` matches the same way.
    fn matches(line: &str, pattern: &str) -> bool {
        let parts: Vec<&str> = pattern.split('*').collect();
        if parts.len() == 1 {
            return line == pattern;
        }
        let (first, last) = (parts[0], parts[parts.len() - 1]);
        if !line.starts_with(first) || !line[first.len()..].ends_with(last) {
            return false;
        }
        let mut rest = &line[first.len()..line.len() - last.len()];
        for p in &parts[1..parts.len() - 1] {
            match rest.find(p) {
                Some(i) => rest = &rest[i + p.len()..],
                None => return false,
            }
        }
        true
    }

    #[test]
    fn linked_stack_matches_the_evaluator() {
        let linked: usize = env!("WASM_CHECK_STACK_SIZE").parse().unwrap();
        assert_eq!(linked, eval::recursion::WASM_STACK_SIZE);
    }

    /// Every case in `cases.json` natively; `run.js` checks the same
    /// expectations in node.
    #[test]
    fn cases() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("cases.json");
        let cases: serde_json::Value =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        for c in cases.as_array().unwrap() {
            let name = c["name"].as_str().unwrap();
            let files = Arc::new(MemFs::new());
            let mut missing = false;
            for f in c["files"].as_array().into_iter().flatten() {
                let data = match (f.get("text"), f.get("from")) {
                    (Some(t), _) => t.as_str().unwrap().as_bytes().to_vec(),
                    // Files from the reference checkout: the case is
                    // skipped without it.
                    (_, Some(p)) => match std::fs::read(root.join(p.as_str().unwrap())) {
                        Ok(d) => d,
                        Err(_) => {
                            missing = true;
                            break;
                        }
                    },
                    _ => panic!("{name}: a file needs text or from"),
                };
                files.insert(f["path"].as_str().unwrap(), data);
            }
            if missing {
                eprintln!("skipped {name}: no reference checkout");
                continue;
            }
            let seed = c["seed"].as_u64().unwrap_or(0) as u32;
            let preview = c["preview"].as_bool().unwrap_or(false);
            let src = c["src"].as_str().unwrap().as_bytes();
            let out = if c["session"] == "lsp" {
                run_lsp(files, src)
            } else if c["session"] == "fmt" || c["session"] == "test" {
                run_tooling(files, src, c["session"] == "test")
            } else if c["session"] == "check" {
                run_check(files, src)
            } else if c["session"].as_bool().unwrap_or(false) {
                run_session(files, src)
            } else {
                run_with(
                    files,
                    src,
                    seed,
                    eval::recursion::DEFAULT_FRAME_LIMIT,
                    preview,
                )
            };
            let expect: Vec<&str> = c["expect"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| l.as_str().unwrap())
                .collect();
            let got: Vec<&str> = out.lines().collect();
            // `WASM_CHECK_PRINT=1 cargo test -p neoscad-wasm-check -- --nocapture`
            // prints every case's lines instead, for writing expectations.
            if std::env::var_os("WASM_CHECK_PRINT").is_some() {
                println!("{name}: {}", serde_json::to_string(&got).unwrap());
                continue;
            }
            assert!(
                got.len() == expect.len() && got.iter().zip(&expect).all(|(g, e)| matches(g, e)),
                "case {name}:\n got {got:#?}\n expected {expect:#?}"
            );
        }
    }
}
