//! The expected-output harness shared by `statements.rs` and
//! `functions.rs`: a program's messages, its `.csg` dump and every node's
//! index and kind, compared with a file the recursive evaluator wrote
//! (`NEOSCAD_BLESS=1`), so the heap evaluator (`--features heap-eval`) is
//! held to its output byte for byte.

#![allow(dead_code)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use eval::{Node, Options};

struct Lines {
    lines: Vec<String>,
    /// Raised after this many messages, when set.
    stop: Option<(usize, Arc<AtomicBool>)>,
}

impl eval::Output for Lines {
    fn message(&mut self, m: &eval::Message<'_>) {
        let mut s = format!(
            "{}: {}",
            m.diag.severity.openscad_label(),
            String::from_utf8_lossy(m.text)
        );
        if m.diag.span.is_some() {
            s.push_str(&format!(" @{}", m.diag.line));
        }
        self.lines.push(s);
        if let Some((n, flag)) = &self.stop
            && self.lines.len() >= *n
        {
            flag.store(true, Ordering::Relaxed);
        }
    }
}

fn indices(n: &Node, depth: usize, out: &mut String) {
    // Iterative: the deep cases' trees are thousands of levels deep.
    let mut todo = vec![(n, depth)];
    while let Some((n, d)) = todo.pop() {
        let kind = format!("{:?}", n.kind);
        let kind = kind.split(['(', ' ', '{']).next().unwrap_or("");
        out.push_str(&format!("{d} {} {kind}\n", n.index));
        for c in n.children.iter().rev() {
            todo.push((c, d + 1));
        }
    }
}

pub fn run(src: &str, opts: &Options, stop: Option<usize>) -> (String, eval::Evaluation) {
    let path = PathBuf::from("/nonexistent/test.scad");
    let mut text = src.as_bytes().to_vec();
    text.extend_from_slice(b"\n\x03\n");
    let program = lang::parse_file(path, text);
    assert!(!program.has_syntax_errors(), "syntax error in test program");
    let mut opts = opts.clone();
    let mut out = Lines {
        lines: Vec::new(),
        stop: None,
    };
    if let Some(n) = stop {
        let flag = Arc::new(AtomicBool::new(false));
        opts.interrupt = Some(flag.clone());
        out.stop = Some((n, flag));
    }
    let ev = eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
        eval::evaluate(
            &program,
            &[],
            &[],
            PathBuf::from("/nonexistent"),
            &opts,
            &mut out,
        )
    });
    let mut s = out.lines.join("\n");
    s.push_str(&format!(
        "\n-- aborted {} interrupted {} hard {}\n-- csg\n",
        ev.aborted, ev.interrupted, ev.hard_warning
    ));
    s.push_str(&eval::dump::csg(
        &ev.root,
        std::path::Path::new("/nonexistent"),
        &*opts.fs,
    ));
    s.push_str("\n-- nodes\n");
    indices(&ev.root, 0, &mut s);
    (s, ev)
}

pub const HEAP: bool = cfg!(feature = "heap-eval");

pub fn with_depth(depth: u64, opts: Options) -> Options {
    let limits = eval::limits::Limits {
        depth: Some(depth),
        ..eval::limits::Limits::NONE
    };
    let flag = Arc::new(AtomicBool::new(false));
    Options {
        guard: Some(Arc::new(eval::limits::Guard::new(
            limits,
            flag.clone(),
            None,
        ))),
        interrupt: Some(flag),
        ..opts
    }
}

pub fn check_in(dir: &str, name: &str, src: &str, opts: &Options, stop: Option<usize>) {
    let (got, _) = run(src, opts, stop);
    let file = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(dir)
        .join(format!("{name}.expected"));
    // Blessed by the recursive evaluator, the reference.
    if !HEAP && std::env::var_os("NEOSCAD_BLESS").is_some() {
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, &got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&file).unwrap_or_else(|e| panic!("{file:?}: {e}"));
    if got != want {
        let (g, w): (Vec<_>, Vec<_>) = (got.lines().collect(), want.lines().collect());
        let at = g
            .iter()
            .zip(&w)
            .position(|(a, b)| a != b)
            .unwrap_or(g.len().min(w.len()));
        panic!(
            "{name}: differs at line {at}:\n got: {:?}\nwant: {:?}",
            g.get(at),
            w.get(at)
        );
    }
}
