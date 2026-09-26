//! Format every `.scad` file under the given directories and report:
//! how many formatted, were left alone for syntax errors, failed the
//! formatter's own checks (token and `.ast` equality), or were not
//! idempotent, and the time taken.
//!
//!     cargo run --release -p neoscad-fmt --example corpus -- DIR... [-v]

use std::path::{Path, PathBuf};
use std::time::Instant;

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.filter_map(|e| e.ok().map(|e| e.path())).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|e| e == "scad") {
            out.push(p);
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let verbose = args.iter().any(|a| a == "-v");
    let cfg = scadfmt::Config::default();
    for dir in args.iter().filter(|a| *a != "-v") {
        let mut files = Vec::new();
        walk(Path::new(dir), &mut files);
        let texts: Vec<Vec<u8>> = files.iter().map(|p| std::fs::read(p).unwrap()).collect();
        let bytes: usize = texts.iter().map(Vec::len).sum();
        let (mut ok, mut syntax, mut unsupported, mut internal, mut not_idem, mut changed) =
            (0, 0, 0, 0, 0, 0);
        let t = Instant::now();
        let outs: Vec<_> = texts.iter().map(|t| scadfmt::format(t, &cfg)).collect();
        let elapsed = t.elapsed();
        for ((p, text), r) in files.iter().zip(&texts).zip(outs) {
            match r {
                Ok(out) => {
                    ok += 1;
                    if &out != text {
                        changed += 1;
                    }
                    match scadfmt::format(&out, &cfg) {
                        Ok(again) if again == out => {}
                        Ok(_) => {
                            not_idem += 1;
                            if verbose {
                                println!("NOT IDEMPOTENT {}", p.display());
                            }
                        }
                        Err(e) => {
                            not_idem += 1;
                            if verbose {
                                println!("SECOND PASS FAILED {}: {e}", p.display());
                            }
                        }
                    }
                }
                Err(e @ scadfmt::Error::Syntax(_)) => {
                    syntax += 1;
                    if verbose {
                        println!("SYNTAX {}: {e}", p.display());
                    }
                }
                Err(e @ scadfmt::Error::Unsupported(_)) => {
                    unsupported += 1;
                    if verbose {
                        println!("UNSUPPORTED {}: {e}", p.display());
                    }
                }
                Err(e @ scadfmt::Error::Internal(_)) => {
                    internal += 1;
                    if verbose {
                        println!("INTERNAL {}: {e}", p.display());
                    }
                }
            }
        }
        println!(
            "{dir}: {} files ({:.1} MB): {ok} formatted ({changed} changed), {syntax} syntax errors, \
             {unsupported} unsupported, {internal} failed checks, {not_idem} not idempotent; {:.0} ms",
            files.len(),
            bytes as f64 / 1e6,
            elapsed.as_secs_f64() * 1000.0
        );
    }
}
