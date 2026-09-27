//! How names in real programs resolve (see the evaluator's `resolve`
//! module), and how long evaluation takes.
//!
//!     cargo run --release -p neoscad-eval --example resolve_stats -- [--runs N] FILE...
//!
//! Libraries are searched in `OPENSCADPATH`. For each file this prints the
//! name references resolved, how many of those are `$` names (dynamic by
//! definition), how many lookups of other names found no resolution and
//! walked the scope chain by name (fallbacks), and the best in-process
//! evaluation time of `N` runs (default 5), then the totals.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use lang::loader::{LibraryPath, StdFs};

fn main() {
    let libs = LibraryPath::from_env();
    let fs = StdFs;
    let mut total = eval::ResolveStats::default();
    let mut runs = 5;
    let mut args = std::env::args().skip(1);
    let mut files = Vec::new();
    while let Some(a) = args.next() {
        if a == "--runs" {
            runs = args.next().and_then(|n| n.parse().ok()).unwrap_or(runs);
        } else {
            files.push(a);
        }
    }
    for arg in files {
        let path = std::fs::canonicalize(&arg).unwrap_or_else(|_| PathBuf::from(&arg));
        let Ok(mut text) = std::fs::read(&path) else {
            eprintln!("{arg}: unreadable");
            continue;
        };
        text.extend_from_slice(b"\n\x03\n");
        let program = lang::parse_program(path.clone(), text, &fs, &libs);
        if program.has_syntax_errors() {
            eprintln!("{arg}: syntax errors");
            continue;
        }
        let libraries = lang::deps::load_dependencies(&program, b"\n\x03\n", &fs, &libs);
        let uses = lang::deps::resolve_uses(&program, &fs, &libs);
        let libs_ev: Vec<eval::Library<'_>> = libraries
            .iter()
            .map(|l| eval::Library {
                path: &l.path,
                program: l.program.as_ref(),
                uses: &l.uses,
            })
            .collect();
        let dir = path.parent().map(PathBuf::from).unwrap_or_default();
        let opts = eval::Options {
            fs: std::sync::Arc::new(StdFs),
            ..eval::Options::default()
        };
        let mut best = Duration::MAX;
        let mut stats = eval::ResolveStats::default();
        for _ in 0..runs.max(1) {
            let mut out = eval::Collect::default();
            let t = Instant::now();
            let ev = eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
                eval::evaluate(&program, &uses, &libs_ev, dir.clone(), &opts, &mut out)
            });
            best = best.min(t.elapsed());
            stats = ev.resolution;
        }
        println!(
            "{arg}: {} references, {} `$`, {} fallbacks; evaluation {:.1} ms",
            stats.references,
            stats.special,
            stats.fallbacks,
            best.as_secs_f64() * 1e3
        );
        total.references += stats.references;
        total.special += stats.special;
        total.fallbacks += stats.fallbacks;
    }
    println!(
        "total: {} references, {} `$`, {} fallbacks",
        total.references, total.special, total.fallbacks
    );
}
