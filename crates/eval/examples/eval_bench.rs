//! Evaluator micro-benchmarks, with the OpenSCAD nightly as the reference.
//!
//!     cargo run --release -p neoscad-eval --example eval_bench [OPENSCAD]
//!
//! Each case is a small script that stresses one hot path: non-tail
//! recursion (calls and variable lookup), tail recursion (the call loop),
//! a large list comprehension (vector building), string building, and
//! module instantiation. The case is parsed once and evaluated for about a
//! second; the best time is reported. If an OpenSCAD binary is given (the
//! pinned nightly by default, when present), the same script also runs
//! there with `-o x.echo`, and its wall time (process start included) is
//! shown next to ours, together with our own process time via `neoscad`
//! when it has been built.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const CASES: &[(&str, &str)] = &[
    (
        "fib(24) recursive",
        "function fib(n) = n < 2 ? n : fib(n - 1) + fib(n - 2);\necho(fib(24));\n",
    ),
    (
        "tail loop 9e5",
        "function count(n, acc = 0) = n == 0 ? acc : count(n - 1, acc + n);\necho(count(900000));\n",
    ),
    (
        "list comp 9e5",
        "v = [for (i = [0 : 899999]) if (i % 3 != 0) [i, i * 2]];\necho(len(v), v[len(v) - 1]);\n",
    ),
    (
        "nested for 1000x1000",
        "s = [for (i = [0 : 999]) for (j = [0 : 999]) i * j];\necho(len(s));\n",
    ),
    (
        "string build 20000",
        "function build(n, s = \"\") = n == 0 ? s : build(n - 1, str(s, chr(65 + n % 26)));\necho(len(build(20000)));\n",
    ),
    (
        "modules 100k",
        "module leaf(i) cube(i);\nmodule row(n) for (i = [1 : n]) leaf(i);\nfor (j = [1 : 100]) row(1000);\necho(\"done\");\n",
    ),
];

fn scratch_dir() -> PathBuf {
    let d = std::env::temp_dir().join("neoscad-eval-bench");
    let _ = std::fs::create_dir_all(&d);
    d
}

/// Best of several runs of `f`, over about a second.
fn best(mut f: impl FnMut()) -> Duration {
    let start = Instant::now();
    let mut best = Duration::MAX;
    let mut n = 0;
    while n < 3 || (start.elapsed() < Duration::from_secs(1) && n < 50) {
        let t = Instant::now();
        f();
        best = best.min(t.elapsed());
        n += 1;
    }
    best
}

fn time_process(bin: &Path, script: &Path) -> Option<Duration> {
    let out = script.with_extension("echo");
    let mut best_t = None::<Duration>;
    for _ in 0..3 {
        let t = Instant::now();
        let ok = Command::new(bin)
            .arg(script)
            .arg("-o")
            .arg(&out)
            .output()
            .ok()?
            .status
            .success();
        let e = t.elapsed();
        if !ok {
            return None;
        }
        best_t = Some(best_t.map_or(e, |b| b.min(e)));
    }
    best_t
}

fn main() {
    let reference = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/Applications/OpenSCAD.app/Contents/MacOS/OpenSCAD"));
    let reference = reference.is_file().then_some(reference);
    let neoscad = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/release/neoscad");
    let neoscad = neoscad.is_file().then_some(neoscad);
    let dir = scratch_dir();
    println!(
        "{:<24} {:>12} {:>14} {:>14}",
        "case", "in-process", "neoscad proc", "openscad proc"
    );
    for (i, (name, src)) in CASES.iter().enumerate() {
        let path = dir.join(format!("case{i}.scad"));
        std::fs::write(&path, src).expect("write scratch file");
        let mut text = src.as_bytes().to_vec();
        text.extend_from_slice(b"\n\x03\n");
        let program = lang::parse_file(path.clone(), text);
        let opts = eval::Options::default();
        let t = eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
            best(|| {
                let mut out = eval::Collect::default();
                let r = eval::evaluate(&program, &[], &[], dir.clone(), &opts, &mut out);
                std::hint::black_box(r);
            })
        });
        let fmt = |d: Option<Duration>| {
            d.map_or("-".to_string(), |d| {
                format!("{:.1} ms", d.as_secs_f64() * 1e3)
            })
        };
        let neo = neoscad.as_deref().and_then(|b| time_process(b, &path));
        let ours = format!("{:.1} ms", t.as_secs_f64() * 1e3);
        let theirs = reference.as_deref().and_then(|b| time_process(b, &path));
        println!("{name:<24} {ours:>12} {:>14} {:>14}", fmt(neo), fmt(theirs));
    }
}
