//! Incremental evaluation ([`eval::evaluate_incremental`]) must be
//! indistinguishable from a full evaluation. Every check here evaluates a
//! program twice, once through a memo that has seen earlier versions of it
//! and once from nothing, and compares everything a host can see: the node
//! tree (with node indices and source positions), the `.csg` export, the
//! top's geometry key, every message with its location, the console's
//! bytes, and the evaluation's flags and camera.
//!
//! The targeted tests cover the semantics that make reuse subtle (hoisting,
//! `$` variables, message order, `rands()`, includes, deprecations, moved
//! statements). `random_edits` applies random small edits to real models;
//! by default it runs a short corpus, and `NEOSCAD_REUSE_CORPUS=full` (in a
//! release build) runs BOSL2's examples and tests, OpenSCAD's test inputs
//! and this repository's examples, with `NEOSCAD_REUSE_EDITS` edits per
//! file (see the function).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eval::{Memo, Options};
use lang::Program;
use lang::fragment::{Fragment, FragmentCache, FragmentKey};
use lang::loader::{LexCache, LexedFile, LibraryPath, Metadata, StdFs};

// --- parsing, with the caches a host keeps ----------------------------------

#[derive(Default)]
struct Lexed(Mutex<HashMap<PathBuf, (Metadata, Arc<LexedFile>)>>);

impl LexCache for Lexed {
    fn get(&self, path: &Path, meta: &Metadata) -> Option<Arc<LexedFile>> {
        let m = self.0.lock().unwrap();
        m.get(path)
            .filter(|(k, _)| k == meta)
            .map(|(_, f)| f.clone())
    }
    fn put(&self, path: &Path, meta: Metadata, file: Arc<LexedFile>) {
        self.0
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), (meta, file));
    }
}

#[derive(Default)]
struct Frags(Mutex<HashMap<FragmentKey, Arc<Fragment>>>);

impl FragmentCache for Frags {
    fn get(&self, key: &FragmentKey) -> Option<Arc<Fragment>> {
        self.0.lock().unwrap().get(key).cloned()
    }
    fn put(&self, key: FragmentKey, fragment: Arc<Fragment>) {
        self.0.lock().unwrap().insert(key, fragment);
    }
}

struct Host {
    fs: Arc<StdFs>,
    libs: LibraryPath,
    lexed: Lexed,
    frags: Frags,
}

struct Parsed {
    program: Program,
    libraries: Vec<lang::deps::Library>,
    uses: Vec<String>,
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

impl Host {
    fn new() -> Host {
        let r = repo();
        Host {
            fs: Arc::new(StdFs),
            libs: LibraryPath(vec![r.join(".reference"), r.join("assets/libraries")]),
            lexed: Lexed::default(),
            frags: Frags::default(),
        }
    }

    /// Drop the included files' parses. A fragment's key includes the
    /// program's main file, so across a corpus of main files an unbounded
    /// cache holds a parse of every include once per file (BOSL2's
    /// `std.scad` is tens of megabytes of syntax tree). The session's
    /// store has a budget; the harness keeps one file's worth.
    fn new_main_file(&self) {
        self.frags.0.lock().unwrap().clear();
    }

    fn parse(&self, path: &Path, text: &[u8]) -> Parsed {
        let suffix = b"\n\x03\n";
        let mut t = text.to_vec();
        t.extend_from_slice(suffix);
        let caches = lang::Caches {
            lex: Some(&self.lexed),
            fragments: Some(&self.frags),
            stats: None,
        };
        let program =
            lang::parse_program_with(path.to_path_buf(), t, &*self.fs, &self.libs, caches);
        let libraries = lang::deps::load_dependencies(&program, suffix, &*self.fs, &self.libs);
        let uses = lang::deps::resolve_uses(&program, &*self.fs, &self.libs);
        Parsed {
            program,
            libraries,
            uses,
        }
    }
}

// --- what a host can see of an evaluation -----------------------------------

/// A stream of output kept as a digest of all of it, with only its first
/// part kept as text for reporting a difference. A runaway model can print
/// millions of messages before a limit stops it, and two evaluations of it
/// held as strings took the harness to 10 GB.
struct Stream {
    digest: sha2::Sha256,
    len: usize,
    head: Vec<u8>,
}

const HEAD: usize = 256 << 10;

impl Stream {
    fn new() -> Stream {
        use sha2::Digest as _;
        Stream {
            digest: sha2::Sha256::new(),
            len: 0,
            head: Vec::new(),
        }
    }

    fn add(&mut self, b: &[u8]) {
        use sha2::Digest as _;
        self.digest.update(b);
        self.len += b.len();
        let room = HEAD.saturating_sub(self.head.len());
        self.head.extend_from_slice(&b[..room.min(b.len())]);
    }

    fn finish(self) -> Seen1 {
        use sha2::Digest as _;
        Seen1 {
            digest: self.digest.finalize().into(),
            len: self.len,
            head: String::from_utf8_lossy(&self.head).into_owned(),
        }
    }
}

impl std::io::Write for Stream {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.add(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// One finished [`Stream`].
#[derive(Debug, PartialEq)]
struct Seen1 {
    digest: [u8; 32],
    len: usize,
    head: String,
}

/// Every message twice: its tool view (with the file it points into), and
/// the console's rendering.
struct Tee {
    lines: Stream,
    console: eval::Console<Stream>,
}

impl eval::Output for Tee {
    fn message(&mut self, m: &eval::Message<'_>) {
        let file = match (m.diag.span, m.sources) {
            (Some(s), Some(src)) => src.path(s.file).display().to_string(),
            _ => String::new(),
        };
        let line = format!(
            "{:?} | {} | {}\n",
            m.diag,
            String::from_utf8_lossy(m.text),
            file
        );
        self.lines.add(line.as_bytes());
        self.console.message(m);
    }
}

#[derive(Debug, PartialEq)]
struct Seen {
    aborted: bool,
    interrupted: bool,
    tree: Seen1,
    csg: Seen1,
    key: u128,
    lines: Seen1,
    console: Seen1,
    flags: String,
}

fn evaluate(p: &Parsed, opts: &Options, memo: Option<&mut Memo>) -> (Seen, eval::ReuseStats) {
    use std::io::Write as _;
    // Each evaluation gets its own interrupt flag (and guard on it), set by
    // a watchdog after `NEOSCAD_REUSE_TIMEOUT_S`: a random edit can send a
    // model into a loop that no memory limit sees, and a time limit could
    // stop one of the two evaluations and not the other, where an
    // interrupted check is simply inconclusive (see `check_full`).
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let opts = Options {
        guard: opts
            .guard
            .as_ref()
            .map(|g| Arc::new(eval::limits::Guard::new(*g.limits(), flag.clone(), None))),
        interrupt: Some(flag.clone()),
        ..opts.clone()
    };
    let timeout = std::env::var("NEOSCAD_REUSE_TIMEOUT_S")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(20u64);
    let (done, wait) = std::sync::mpsc::channel::<()>();
    let dog = {
        let flag = flag.clone();
        std::thread::spawn(move || {
            if wait
                .recv_timeout(std::time::Duration::from_secs(timeout))
                .is_err()
            {
                flag.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        })
    };
    let opts = &opts;
    let main_dir = p
        .program
        .sources
        .path(p.program.main)
        .parent()
        .unwrap()
        .to_path_buf();
    let libs: Vec<eval::Library<'_>> = p
        .libraries
        .iter()
        .map(|l| eval::Library {
            path: &l.path,
            program: l.program.as_ref(),
            uses: &l.uses,
        })
        .collect();
    let mut out = Tee {
        lines: Stream::new(),
        console: eval::Console::new(Stream::new(), main_dir.clone(), opts.fs.clone(), false),
    };
    let ev = eval::with_stack(eval::DEFAULT_THREAD_STACK, || match memo {
        Some(m) => eval::evaluate_incremental(
            &p.program,
            &p.uses,
            &libs,
            main_dir.clone(),
            opts,
            &mut out,
            m,
        ),
        None => eval::evaluate(&p.program, &p.uses, &libs, main_dir.clone(), opts, &mut out),
    });
    let _ = done.send(());
    let _ = dog.join();
    let top = ev.root.find_root_tag().0.unwrap_or(&ev.root);
    let keys = eval::dump::Keys::new(&ev.root, &StdFs);
    // `Debug` prints numbers exactly (and NaN equal to itself).
    let mut tree = Stream::new();
    write!(tree, "{:?}", ev.root).unwrap();
    let mut csg = Stream::new();
    csg.add(eval::dump::csg(top, &main_dir, &StdFs).as_bytes());
    let seen = Seen {
        aborted: ev.aborted,
        interrupted: ev.interrupted,
        tree: tree.finish(),
        csg: csg.finish(),
        key: keys.get(top),
        lines: out.lines.finish(),
        console: out.console.into_inner().finish(),
        flags: format!(
            "aborted {} interrupted {} hard {} camera {:?} {:?}",
            ev.aborted, ev.interrupted, ev.hard_warning, ev.camera, ev.camera_assigned
        ),
    };
    (seen, ev.reuse)
}

/// The first difference between two evaluations, briefly.
fn difference(a: &Seen, b: &Seen) -> String {
    let first = |x: &Seen1, y: &Seen1| {
        let (x, y) = (&x.head, &y.head);
        let i = x
            .bytes()
            .zip(y.bytes())
            .position(|(p, q)| p != q)
            .unwrap_or(x.len().min(y.len()));
        let from = i.saturating_sub(200);
        let cut = |s: &str| {
            let a = (from..=from.min(s.len()))
                .rev()
                .find(|&k| s.is_char_boundary(k))
                .unwrap_or(0);
            let e = ((i + 200).min(s.len())..=s.len())
                .find(|&k| s.is_char_boundary(k))
                .unwrap_or(s.len());
            s[a.min(e)..e].to_string()
        };
        format!(
            "\n  incremental: ...{}\n  full:        ...{}",
            cut(x),
            cut(y)
        )
    };
    if a.tree != b.tree {
        return format!("node tree differs:{}", first(&a.tree, &b.tree));
    }
    if a.lines != b.lines {
        return format!("messages differ:{}", first(&a.lines, &b.lines));
    }
    if a.console != b.console {
        return format!("console differs:{}", first(&a.console, &b.console));
    }
    if a.csg != b.csg {
        return format!("csg differs:{}", first(&a.csg, &b.csg));
    }
    if a.key != b.key {
        return "geometry key differs".into();
    }
    format!("flags differ: {} / {}", a.flags, b.flags)
}

/// Evaluate `text` through `memo` and from nothing; they must agree.
fn check(
    host: &Host,
    path: &Path,
    text: &str,
    opts: &Options,
    memo: &mut Memo,
) -> Result<eval::ReuseStats, String> {
    check_full(host, path, text, opts, memo).map(|(s, _)| s)
}

/// How a checked edit ended, when both evaluations agreed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Ran,
    /// Evaluation stopped on an error.
    Aborted,
    /// The watchdog stopped an evaluation: nothing to compare.
    TimedOut,
}

/// [`check`], also saying whether evaluation stopped on an error.
fn check_full(
    host: &Host,
    path: &Path,
    text: &str,
    opts: &Options,
    memo: &mut Memo,
) -> Result<(eval::ReuseStats, Outcome), String> {
    let p = host.parse(path, text.as_bytes());
    if p.program.has_syntax_errors() {
        return Ok((eval::ReuseStats::default(), Outcome::Ran));
    }
    let (inc, stats) = evaluate(&p, opts, Some(memo));
    if inc.interrupted {
        return Ok((stats, Outcome::TimedOut));
    }
    // A fresh parse, as a restarted host would have.
    let q = host.parse(path, text.as_bytes());
    let (full, _) = evaluate(&q, opts, None);
    if full.interrupted {
        return Ok((stats, Outcome::TimedOut));
    }
    if inc != full {
        return Err(difference(&inc, &full));
    }
    let outcome = if full.aborted {
        Outcome::Aborted
    } else {
        Outcome::Ran
    };
    Ok((stats, outcome))
}

// --- targeted cases -----------------------------------------------------------

/// Run each version of a program in turn through one memo, checking each
/// against a full evaluation, and return the reuse of each.
fn versions(files: &[(&str, &str)], versions: &[&str]) -> Vec<eval::ReuseStats> {
    versions_with(files, versions, &Options::default())
}

/// [`versions`] with evaluation options.
fn versions_with(
    files: &[(&str, &str)],
    versions: &[&str],
    opts: &Options,
) -> Vec<eval::ReuseStats> {
    let dir = scratch_dir();
    for (name, text) in files {
        std::fs::write(dir.join(name), text).unwrap();
    }
    let host = Host::new();
    let path = dir.join("main.scad");
    let mut memo = Memo::new();
    let stats = versions
        .iter()
        .enumerate()
        .map(|(i, v)| {
            // A version may rewrite a helper file first: `@name@text`.
            let text = if let Some(rest) = v.strip_prefix('@') {
                let (name, rest) = rest.split_once('@').unwrap();
                let (file, main) = rest.split_once("@@").unwrap();
                std::fs::write(dir.join(name), file).unwrap();
                main
            } else {
                v
            };
            check(&host, &path, text, opts, &mut memo)
                .unwrap_or_else(|e| panic!("version {i}: {e}\n--- program ---\n{text}"))
        })
        .collect();
    let _ = std::fs::remove_dir_all(&dir);
    stats
}

fn scratch_dir() -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let d = std::env::temp_dir().join(format!(
        "neoscad-reuse-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

#[test]
fn unchanged_statements_are_replayed() {
    let s = versions(
        &[],
        &[
            "a = 1;\ncube(a);\nsphere(2);\necho(\"b\", a);",
            "a = 1;\ncube(a);\nsphere(3);\necho(\"b\", a);",
            "a = 2;\ncube(a);\nsphere(3);\necho(\"b\", a);",
        ],
    );
    assert_eq!(s[0].recorded, 3);
    assert_eq!((s[1].reused, s[1].recorded), (2, 1));
    // `a` changed: both statements that read it run again.
    assert_eq!((s[2].reused, s[2].recorded), (1, 2));
}

#[test]
fn values_digest_by_content_and_shared_trees_digest_fast() {
    // A variable's digest walks its value. `t` below has 2^40 paths but 41
    // distinct lists; walking it path by path never finished. And the
    // digest is of the content alone: an equal value built another way
    // (unshared, as a literal, from other pieces) replays the statements
    // that read it, and a different one does not.
    let tree = "function f(v, n) = n == 0 ? v : f([v, v], n - 1);\n";
    let long = "a".repeat(70);
    let v = |t: &str| format!("{tree}t = {t};\ncube(len(t));\nsphere(1);");
    let versions_text = [
        v("f([1], 40)"),
        v("f([1], 40)"),
        v("f([2], 40)"),
        v("f([1], 3)"),
        // The same content with no sharing: replays.
        v("[[[[1], [1]], [[1], [1]]], [[[1], [1]], [[1], [1]]]]"),
        // Long lists and long strings (hashed on their own), rebuilt.
        v("[for (i = [0:19]) [i, str(\"x\", i)]]"),
        v("concat([for (i = [0:9]) [i, str(\"x\", i)]], [for (i = [10:19]) [i, str(\"x\", i)]])"),
        v(&format!("[\"{long}\", [\"{long}\"]]")),
        v(&format!(
            "[str(\"{}\", \"{}\"), [\"{long}\"]]",
            &long[..35],
            &long[35..]
        )),
        v(&format!("[\"{long}b\", [\"{long}\"]]")),
    ];
    let texts: Vec<&str> = versions_text.iter().map(String::as_str).collect();
    let t0 = std::time::Instant::now();
    let s = versions(&[], &texts);
    assert!(t0.elapsed().as_secs_f64() < 20.0, "{:?}", t0.elapsed());
    let reuse: Vec<_> = s.iter().map(|s| (s.reused, s.recorded)).collect();
    assert_eq!(
        reuse,
        [
            (0, 2),
            (2, 0),
            (1, 1),
            (1, 1),
            (2, 0),
            (1, 1),
            (2, 0),
            (1, 1),
            (2, 0),
            (1, 1)
        ]
    );
}

#[test]
fn objects_digest_by_content_and_shared_trees_digest_fast() {
    // As for lists: an object tree of depth 40 whose fields are one object
    // digests in linear time, an equal object built another way replays,
    // and key order is content (objects with their keys in another order
    // are not equal).
    let tree = "function f(v, n) = n == 0 ? v : f(object(a=v, b=v), n - 1);\n";
    let v = |t: &str| format!("{tree}t = {t};\ncube(len(t));\nsphere(1);");
    let versions_text = [
        v("f(object(z=1), 40)"),
        v("f(object(z=1), 40)"),
        v("f(object(z=2), 40)"),
        v("object(a=1, b=[1, \"x\"])"),
        v("object(object(a=1), [[\"b\", [1, \"x\"]]])"),
        v("object(b=[1, \"x\"], a=1)"),
        v("[object(k=[for (i = [0:19]) i])]"),
        v("[object(k=[each [0:9], each [10:19]])]"),
    ];
    let texts: Vec<&str> = versions_text.iter().map(String::as_str).collect();
    let opts = Options {
        features: eval::Features::from_names(&["object-function"]),
        ..Options::default()
    };
    let t0 = std::time::Instant::now();
    let s = versions_with(&[], &texts, &opts);
    assert!(t0.elapsed().as_secs_f64() < 20.0, "{:?}", t0.elapsed());
    let reuse: Vec<_> = s.iter().map(|s| (s.reused, s.recorded)).collect();
    assert_eq!(
        reuse,
        [
            (0, 2),
            (2, 0),
            (1, 1),
            (1, 1),
            (2, 0),
            (1, 1),
            (1, 1),
            (2, 0)
        ]
    );
}

/// Constrained sketches (`--enable sketch`) through the memo: a sketch is
/// never replayed (what it makes depends on everything its body ran, and
/// its entities are numbered per evaluation), and the statements around
/// it still are, with the same output as a fresh evaluation.
#[test]
fn sketches_are_evaluated_anew_and_their_neighbours_reused() {
    let sketch = |w: u32| {
        format!(
            "module rect(o, w, h) sketch() {{\n\
               a = point([w, 0]); b = point([w, h]); c = point([0, h]);\n\
               l1 = line(o, a); l2 = line(a, b); l3 = line(b, c); l4 = line(c, o);\n\
               horizontal(l1); vertical(l2); horizontal(l3); vertical(l4);\n\
               length(l1, w); length(l2, h);\n\
             }}\n\
             sketch(name = \"s\") {{ o = point([0, 0]); fix(o); rect(o, {w}, 10); echo(o); fillet(o, 2); }}\n"
        )
    };
    let texts = [
        format!("cube(1);\n{}", sketch(20)),
        format!("cube(1);\n{}", sketch(20)),
        format!("cube(2);\n{}", sketch(20)),
        format!("cube(2);\n{}", sketch(30)),
    ];
    let texts: Vec<&str> = texts.iter().map(String::as_str).collect();
    let opts = Options {
        extensions: eval::Extensions::NONE.with(eval::Extension::Sketch),
        ..Options::default()
    };
    let s = versions_with(&[], &texts, &opts);
    let reuse: Vec<_> = s
        .iter()
        .map(|s| (s.reused, s.recorded, s.unrecorded))
        .collect();
    assert_eq!(reuse, [(0, 1, 1), (1, 0, 1), (0, 1, 1), (1, 0, 1)]);
}

#[test]
fn moved_statements_keep_their_positions_and_indices() {
    versions(
        &[],
        &[
            "module m(x) { echo(x); cube(x); }\nm(1);\ntranslate([1,0,0]) { m(2); undefined_thing(); }\n",
            // Lines and offsets above everything shift.
            "// a comment\n\n\nmodule m(x) { echo(x); cube(x); }\nsphere(1);\nm(1);\ntranslate([1,0,0]) { m(2); undefined_thing(); }\n",
            // The definition moves below its uses.
            "sphere(1);\nm(1);\ntranslate([1,0,0]) { m(2); undefined_thing(); }\n\n\nmodule m(x) { echo(x); cube(x); }\n",
            // Statements reordered, one duplicated.
            "m(1);\nm(1);\nsphere(1);\ntranslate([1,0,0]) { m(2); undefined_thing(); }\nmodule m(x) { echo(x); cube(x); }\n",
        ],
    );
}

#[test]
fn hoisted_assignments_and_definitions() {
    let s = versions(
        &[],
        &[
            "echo(x);\nx = 1;\ncube(f(x));\nfunction f(v) = v * 2;\nsphere(1);",
            // The last assignment wins, wherever it is.
            "echo(x);\nx = 1;\ncube(f(x));\nfunction f(v) = v * 2;\nsphere(1);\nx = 3;",
            // A definition a statement reaches changes.
            "echo(x);\nx = 1;\ncube(f(x));\nfunction f(v) = v * 3;\nsphere(1);\nx = 3;",
            // A second definition of the same name replaces the first.
            "echo(x);\nx = 1;\ncube(f(x));\nfunction f(v) = v * 3;\nsphere(1);\nx = 3;\nfunction f(v) = v + 1;",
            // Functions reached through other functions.
            "echo(x);\nx = 1;\ncube(f(x));\nfunction f(v) = g(v) + 1;\nfunction g(v) = v * y;\ny = 2;\nsphere(1);",
            "echo(x);\nx = 1;\ncube(f(x));\nfunction f(v) = g(v) + 1;\nfunction g(v) = v * y;\ny = 5;\nsphere(1);",
        ],
    );
    assert_eq!(s[5].reused, 2, "only the statement reaching y reruns");
}

#[test]
fn special_variables_are_dynamic() {
    let s = versions(
        &[],
        &[
            "$fn = 8;\nsphere(1);\ncube(1);\nmodule m() echo($x);\nm();",
            "$fn = 9;\nsphere(1);\ncube(1);\nmodule m() echo($x);\nm();",
            "$fn = 9;\nsphere(1);\ncube(1);\nmodule m() echo($x);\nm();\n$x = 3;",
            "$fn = 9;\nsphere(1);\ncube(1);\nmodule m() echo($x);\nm($x = 4);\n$x = 3;",
            "$fn = 9;\n$t = 0.5;\n$vpr = [1, 2, 3];\nsphere(1);\ncube(1);\nmodule m() echo($x, $t, $vpr);\nm($x = 4);\n$x = 3;",
        ],
    );
    assert_eq!(s[1].reused, 0, "every statement can read $fn");
}

#[test]
fn messages_replay_in_order() {
    versions(
        &[],
        &[
            "echo(1);\nx = [1, 2][5];\necho(x + \"a\");\ncube(-1);\nfoo();\necho(3);\nfunction g() = h();\necho(g());",
            "echo(1);\nx = [1, 2][5];\necho(x + \"a\");\ncube(-1);\nfoo();\necho(4);\nfunction g() = h();\necho(g());",
            "echo(0);\necho(1);\nx = [1, 2][5];\necho(x + \"a\");\ncube(-1);\nfoo();\necho(4);\nfunction g() = h();\necho(g());",
        ],
    );
}

#[test]
fn random_numbers_always_rerun() {
    let s = versions(
        &[],
        &[
            "echo(rands(0, 1, 2));\ncube(1);\necho(rands(0, 1, 1, 7));\necho(rands(0, 1, 2));",
            "echo(rands(0, 1, 2));\ncube(2);\necho(rands(0, 1, 1, 7));\necho(rands(0, 1, 2));",
            // Removing the seeding call changes what the last one draws.
            "echo(rands(0, 1, 2));\ncube(2);\necho(rands(0, 1, 2));",
        ],
    );
    assert_eq!(s[1].reused, 0);
    assert_eq!(s[1].unrecorded, 3);
}

#[test]
fn deprecations_print_once_across_statements() {
    versions(
        &[],
        &[
            "r = 5;\nrotate_extrude($fn = r) translate([2, 0]) square(1);\ncube(1);\nrotate_extrude($fn = 7) translate([2, 0]) square(1);",
            // The first one no longer prints it, so the second must.
            "r = 6;\nrotate_extrude($fn = r) translate([2, 0]) square(1);\ncube(1);\nrotate_extrude($fn = 7) translate([2, 0]) square(1);",
            "r = 5;\nrotate_extrude($fn = r) translate([2, 0]) square(1);\ncube(1);\nrotate_extrude($fn = 7) translate([2, 0]) square(1);",
            "import(filename = \"a.stl\");\nimport(filename = \"a.stl\");\ncube(1);",
            "cube(1);\nimport(filename = \"a.stl\");",
        ],
    );
}

#[test]
fn included_files_are_part_of_the_key() {
    let s = versions(
        &[("inc.scad", "module part(x) cube(x + k);\nk = 1;\n")],
        &[
            "include <inc.scad>\npart(1);\nsphere(2);",
            "include <inc.scad>\npart(1);\nsphere(3);",
            // The included module changes, the main file does not.
            "@inc.scad@module part(x) cube(x * k);\nk = 1;\n@@include <inc.scad>\npart(1);\nsphere(3);",
            // Lines added to the included file move the definition.
            "@inc.scad@\n\n// moved\nmodule part(x) cube(x * k);\nk = 1;\n@@include <inc.scad>\npart(1);\nsphere(3);",
        ],
    );
    assert_eq!(s[1].reused, 1);
    assert_eq!(s[2].reused, 1, "only the statement using part() reruns");
}

#[test]
fn used_libraries_are_part_of_the_key() {
    versions(
        &[(
            "lib.scad",
            "module part(x) { echo(\"lib\", x); cube(x); }\n",
        )],
        &[
            "use <lib.scad>\npart(1);\nsphere(2);",
            "@lib.scad@module part(x) { echo(\"lib2\", x); cube(x); }\n@@use <lib.scad>\npart(1);\nsphere(2);",
            // The main file now defines it, over the library's.
            "use <lib.scad>\npart(1);\nsphere(2);\nmodule part(x) sphere(x);",
        ],
    );
}

#[test]
fn function_values_and_errors() {
    versions(
        &[],
        &[
            "f = function(x) x + 1;\necho(f(1));\ncube(1);",
            "f = function(x) x + 2;\necho(f(1));\ncube(1);",
            "f = function(x) x + 2;\necho(f(1));\ncube(1);\nassert(false, \"stop\");\nsphere(1);",
            "f = function(x) x + 2;\necho(f(1));\ncube(1);\nsphere(1);",
            "module r(n) r(n + 1);\ncube(1);\nr(0);\nsphere(1);",
            "module r(n) r(n + 1);\ncube(1);\nr(0);\nsphere(2);",
        ],
    );
}

#[test]
fn parent_modules_are_the_statements_own() {
    let s = versions(
        &[],
        &[
            "module a() b();\nmodule b() echo(parent_module(1), $parent_modules);\na();\nb();\ncube(1);",
            "module a() b();\nmodule b() echo(parent_module(1), $parent_modules);\na();\nb();\ncube(2);",
            "module a() b();\nmodule b() echo(parent_module(1), $parent_modules);\nb();\na();\ncube(2);",
        ],
    );
    assert_eq!(s[1].reused, 2);
    assert_eq!(s[2].reused, 3);
}

#[test]
fn root_modifier_and_camera() {
    versions(
        &[],
        &[
            "cube(1);\n!sphere(1);\n$vpd = 50;",
            "cube(1);\n!sphere(1);\n!cylinder(1);\n$vpd = 50;",
            "cube(2);\n!sphere(1);\n!cylinder(1);\n$vpd = 60;",
        ],
    );
}

#[test]
fn a_memo_over_its_budget_still_answers_as_a_full_evaluation() {
    let host = Host::new();
    let dir = scratch_dir();
    let path = dir.join("main.scad");
    // About 40 nodes of 512 bytes each fit: the small statements are kept,
    // the loop is not.
    let mut memo = Memo::with_budget(20_000);
    let opts = Options::default();
    let mut stats = Vec::new();
    for n in [1, 2, 3] {
        let text =
            format!("cube({n});\nfor (i = [0 : 99]) translate([i, 0, 0]) sphere(1);\nsphere(2);\n");
        stats.push(check(&host, &path, &text, &opts, &mut memo).unwrap());
    }
    assert!(memo.bytes() <= 20_000, "{}", memo.bytes());
    assert_eq!((stats[1].reused, stats[1].unrecorded), (1, 1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn memory_limit_is_not_skipped() {
    // A statement that needs a lot of memory, replayed after the ones before
    // it grew: the full evaluation passes the limit, so the replay must too.
    let dir = scratch_dir();
    let host = Host::new();
    let path = dir.join("main.scad");
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let limits = eval::limits::Limits {
        memory: Some(16 << 20),
        ..eval::limits::Limits::NONE
    };
    let opts = Options {
        guard: Some(Arc::new(eval::limits::Guard::new(
            limits,
            flag.clone(),
            None,
        ))),
        interrupt: Some(flag),
        ..Options::default()
    };
    let mut memo = Memo::new();
    // About 13 MB of list (counted), held while a loop runs the checks that
    // sample the estimate; the statements before add 512 bytes a node.
    let big = "echo(let (a = [for (i = [0 : 799999]) i]) len([for (j = [0 : 20000]) a[j]]));";
    let mut stats = Vec::new();
    for pre in [0, 1, 10000] {
        let text = format!("for (i = [0 : {pre}]) cube(1);\n{big}\n");
        stats.push(check(&host, &path, &text, &opts, &mut memo).unwrap());
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(stats[1].reused, 1, "far from the limit, the list replays");
    assert_eq!(stats[2].reused, 0, "near it, the list is evaluated");
}

// --- random edits ---------------------------------------------------------------

/// xorshift64*: repeatable without a dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Byte ranges of number literals outside comments and strings.
fn numbers(t: &str) -> Vec<(usize, usize)> {
    let b = t.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i += 2;
            }
            b'"' => {
                i += 1;
                while i < b.len() && b[i] != b'"' {
                    i += if b[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            c if c.is_ascii_alphabetic() || c == b'_' || c == b'$' => {
                while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] == b'$')
                {
                    i += 1;
                }
            }
            c if c.is_ascii_digit() => {
                let s = i;
                while i < b.len() && (b[i].is_ascii_digit() || b[i] == b'.') {
                    i += 1;
                }
                out.push((s, i));
            }
            _ => i += 1,
        }
    }
    out
}

/// Offsets of line starts at brace depth 0 outside comments: places a
/// top-level statement can go.
fn top_level_lines(t: &str) -> Vec<usize> {
    let b = t.as_bytes();
    let mut out = vec![0];
    let (mut depth, mut i) = (0i32, 0);
    let mut in_block = false;
    while i < b.len() {
        if in_block {
            if b[i] == b'*' && b.get(i + 1) == Some(&b'/') {
                in_block = false;
                i += 1;
            }
        } else {
            match b[i] {
                b'/' if b.get(i + 1) == Some(&b'*') => in_block = true,
                b'/' if b.get(i + 1) == Some(&b'/') => {
                    while i < b.len() && b[i] != b'\n' {
                        i += 1;
                    }
                    continue;
                }
                b'{' | b'(' | b'[' => depth += 1,
                b'}' | b')' | b']' => depth -= 1,
                b'\n' if depth == 0 && i + 1 < b.len() => {
                    // Only after a statement ends.
                    let prev = t[..i].trim_end();
                    if prev.is_empty() || prev.ends_with(';') || prev.ends_with('}') {
                        out.push(i + 1);
                    }
                }
                _ => {}
            }
        }
        i += 1;
    }
    out
}

/// Lines that are whole top-level statements or assignments.
fn statement_lines(t: &str) -> Vec<(usize, usize)> {
    let starts = top_level_lines(t);
    let mut out = Vec::new();
    for w in starts.windows(2) {
        let line = &t[w[0]..w[1]];
        let s = line.trim();
        if !s.is_empty() && s.ends_with(';') && !s.starts_with("include") && !s.starts_with("use") {
            out.push((w[0], w[1]));
        }
    }
    out
}

/// Top-level assignments `name = ...;` on one line.
fn assignment_names(t: &str) -> Vec<String> {
    statement_lines(t)
        .into_iter()
        .filter_map(|(a, b)| {
            let s = t[a..b].trim();
            let (name, _) = s.split_once('=')?;
            let name = name.trim();
            (!name.is_empty()
                && name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'$'))
            .then(|| name.to_string())
        })
        .collect()
}

const SNIPPETS: &[&str] = &[
    "cube(%N);\n",
    "echo(\"inserted\", %N);\n",
    "translate([%N, 0, 0]) sphere(%N);\n",
    "$fn = %N;\n",
    "$fa = %N;\n",
    "inserted_%N = %N;\n",
    "echo(undefined_%N);\n",
    "if (%N > 3) cube(1); else sphere(1);\n",
    "\n\n",
    "// a comment line\n",
    "module inserted_m() { echo(\"m\", $fn); cylinder(h = %N, r = 1); }\n",
    "inserted_m();\n",
    "function inserted_f(x) = x * %N;\n",
    "echo(inserted_f(2));\n",
    "for (i = [0 : %N]) translate([i, 0, 0]) cube(0.5);\n",
    "color(\"red\") #cube(%N);\n",
    "echo(rands(0, 1, 1));\n",
];

/// One random small edit; `None` if the text offers nothing for the kind
/// picked.
fn edit(t: &str, r: &mut Rng) -> Option<String> {
    let num = |r: &mut Rng| match r.below(4) {
        0 => format!("{}", r.below(10)),
        1 => format!("{}", r.below(100) + 1),
        2 => format!("{}.{}", r.below(20), r.below(10)),
        _ => format!("{}", r.below(3) + 1),
    };
    let mut out = t.to_string();
    match r.below(10) {
        // Tweak a number (most common: what typing a value does).
        0..=3 => {
            let ns = numbers(t);
            let (a, b) = ns[r.below(ns.len().max(1)).min(ns.len().checked_sub(1)?)];
            out.replace_range(a..b, &num(r));
        }
        // Insert a statement at the top level.
        4 | 5 => {
            let at = top_level_lines(t);
            let at = at[r.below(at.len())];
            let mut s = SNIPPETS[r.below(SNIPPETS.len())].to_string();
            while let Some(i) = s.find("%N") {
                s.replace_range(i..i + 2, &num(r));
            }
            out.insert_str(at, &s);
        }
        // Delete a statement.
        6 => {
            let ls = statement_lines(t);
            let (a, b) = *ls.get(r.below(ls.len()))?;
            out.replace_range(a..b, "");
        }
        // Change a top-level variable: reassign it (the last one wins)
        // somewhere at the top level.
        7 => {
            let names = assignment_names(t);
            let name = names.get(r.below(names.len()))?;
            let at = top_level_lines(t);
            let at = at[r.below(at.len())];
            let v = if r.below(2) == 0 {
                num(r)
            } else {
                format!("{name} + {}", num(r))
            };
            out.insert_str(at, &format!("{name} = {v};\n"));
        }
        // Duplicate a statement elsewhere.
        8 => {
            let ls = statement_lines(t);
            let (a, b) = *ls.get(r.below(ls.len()))?;
            let s = t[a..b].to_string();
            let at = top_level_lines(t);
            out.insert_str(at[r.below(at.len())], &s);
        }
        // Whitespace that moves lines or columns.
        _ => {
            let at = top_level_lines(t);
            let at = at[r.below(at.len())];
            out.insert_str(at, if r.below(2) == 0 { "\n" } else { "  \n\n" });
        }
    }
    Some(out)
}

/// This process's resident memory, from `ps` (0 if it cannot be read).
fn rss_mb() -> u64 {
    std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &std::process::id().to_string()])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<u64>()
                .ok()
        })
        .map_or(0, |kb| kb / 1024)
}

fn corpus(full: bool) -> Vec<PathBuf> {
    let r = repo();
    let mut out = Vec::new();
    let mut add_dir = |d: PathBuf, recurse: bool| {
        let mut stack = vec![d];
        while let Some(d) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else {
                continue;
            };
            let mut es: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
            es.sort();
            for p in es {
                if p.is_dir() {
                    if recurse {
                        stack.push(p);
                    }
                } else if p.extension().is_some_and(|e| e == "scad") {
                    out.push(p);
                }
            }
        }
    };
    if full {
        add_dir(r.join(".reference/BOSL2/examples"), false);
        add_dir(r.join(".reference/BOSL2/examples_x"), false);
        add_dir(r.join(".reference/BOSL2/tests_x"), false);
        add_dir(r.join(".reference/openscad/tests/data/scad"), true);
        add_dir(r.join(".reference/openscad/examples"), true);
        add_dir(r.join("examples"), true);
        out.push(r.join("apple/Icon/hero.scad"));
    } else {
        for f in [
            ".reference/openscad/examples/Basics/CSG.scad",
            ".reference/openscad/examples/Basics/logo.scad",
            ".reference/openscad/examples/Functions/echo.scad",
            ".reference/openscad/examples/Functions/recursion.scad",
            ".reference/openscad/tests/data/scad/misc/echo-tests.scad",
            ".reference/openscad/tests/data/scad/misc/variable-scope-tests.scad",
        ] {
            let p = r.join(f);
            if p.exists() {
                out.push(p);
            }
        }
    }
    out
}

/// Random small edits to real models, each checked against a full
/// evaluation. `NEOSCAD_REUSE_CORPUS=full` runs the whole corpus (use a
/// release build: `cargo test --release -p neoscad-eval --test incremental
/// random_edits -- --nocapture`); `NEOSCAD_REUSE_EDITS` sets the edits per
/// file (default 12, 4 for the short corpus), `NEOSCAD_REUSE_SEED` the
/// seed, `NEOSCAD_REUSE_FILTER` keeps only paths containing it, and
/// `NEOSCAD_REUSE_MAX_MS` skips files whose first evaluation takes longer
/// (default 3000), `NEOSCAD_REUSE_TIMEOUT_S` interrupts an evaluation
/// (default 20; the edit is then not compared), and
/// `NEOSCAD_REUSE_VERBOSE` prints each file's path.
/// The test fails if the process holds more than `NEOSCAD_REUSE_MAX_RSS_MB`
/// (default 4096) at a check (every file, and every 10 edits).
/// `NEOSCAD_REUSE_SHARD=k/n` runs every n-th file from the k-th, to split
/// the full corpus (about 4,100 files) across processes run in turn.
#[test]
fn random_edits() {
    let env = |k: &str| std::env::var(k).ok();
    let full = env("NEOSCAD_REUSE_CORPUS").as_deref() == Some("full");
    let edits: usize = env("NEOSCAD_REUSE_EDITS")
        .and_then(|s| s.parse().ok())
        .unwrap_or(if full { 12 } else { 4 });
    let seed: u64 = env("NEOSCAD_REUSE_SEED")
        .and_then(|s| s.parse().ok())
        .unwrap_or(0x5eed);
    let max_ms: u128 = env("NEOSCAD_REUSE_MAX_MS")
        .and_then(|s| s.parse().ok())
        .unwrap_or(3000);
    let filter = env("NEOSCAD_REUSE_FILTER").unwrap_or_default();
    let verbose = env("NEOSCAD_REUSE_VERBOSE").is_some();
    // The harness holds one file's parses and memo at a time (about
    // 0.8 GB with BOSL2, with peaks near 2.4 GB after BOSL2's heavier
    // examples, which the allocator keeps); more means something is kept
    // that should not be.
    let max_rss: u64 = env("NEOSCAD_REUSE_MAX_RSS_MB")
        .and_then(|s| s.parse().ok())
        .unwrap_or(4096);
    // `k/n`: only every n-th file from the k-th, so the full corpus can run
    // as several processes in turn.
    let (shard, shards) = env("NEOSCAD_REUSE_SHARD")
        .and_then(|s| {
            let (k, n) = s.split_once('/')?;
            Some((k.parse::<usize>().ok()?, n.parse::<usize>().ok()?))
        })
        .unwrap_or((0, 1));
    let host = Host::new();
    let mut rng = Rng(seed | 1);
    let (mut files, mut checked, mut skipped, mut aborted_edits) = (0, 0, 0, 0);
    let mut timed_out = 0;
    let mut totals = eval::ReuseStats::default();
    let mut failures = Vec::new();
    for (i, path) in corpus(full).into_iter().enumerate() {
        if !path.to_string_lossy().contains(&filter) || i % shards.max(1) != shard {
            continue;
        }
        let Ok(orig) = std::fs::read_to_string(&path) else {
            continue;
        };
        // Half the files run under the app's resource limits, which takes
        // the memory-tracking path. The full corpus runs everything under a
        // memory limit, since some models (and random edits: `$fn = 90` in
        // a BOSL2 example) can otherwise exhaust the machine; not under a
        // time limit, which could stop one of the two evaluations and not
        // the other.
        let guarded = |mut limits: eval::limits::Limits| {
            limits.time = None;
            let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let guard = eval::limits::Guard::new(limits, flag.clone(), None);
            Options {
                guard: Some(Arc::new(guard)),
                interrupt: Some(flag),
                ..Options::default()
            }
        };
        let opts = match (rng.below(2), full) {
            (0, false) => Options::default(),
            (0, true) => guarded(eval::limits::Limits {
                memory: Some(1 << 30),
                ..eval::limits::Limits::NONE
            }),
            _ => guarded(eval::limits::Limits {
                memory: Some(1 << 30),
                ..eval::limits::Limits::AGENT
            }),
        };
        host.new_main_file();
        let rss = rss_mb();
        assert!(
            rss <= max_rss,
            "{rss} MB resident before {}: over NEOSCAD_REUSE_MAX_RSS_MB",
            path.display()
        );
        if full && verbose {
            eprintln!("{} (rss {rss} MB)", path.display());
        }
        let t0 = std::time::Instant::now();
        let first = host.parse(&path, orig.as_bytes());
        if first.program.has_syntax_errors() {
            continue;
        }
        let _ = evaluate(&first, &opts, None);
        if t0.elapsed().as_millis() > max_ms {
            skipped += 1;
            continue;
        }
        files += 1;
        let mut memo = Memo::new();
        let mut text = orig.clone();
        let mut history = vec![text.clone()];
        let mut done = 0;
        let mut tries = 0;
        let orig_aborted = match check_full(&host, &path, &text, &opts, &mut memo) {
            Ok((_, Outcome::TimedOut)) => {
                skipped += 1;
                continue;
            }
            Ok((_, a)) => a == Outcome::Aborted,
            Err(e) => {
                failures.push(format!("{} (unedited): {e}", path.display()));
                continue;
            }
        };
        while done < edits && tries < edits * 10 {
            tries += 1;
            // Now and then go back to an earlier version (an undo).
            let next = if rng.below(8) == 0 {
                Some(history[rng.below(history.len())].clone())
            } else {
                edit(&text, &mut rng)
            };
            let Some(next) = next else { continue };
            if host
                .parse(&path, next.as_bytes())
                .program
                .has_syntax_errors()
            {
                continue;
            }
            let prev = std::mem::replace(&mut text, next);
            done += 1;
            checked += 1;
            if env("NEOSCAD_REUSE_TRACE").is_some() {
                eprintln!("  edit {done} (rss {} MB):\n{text}", rss_mb());
            }
            if done % 10 == 0 {
                let rss = rss_mb();
                assert!(
                    rss <= max_rss,
                    "{} MB resident after {done} edits of {}: over NEOSCAD_REUSE_MAX_RSS_MB",
                    rss,
                    path.display()
                );
                if verbose && done % 50 == 0 {
                    eprintln!(
                        "  {done} edits (rss {rss} MB, memo {} entries, {} bytes)",
                        memo.len(),
                        memo.bytes()
                    );
                }
            }
            match check_full(&host, &path, &text, &opts, &mut memo) {
                Ok((s, outcome)) => {
                    // An edit that makes the model fail stops evaluation
                    // at the failing statement, and every later edit would
                    // only test that; so it is checked, then backed out.
                    // One that ran too long is backed out uncompared.
                    if outcome == Outcome::TimedOut {
                        timed_out += 1;
                        text = prev;
                        continue;
                    }
                    if outcome == Outcome::Aborted && !orig_aborted {
                        aborted_edits += 1;
                        text = prev;
                    } else {
                        history.push(text.clone());
                    }
                    totals.statements += s.statements;
                    totals.reused += s.reused;
                    totals.recorded += s.recorded;
                    totals.unrecorded += s.unrecorded;
                }
                Err(e) => {
                    let dump = std::env::temp_dir()
                        .join(format!("neoscad-reuse-failure-{}.scad", failures.len()));
                    let _ = std::fs::write(&dump, &text);
                    failures.push(format!(
                        "{} after {done} edits (text in {}): {e}",
                        path.display(),
                        dump.display()
                    ));
                    break;
                }
            }
        }
    }
    eprintln!(
        "random_edits: {files} files ({skipped} skipped as slow), {checked} edits checked \
         ({aborted_edits} made the model fail and were backed out, \
         {timed_out} ran past the watchdog and were not compared); \
         statements {} reused {} recorded {} unrecorded {}",
        totals.statements, totals.reused, totals.recorded, totals.unrecorded
    );
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
