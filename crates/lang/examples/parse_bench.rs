//! Front-end throughput over a corpus of `.scad` files.
//!
//!     cargo run --release -p neoscad-lang --example parse_bench [PATHS...]
//!
//! Default corpus: the reference checkout's tests/data/scad, examples and
//! libraries/MCAD. Files are read into memory first, then each stage runs
//! over the whole corpus repeatedly for about a second: lexing alone, lexing
//! plus parsing into the CST, and the full single-file pipeline (CST, AST
//! lowering, diagnostics). Includes are not followed, so every byte is
//! counted once.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use lang::source::FileId;
use lang::syntax::lexer::lex;

fn collect(p: &Path, out: &mut Vec<PathBuf>) {
    if p.is_dir() {
        if let Ok(rd) = std::fs::read_dir(p) {
            for e in rd.flatten() {
                collect(&e.path(), out);
            }
        }
    } else if p.extension().is_some_and(|e| e == "scad") {
        out.push(p.to_path_buf());
    }
}

fn bench(name: &str, bytes: usize, mut f: impl FnMut()) {
    f(); // warm up
    let start = Instant::now();
    let mut iters = 0u32;
    while start.elapsed() < Duration::from_secs(1) {
        f();
        iters += 1;
    }
    let secs = start.elapsed().as_secs_f64() / f64::from(iters);
    println!(
        "{name:<22} {:>8.1} MB/s  ({:.2} ms per pass, {iters} passes)",
        bytes as f64 / secs / 1e6,
        secs * 1e3
    );
}

fn main() {
    let mut roots: Vec<PathBuf> = std::env::args().skip(1).map(PathBuf::from).collect();
    if roots.is_empty() {
        let r = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.reference/openscad");
        roots = ["tests/data/scad", "examples", "libraries/MCAD"]
            .iter()
            .map(|p| r.join(p))
            .collect();
    }
    let mut files = Vec::new();
    for r in &roots {
        collect(r, &mut files);
    }
    let texts: Vec<(PathBuf, Vec<u8>)> = files
        .into_iter()
        .filter_map(|p| Some((p.clone(), std::fs::read(&p).ok()?)))
        .collect();
    let bytes: usize = texts.iter().map(|(_, t)| t.len()).sum();
    println!("{} files, {:.2} MB", texts.len(), bytes as f64 / 1e6);

    bench("lex", bytes, || {
        for (_, t) in &texts {
            std::hint::black_box(lex(t, FileId(0)));
        }
    });
    bench("lex + parse (CST)", bytes, || {
        for (_, t) in &texts {
            std::hint::black_box(lang::syntax::parse(lex(t, FileId(0)).tokens));
        }
    });
    bench("full (CST + AST)", bytes, || {
        for (p, t) in &texts {
            std::hint::black_box(lang::parse_file(p.clone(), t.clone()));
        }
    });
}
