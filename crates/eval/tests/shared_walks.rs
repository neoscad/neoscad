//! Walks over values whose lists and strings are shared: `==`, `<` and
//! the rest on lists, and `chr()`.
//!
//! A tail-recursive function doubling a list (`f([v, v], n - 1)`) builds a
//! tree with 2^n paths in n steps and almost no memory. Walking it path by
//! path made `t == t` and `t < t` take seconds at depth 26 and hours at 40,
//! in one operator call that neither the time limit nor a cancel could
//! stop. These check that the results and messages are still OpenSCAD's
//! (the expected lines are the nightly's `-o x.echo` output), that shared
//! trees and long shared strings now cost their distinct parts, and that a
//! comparison that is long for real stops at the time limit or a cancel.
//!
//! Every program here is small in memory; a watchdog aborts the process
//! past 1 GB resident all the same, so a regression cannot fill the swap.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use eval::Options;
use eval::limits::{Clock, Guard, Limit, Limits};
use lang::diag::DiagCode;

const TREE: &str = "function f(v, n) = n == 0 ? v : f([v, v], n - 1);\n";

fn watch_memory() {
    static START: Once = Once::new();
    START.call_once(|| {
        std::thread::spawn(|| {
            loop {
                let mb = rss_mb();
                if mb > 1024 {
                    eprintln!("shared_walks: {mb} MB resident, over the 1 GB guard; aborting");
                    std::process::abort();
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
    });
}

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

/// Collects messages as `LABEL: text @line`, and calls `on_echo` with each
/// echo's text as it is printed.
struct Lines<F: FnMut(&str)> {
    lines: Vec<(DiagCode, String)>,
    on_echo: F,
}

impl<F: FnMut(&str)> eval::Output for Lines<F> {
    fn message(&mut self, m: &eval::Message<'_>) {
        let text = String::from_utf8_lossy(m.text);
        if m.diag.code == DiagCode::Echo {
            (self.on_echo)(&text);
        }
        let mut s = format!("{}: {text}", m.diag.severity.openscad_label());
        if m.diag.span.is_some() {
            s.push_str(&format!(" @{}", m.diag.line));
        }
        self.lines.push((m.diag.code, s));
    }
}

fn evaluate(
    src: &str,
    opts: &Options,
    on_echo: impl FnMut(&str) + Send,
) -> (Vec<(DiagCode, String)>, eval::Evaluation) {
    watch_memory();
    let program = lang::parse_file(
        PathBuf::from("/nonexistent/test.scad"),
        src.as_bytes().to_vec(),
    );
    assert!(!program.has_syntax_errors(), "syntax error in {src}");
    let mut out = Lines {
        lines: Vec::new(),
        on_echo,
    };
    let ev = eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
        eval::evaluate(
            &program,
            &[],
            &[],
            PathBuf::from("/nonexistent"),
            opts,
            &mut out,
        )
    });
    (out.lines, ev)
}

/// The printed lines of `src`, which must finish within `secs` (in a debug
/// build).
fn run(src: &str, secs: f64) -> Vec<String> {
    let t0 = Instant::now();
    let (lines, ev) = evaluate(src, &Options::default(), |_| {});
    let dt = t0.elapsed().as_secs_f64();
    assert!(dt < secs, "took {dt:.1} s");
    assert!(!ev.aborted, "{lines:?}");
    lines.into_iter().map(|(_, s)| s).collect()
}

#[test]
fn comparisons_match_the_nightly() {
    // Nested, shared and mixed lists, NaN, undef, functions and ranges,
    // with the nightly's results and warnings. `a == a` is false for a
    // list holding NaN, the same list or not: no identity shortcut.
    let src = r#"function f(v, n) = n == 0 ? v : f([v, v], n - 1);
function g(v, n) = n == 0 ? v : g([v], n - 1);
nan = 0/0;
a = [nan];
echo(a == a, a != a, [a] == [a], a < a, a <= a, a > a, a >= a);
t = f([1], 12);
u = f([1], 12);
w = f([2], 12);
echo(t == t, t != t, t == u, t < u, t <= u, t < w, t > w, w > t, t >= w);
m = f([1, nan], 6);
echo(m == m, m < m, m <= m);
d = f([undef], 4);
echo(d == d);
echo(d < d);
echo(d >= d);
e = g([undef], 5);
echo(e < e);
echo(e == e);
x = f([1, "a"], 3);
y = f([1, 2], 3);
echo(x == y, x < y);
echo(y < x);
echo(x <= y);
fn1 = function(q) q;
echo([fn1] == [fn1], [fn1] < [fn1]);
echo(f([fn1], 3) == f([fn1], 3));
echo(f([fn1], 3) < f([fn1], 3));
r = [0:1:3];
echo(f([r], 3) == f([r], 3), f([r], 3) < f([r], 3), f([r], 3) <= f([r], 3));
nr = [nan:1:3];
echo([nr] == [nr], [nr] < [nr]);
echo(f([1], 5) == f([1], 6), f([1], 5) < f([1], 6), f([[1]], 5) < f([1], 6));
echo([1, 2, 3] < [1, 2], [1, 2] < [1, 2, 3], [] < [], [] == []);
echo([[1, 2], [3]] < [[1, 2], [3, 0]], [true, 1] < [true, 2], ["a", [1]] < ["a", [0]]);
echo([1, [2, undef]] < [1, [2, undef]]);
echo([[1, "x"], 2] < [[1, 3], 2]);
echo([1, 2] == [1, 2, 3], [1, [2, [3, [nan]]]] == [1, [2, [3, [nan]]]]);
echo(g([1], 30) == g([1], 30), g([1], 30) < g([1], 30), g([1], 30) <= g([2], 30));
echo(t == u ? "same" : "differ", t < f([1], 11));
"#;
    let idx = |n: usize, at: usize| {
        let mut s = String::new();
        for _ in 0..n {
            s.push_str(&format!("\n\tin vector comparison at index {at}"));
        }
        s
    };
    let expected = [
        "ECHO: false, true, false, false, true, false, true".to_string(),
        "ECHO: true, false, true, false, true, true, false, true, false".into(),
        "ECHO: false, false, true".into(),
        "ECHO: true".into(),
        format!(
            "WARNING: operation undefined (undefined < undefined){} @14",
            idx(5, 0)
        ),
        "ECHO: undef".into(),
        format!(
            "WARNING: operation undefined (undefined < undefined){} @15",
            idx(5, 0)
        ),
        "ECHO: undef".into(),
        format!(
            "WARNING: operation undefined (undefined < undefined){} @17",
            idx(6, 0)
        ),
        "ECHO: undef".into(),
        "ECHO: true".into(),
        format!(
            "WARNING: undefined operation (string < number){}{} @21",
            idx(1, 1),
            idx(3, 0)
        ),
        "ECHO: false, undef".into(),
        format!(
            "WARNING: undefined operation (number < string){}{} @22",
            idx(1, 1),
            idx(3, 0)
        ),
        "ECHO: undef".into(),
        format!(
            "WARNING: undefined operation (number < string){}{} @23",
            idx(1, 1),
            idx(3, 0)
        ),
        "ECHO: undef".into(),
        format!(
            "WARNING: operation undefined (function < function){} @25",
            idx(1, 0)
        ),
        "ECHO: true, undef".into(),
        "ECHO: true".into(),
        format!(
            "WARNING: operation undefined (function < function){} @27",
            idx(4, 0)
        ),
        "ECHO: undef".into(),
        "ECHO: true, false, true".into(),
        "ECHO: true, false".into(),
        format!(
            "WARNING: undefined operation (number < vector){} @32",
            idx(6, 0)
        ),
        "ECHO: false, undef, true".into(),
        "ECHO: false, true, false, true".into(),
        "ECHO: true, true, false".into(),
        format!(
            "WARNING: operation undefined (undefined < undefined){} @35",
            idx(2, 1)
        ),
        "ECHO: undef".into(),
        format!(
            "WARNING: undefined operation (string < number){}{} @36",
            idx(1, 1),
            idx(1, 0)
        ),
        "ECHO: undef".into(),
        "ECHO: false, false".into(),
        "ECHO: true, false, true".into(),
        format!(
            "WARNING: undefined operation (vector < number){} @39",
            idx(12, 0)
        ),
        "ECHO: \"same\", undef".into(),
    ];
    assert_eq!(run(src, 20.0), expected);
}

#[test]
fn shared_trees_cost_their_distinct_lists() {
    // Depth 40: 2^40 paths. The nightly takes the same results at depth 12
    // (checked there: `true, false, true, false, true, false`); the shape
    // of the answer does not depend on the depth.
    for n in [12, 40] {
        let src = format!(
            "{TREE}t = f([1], {n});\necho(t == t, t != t, t == f([1], {n}), t < t, t <= f([1], {n}), t > f([2], {n}));"
        );
        assert_eq!(
            run(&src, 5.0),
            ["ECHO: true, false, true, false, true, false"],
            "depth {n}"
        );
    }
    // Unshared but nested thirty deep: OpenSCAD's `<` asks each level twice
    // (`x[i] < y[i]`, then `y[i] < x[i]`), 2^30 steps; one walk answers both.
    let src = "function g(v, n) = n == 0 ? v : g([v], n - 1);\necho(g([1], 30) < g([1], 30), g([1], 30) <= g([1], 30));";
    assert_eq!(run(src, 5.0), ["ECHO: false, true"]);
}

#[test]
fn long_shared_strings_compare_once() {
    // As the nightly prints it, then the same with a million elements
    // holding one 1 MiB string each, compared with another equal string.
    let small = r#"function rep(s, n) = n == 0 ? s : rep(str(s, s), n - 1);
s = rep("abcdefgh", 7);
u = rep("abcdefgh", 7);
v = str(rep("abcdefgh", 6), rep("abcdefgi", 6));
x = [for (i = [0:99]) s];
y = [for (i = [0:99]) u];
z = [for (i = [0:99]) i == 99 ? v : u];
echo(len(s), x == y, x != y, x < y, x <= y, x == z, x < z, z < x, x >= z);
echo([s, 1] < [u, 2], [s, [s]] == [u, [u]], [[s], 1] > [[v], 0]);
"#;
    assert_eq!(
        run(small, 10.0),
        [
            "ECHO: 1024, true, false, false, true, false, true, false, false",
            "ECHO: true, true, false",
        ]
    );
    let big = r#"function rep(s, n) = n == 0 ? s : rep(str(s, s), n - 1);
s = rep("abcdefgh", 17);
u = rep("abcdefgh", 17);
x = [for (i = [0:999998]) s];
y = [for (i = [0:999998]) u];
echo(x == y, x < y, x <= y);
"#;
    assert_eq!(run(big, 20.0), ["ECHO: true, false, true"]);
}

#[test]
fn chr_skips_what_prints_nothing() {
    // The nightly prints `"", "ABC", "HiHiHiHi"` at depth 12.
    let src = format!(
        "{TREE}echo(chr(f([0], 40)), chr([65, [66, f([0], 40), 67]]), chr(f([72, 105], 2)));"
    );
    assert_eq!(run(&src, 5.0), ["ECHO: \"\", \"ABC\", \"HiHiHiHi\""]);
    // A range too long to expand prints nothing but warns, and the nightly
    // warns at every occurrence, shared or not: such a list is not skipped.
    let src =
        format!("{TREE}r = [0:1:2e6];\necho(chr(f([r, 0], 2)));\necho(chr(f([[0, 0], 65], 2)));");
    let warning = "WARNING: Bad range parameter in for statement: too many elements (2000001).";
    assert_eq!(
        run(&src, 5.0),
        [
            warning,
            warning,
            warning,
            warning,
            "ECHO: \"\"",
            "ECHO: \"AAAA\""
        ]
    );
}

/// Pairs of lists that are all distinct: `x[i][j]` and `y[i][j]` are
/// different lists for every `(i, j)`, so remembering walked pairs does not
/// help, and `x == y` takes 1,200 * 1,200 * 1,200 steps (seconds in a
/// release build, minutes in a debug one) over values of about 100 MB.
const LONG: &str = r#"N = 1200;
P = [for (j = [0:N-1]) [for (k = [0:N-1]) 1]];
R = [for (j = [0:N-1]) [for (k = [0:N-1]) 1]];
x = [for (i = [0:N-1]) [for (j = [0:N-1]) P[(i + j) % N]]];
y = [for (i = [0:N-1]) [for (j = [0:N-1]) R[(2 * i + j) % N]]];
echo("built");
echo(x == y, x < y);
echo("after");
"#;

/// Run [`LONG`], and once it has printed "built" and `delay` has passed,
/// call `act` (which runs the clock out, or cancels). Returns the lines,
/// the evaluation, the guard, and the time from `act` to the end.
fn stop_after_built(
    act: impl Fn(&AtomicU64, &AtomicBool) + Send + 'static,
    time_limit: bool,
) -> (
    Vec<(DiagCode, String)>,
    eval::Evaluation,
    Arc<Guard>,
    Duration,
) {
    let now = Arc::new(AtomicU64::new(0));
    let flag = Arc::new(AtomicBool::new(false));
    let clock: Clock = {
        let now = now.clone();
        Arc::new(move || now.load(Ordering::Relaxed) as f64)
    };
    let limits = Limits {
        time: time_limit.then_some(10.0),
        ..Limits::NONE
    };
    let guard = Arc::new(Guard::new(limits, flag.clone(), Some(clock)));
    let opts = Options {
        guard: Some(guard.clone()),
        interrupt: Some(flag.clone()),
        ..Options::default()
    };
    let acted = Arc::new(std::sync::Mutex::new(None::<Instant>));
    let (tx, rx) = std::sync::mpsc::channel::<()>();
    let helper = {
        let acted = acted.clone();
        let (now, flag) = (now.clone(), flag.clone());
        std::thread::spawn(move || {
            if rx.recv().is_ok() {
                // Well inside the comparison, which takes seconds.
                std::thread::sleep(Duration::from_millis(200));
                act(&now, &flag);
                *acted.lock().unwrap() = Some(Instant::now());
            }
        })
    };
    let (lines, ev) = evaluate(LONG, &opts, move |t| {
        if t == "\"built\"" {
            let _ = tx.send(());
        }
    });
    let end = Instant::now();
    helper.join().unwrap();
    let at = acted
        .lock()
        .unwrap()
        .expect("the comparison ended before the stop");
    (lines, ev, guard, end.saturating_duration_since(at))
}

#[test]
fn a_long_comparison_stops_at_the_time_limit() {
    let (lines, ev, guard, after) = stop_after_built(
        // Past the 10 s limit on the evaluation's clock.
        |now, _| now.store(60_000, Ordering::Relaxed),
        true,
    );
    let texts: Vec<_> = lines.iter().map(|(_, t)| t.as_str()).collect();
    assert_eq!(texts[0], "ECHO: \"built\"", "{texts:?}");
    assert!(
        texts[1].starts_with(
            "ERROR: Resource limit exceeded: the request ran longer than the time limit of 10 s"
        ),
        "{texts:?}"
    );
    assert!(
        texts[2..].iter().all(|t| t.starts_with("TRACE:")),
        "{texts:?}"
    );
    assert!(ev.aborted && !ev.interrupted);
    assert_eq!(guard.exceeded().map(|e| e.limit), Some(Limit::Time));
    assert!(
        after < Duration::from_secs(2),
        "stopped {after:?} after the deadline"
    );
}

#[test]
fn a_long_comparison_stops_when_cancelled() {
    let (lines, ev, guard, after) =
        stop_after_built(|_, flag| flag.store(true, Ordering::Relaxed), false);
    let texts: Vec<_> = lines.iter().map(|(_, t)| t.as_str()).collect();
    assert_eq!(texts, ["ECHO: \"built\""]);
    assert!(ev.interrupted);
    assert!(guard.exceeded().is_none());
    assert!(
        after < Duration::from_secs(2),
        "stopped {after:?} after the cancel"
    );
}
