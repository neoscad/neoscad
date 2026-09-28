//! The memory limit against programs that materialise values the estimate
//! once missed: every list and string counts now, however small, and the
//! limit trips from inside the allocation that passes it.
//!
//! The first of these came from the bytecode VM's fuzzer (see
//! `docs/audits/bytecode-vm.md`, "The fuzzer's memory incident"): a tail
//! call that doubles a list of two elements whose halves are shared costs
//! nothing, and unary minus then copies it into 2^26 small lists. Under a
//! 64 MiB limit it passed 1.1 GB before an outside guard killed it.
//!
//! Every evaluation here runs under a limit, and a watchdog thread aborts
//! the test process if it ever holds more than 1 GB, so a regression fails
//! loudly instead of filling the machine's swap.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Once};

use eval::limits::{Guard, Limit, Limits};
use eval::{Collect, Options};
use lang::diag::DiagCode;

/// The limit every program here must stop under.
const LIMIT: u64 = 64 << 20;

const TREE: &str = "function f(p, n) = n == 0 ? p : f([p, p], n - 1);\n";

/// Programs that each build far more than [`LIMIT`] of values, messages or
/// text, by a route the estimate once did not count.
fn runaways() -> Vec<(&'static str, String)> {
    let tree = |s: &str| format!("{TREE}{s}");
    vec![
        // Shared trees materialised element-wise, and printed.
        ("negate", tree("echo(len(-f([1], 26)));")),
        ("add", tree("t = f([1], 26); echo(len(t + t));")),
        ("scale", tree("echo(len(f([1], 26) * 2));")),
        ("divide", tree("echo(len(f([1], 26) / 2));")),
        ("divide into", tree("echo(len(2 / f([1], 26)));")),
        ("str", tree("echo(len(str(f([1], 26))));")),
        ("echo", tree("echo(f([1], 26));")),
        // The fuzzer's own shape: `each`, a value and a comprehension.
        (
            "fuzzer",
            "function f2(p, n) = n == 0 ? p : f2([each false, p, for (i = [0:0]) p], n - 1);\n\
             echo(len(-f2([1], 26)));"
                .into(),
        ),
        // Many small values, each counted where it is made.
        (
            "small lists",
            "x = [for (i = [0:1999]) for (j = [0:1999]) [j]]; echo(len(x));".into(),
        ),
        (
            "nested lists",
            "x = [for (i = [0:1999]) [for (j = [0:1999]) [j]]]; echo(len(x));".into(),
        ),
        (
            "concat",
            "function g(v, n) = n == 0 ? v : g(concat(v, [[n, n]]), n - 1);\n\
             echo(len(g([], 3000000)));"
                .into(),
        ),
        (
            "short strings",
            "x = [for (i = [0:1999]) for (j = [0:1499]) str(\"item \", j)]; echo(len(x));".into(),
        ),
        (
            "doubling string",
            "function s(a, n) = n == 0 ? len(a) : s(str(a, a), n - 1); echo(s(\"ab\", 40));".into(),
        ),
        (
            "ranges",
            "x = [for (i = [0:1999]) for (j = [0:1499]) [0:j]]; echo(len(x));".into(),
        ),
        (
            "function literals",
            "x = [for (i = [0:1999]) for (j = [0:1499]) function(y) y + j]; echo(len(x));".into(),
        ),
        // Messages a host keeps, and nodes.
        ("echoes", "for (i = [0:1999], j = [0:1499]) echo(j);".into()),
        (
            "warnings",
            "for (i = [0:1999], j = [0:1499]) let (x = j + \"a\") cube(0);".into(),
        ),
        (
            "nodes",
            "for (i = [0:1999]) for (j = [0:999]) cube(1);".into(),
        ),
        // The text `chr()` builds is not a value until it is done.
        ("chr", tree("echo(len(chr(f([65], 30))));")),
    ]
}

/// Abort the whole test process past 1 GB resident: these programs are
/// built to exhaust memory, and a broken limit must not take the machine
/// with it.
fn watch_memory() {
    static START: Once = Once::new();
    START.call_once(|| {
        std::thread::spawn(|| {
            loop {
                let mb = rss_mb();
                if mb > 1024 {
                    eprintln!("memory_limit: {mb} MB resident, over the 1 GB guard; aborting");
                    std::process::abort();
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        });
    });
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

struct Run {
    lines: Vec<(DiagCode, String)>,
    ev: eval::Evaluation,
    exceeded: Option<eval::limits::Exceeded>,
}

/// Evaluate `src` under a memory limit of `memory` bytes, with the guard on
/// the interrupt flag the evaluator polls (as every host wires it) unless
/// `wired` is false.
fn run(src: &str, memory: u64, wired: bool) -> Run {
    watch_memory();
    let path = PathBuf::from("/nonexistent/test.scad");
    let program = lang::parse_file(path, src.as_bytes().to_vec());
    assert!(!program.has_syntax_errors(), "syntax error in {src}");
    let flag = Arc::new(AtomicBool::new(false));
    let limits = Limits {
        memory: Some(memory),
        ..Limits::NONE
    };
    let guard = Arc::new(Guard::new(limits, flag.clone(), None));
    let opts = Options {
        guard: Some(guard.clone()),
        interrupt: wired.then_some(flag),
        ..Options::default()
    };
    let mut out = Collect::default();
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
    Run {
        lines: out.lines.into_iter().map(|(_, c, t)| (c, t)).collect(),
        ev,
        exceeded: guard.exceeded(),
    }
}

fn assert_stopped(name: &str, r: &Run) {
    let limit = r
        .lines
        .iter()
        .find(|(c, _)| *c == DiagCode::ResourceLimit)
        .unwrap_or_else(|| panic!("{name}: no resource-limit error in {:?}", r.lines));
    assert!(
        limit.1.contains("memory limit of 64 MiB"),
        "{name}: {}",
        limit.1
    );
    assert!(r.ev.aborted, "{name}: not aborted");
    assert!(!r.ev.interrupted, "{name}: reported as a cancellation");
    assert_eq!(
        r.exceeded.as_ref().map(|e| e.limit),
        Some(Limit::Memory),
        "{name}"
    );
    // Only the limit and its call sites follow it: nothing computed from
    // a value cut short at the limit is printed.
    let at = r
        .lines
        .iter()
        .position(|(c, _)| *c == DiagCode::ResourceLimit)
        .unwrap();
    for (c, t) in &r.lines[at + 1..] {
        assert!(
            t.starts_with("called by") || *c == DiagCode::Trace,
            "{name}: {t}"
        );
    }
}

#[test]
fn runaway_values_stop_at_the_memory_limit() {
    // One after another on one thread, so the watchdog's reading is this
    // test's alone and the programs never overlap.
    for (name, src) in runaways() {
        let t0 = std::time::Instant::now();
        let r = run(&src, LIMIT, true);
        assert_stopped(name, &r);
        drop(r);
        assert!(
            t0.elapsed().as_secs_f64() < 30.0,
            "{name}: {:?}",
            t0.elapsed()
        );
    }
}

#[test]
fn the_limit_is_found_without_the_interrupt_flag() {
    // A host that gives the evaluator a guard but not its flag still gets
    // the limit: the operator and the printer stop at it, and the
    // evaluator reports it as soon as they return.
    for (name, src) in runaways().into_iter().take(8) {
        let r = run(&src, LIMIT, false);
        assert_stopped(name, &r);
    }
}

#[test]
fn values_under_the_limit_are_untouched() {
    // The same shapes, small: counting every value must not stop models
    // that fit, and the results are exact.
    let r = run(
        &format!(
            "{TREE}t = f([1], 10);\necho(len(-t), len(t + t), len(str(t)));\n\
             x = [for (i = [0:99]) for (j = [0:99]) [j]]; echo(len(x));"
        ),
        LIMIT,
        true,
    );
    assert!(!r.ev.aborted, "{:?}", r.lines);
    assert!(r.exceeded.is_none());
    let echoes: Vec<_> = r.lines.iter().map(|(_, t)| t.as_str()).collect();
    assert_eq!(echoes, ["2, 2, 7164", "10000"]);
}
