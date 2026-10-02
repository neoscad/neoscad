//! Replaying repeated module calls (`callmemo`) must be indistinguishable
//! from evaluating them. Every check evaluates a program with the memo on
//! and off and compares everything a host can see: the node tree (node
//! indices and origins included), the `.csg` export, every message with its
//! severity, code, location and text, in order, and the evaluation's flags.
//! Each hazard also checks the memo's counters, so a test cannot pass by
//! never reusing anything where reuse is expected, or pass by reusing where
//! it must not. A key's first call only notes it, its second is recorded,
//! and later ones replay (`CallMemo::can_record`).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use eval::limits::{Guard, Limits};
use eval::{CallStats, Options};
use lang::loader::StdFs;

#[derive(Default)]
struct Lines(Vec<String>);

impl eval::Output for Lines {
    fn message(&mut self, m: &eval::Message<'_>) {
        self.0.push(format!(
            "{:?} {:?} {:?}@{} {}",
            m.diag.severity,
            m.diag.code,
            m.diag.span,
            m.diag.line,
            String::from_utf8_lossy(m.text)
        ));
    }
}

/// What a host sees of one evaluation.
#[derive(Debug, PartialEq)]
struct Seen {
    tree: String,
    csg: String,
    lines: Vec<String>,
    flags: (bool, bool, bool),
}

fn eval_with(src: &str, opts: &Options) -> (Seen, CallStats) {
    let mut out = Lines::default();
    // Parsed on the big stack too: a test nests hundreds of statements.
    let ev = eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
        let path = PathBuf::from("/nonexistent/test.scad");
        let program = lang::parse_file(path, src.as_bytes().to_vec());
        assert!(!program.has_syntax_errors(), "syntax error in {src}");
        eval::evaluate(
            &program,
            &[],
            &[],
            PathBuf::from("/nonexistent"),
            opts,
            &mut out,
        )
    });
    let seen = Seen {
        tree: format!("{:?}", ev.root),
        csg: eval::dump::csg(&ev.root, Path::new("/nonexistent"), &StdFs),
        lines: out.0,
        flags: (ev.aborted, ev.interrupted, ev.hard_warning),
    };
    (seen, ev.calls)
}

/// Evaluate `src` with the memo on and off under `opts`; the two must
/// agree. Returns the memo's counters and the messages.
fn same_with(src: &str, opts: &Options) -> (CallStats, Vec<String>) {
    let (on, stats) = eval_with(
        src,
        &Options {
            call_memo: true,
            ..opts.clone()
        },
    );
    let (off, none) = eval_with(
        src,
        &Options {
            call_memo: false,
            ..opts.clone()
        },
    );
    assert_eq!(none, CallStats::default(), "the memo ran while off");
    assert_eq!(on.flags, off.flags, "flags differ for {src}");
    assert_eq!(on.lines, off.lines, "messages differ for {src}");
    assert_eq!(on.csg, off.csg, "csg differs for {src}");
    assert!(on.tree == off.tree, "node trees differ for {src}");
    (stats, on.lines)
}

fn same(src: &str) -> (CallStats, Vec<String>) {
    same_with(src, &Options::default())
}

/// A body with enough steps to be kept (`callmemo::MIN_WORK`).
const WORK: &str = "for (i = [0:39]) translate([i, 0, 0]) cube(1);";

#[test]
fn repeated_calls_replay() {
    let src =
        format!("module m(x) {{ {WORK} sphere(x); }}\nm(1); m(2); m(1); m(1); m(2); m(2); m(1);");
    let (s, _) = same(&src);
    assert_eq!(s.hits, 3, "{s:?}");
}

#[test]
fn dollar_fn_differs_between_call_sites() {
    // The body reads `$fn` (the sphere does, and so does the echo). The
    // same call under a different `$fn` must evaluate again, and a call
    // under the first `$fn` must still replay.
    let src = format!(
        "module m() {{ echo($fn); {WORK} sphere(1); }}
         module w() {{ $fn = 7; m(); }}
         m(); w(); m(); w(); m(); w(); m($fn = 7); m($fn = 7); m($fn = 7);"
    );
    let (s, lines) = same(&src);
    // The third of each. `m($fn = 7)` binds `$fn` in its own frame, so its
    // key differs from the `m()` inside `w()`, which reads it from `w`.
    assert_eq!(s.hits, 3, "{s:?}");
    let echoes: Vec<_> = lines.iter().filter(|l| l.starts_with("Echo ")).collect();
    assert_eq!(echoes.len(), 9, "{lines:?}");
    assert!(
        echoes[1].ends_with(" 7") && echoes[0].ends_with(" 0"),
        "{echoes:?}"
    );
}

#[test]
fn user_dollar_variable_read_inside_a_called_function() {
    // `$k` is read two calls down, in a function; replay must see it.
    let src = format!(
        "function f() = g();
         function g() = $k;
         module m() {{ echo(f()); {WORK} }}
         $k = 1;
         m(); m(); let($k = 2) m(); module s() {{ $k = 3; m(); }} s(); m(); let($k = 2) m();"
    );
    let (s, lines) = same(&src);
    // The last two: under `$k = 1` and `$k = 2`, one entry each.
    assert_eq!(s.hits, 2, "{s:?}");
    assert_eq!(lines.iter().filter(|l| l.starts_with("Echo ")).count(), 6);
}

#[test]
fn unbound_dollar_variable_is_a_dependency() {
    let src = format!(
        "module m() {{ echo($q); {WORK} }}
         m(); m(); let($q = 1) m(); m();"
    );
    let (s, lines) = same(&src);
    // The last `m()`: `$q` is unbound again, as when it was recorded.
    assert_eq!(s.hits, 1, "{s:?}");
    let echoes: Vec<_> = lines.iter().filter(|l| l.starts_with("Echo ")).collect();
    assert!(
        echoes[2].ends_with(" 1") && echoes[3].ends_with(" undef"),
        "{lines:?}"
    );
}

#[test]
fn calls_with_children_key_on_the_children() {
    let src = format!(
        "module c() {{ {WORK} children(); }}
         c() cube(1); c() sphere(1); c() cube(1); c(); c(); c();
         module d() {{ c() cube(2); }} d(); d(); d();
         for (i = [0:3]) c() cube(3);"
    );
    let (s, _) = same(&src);
    // The third `c();` (no children), the third `d()`, and the loop's
    // fourth `c() cube(3)` (a call with children is recorded at its third
    // call): each `c() ...` above is its own call site, and children from
    // different sites never share an entry. The loop's calls share one
    // although `i` differs, since the children never mention it.
    assert_eq!(s.hits, 3, "{s:?}");
}

#[test]
fn children_reading_through_a_local_module_are_keyed_on_what_it_reads() {
    // The children mention only `inner`, a module defined in `w`'s body
    // that reads `w`'s parameter: `k` must be in the key. (`n` makes every
    // `w` call distinct, so `c` is looked up each time rather than `w`
    // replaying whole.) In the loop, the children pass `j` to `inner2` as
    // an argument, so it is mentioned and keyed.
    let src = format!(
        "module c() {{ {WORK} children(); }}
         module w(k, n) {{ module inner() {{ echo(k); }} c() inner(); }}
         w(1, 0); w(1, 1); w(2, 2); w(1, 3); w(2, 4); w(2, 5); w(1, 6); w(2, 7);
         module inner2(x) {{ echo(x); }}
         for (j = [5, 6, 5, 6, 5, 6]) c() inner2(j);"
    );
    let (s, lines) = same(&src);
    assert!(s.hits >= 2, "{s:?}");
    let e = [
        "1", "1", "2", "1", "2", "2", "1", "2", "5", "6", "5", "6", "5", "6",
    ];
    assert_eq!(echoed(&lines), e);
}

/// The last word of each echo, in order.
fn echoed(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|l| l.starts_with("Echo "))
        .map(|l| l.rsplit(' ').next().unwrap_or_default().to_string())
        .collect()
}

#[test]
fn children_with_only_assignments_are_keyed() {
    // `children()` runs the children's assignments even with nothing to
    // instantiate, so the call sites differ. (The memo used to leave such
    // children out of the key and print the second site's echo for the
    // third and fourth.)
    let src = format!(
        r#"module foo() {{ {WORK} children(); }}
           function f(x) = echo(x) x;
           foo() {{ a = f(1); }} foo() {{ a = f(2); }} foo() {{ a = f(3); }} foo() {{ a = f(4); }}
           for (i = [0, 0, 0, 0]) foo() {{ a = f(5); }}"#
    );
    let (s, lines) = same(&src);
    assert_eq!(s.hits, 1, "{s:?}");
    assert_eq!(echoed(&lines), ["1", "2", "3", "4", "5", "5", "5", "5"]);
}

#[test]
fn children_see_the_caller_lexically() {
    // The children read the caller's parameter and the loop variable, so
    // calls differ exactly when those do.
    let src = format!(
        "module c() {{ {WORK} children(); }}
         module w(x) {{ c() echo(x); }}
         w(1); w(1); w(2); w(1); w(2); w(2);
         for (i = [0, 0, 1, 0, 1, 1, 0, 1]) c() echo(i);"
    );
    let (s, lines) = same(&src);
    // `w`'s third call of each argument, and the loop's fourth `c` call
    // of each value of `i`.
    assert_eq!(s.hits, 4, "{s:?}");
    let e = [
        "1", "1", "2", "1", "2", "2", "0", "0", "1", "0", "1", "1", "0", "1",
    ];
    assert_eq!(echoed(&lines), e);
}

#[test]
fn children_reaching_the_callers_children_are_keyed_on_them() {
    // `c`'s children are `{ children(); }` in `wrap`, which reach `site`'s
    // `echo(k)`: the same syntax at every `c` call, told apart only by a
    // frame two calls out. (`n` makes every `site` call distinct, so none
    // replays whole and `c` is looked up each time.)
    let src = format!(
        "module c() {{ {WORK} children(); }}
         module wrap() {{ c() children(); }}
         module site(k, n) {{ wrap() echo(k); }}
         site(1, 0); site(1, 1); site(2, 2); site(1, 3); site(2, 4); site(1, 5); site(2, 6); site(1, 7);"
    );
    let (s, lines) = same(&src);
    assert!(s.hits >= 2, "{s:?}");
    assert_eq!(echoed(&lines), ["1", "1", "2", "1", "2", "1", "2", "1"]);
}

#[test]
fn a_child_reading_a_dollar_variable_the_module_sets() {
    // `$v` is set inside the call and read only by the children: its value
    // follows the call's argument, which is in the key.
    let src = format!(
        "module s(v) {{ $v = v; {WORK} children(); }}
         for (k = [1, 1, 2, 1, 2, 2, 1, 2]) s(k) echo($v);"
    );
    let (s, lines) = same(&src);
    assert_eq!(s.hits, 2, "{s:?}");
    assert_eq!(echoed(&lines), ["1", "1", "2", "1", "2", "2", "1", "2"]);
}

#[test]
fn a_child_reading_a_dollar_variable_set_from_outside() {
    // The module sets `$v` from `$w`, bound outside the call, and only the
    // children read `$v`: the entry must depend on `$w`. A module between
    // the call and the children that sets `$v` again wins.
    let src = format!(
        "module s() {{ $v = $w + 1; {WORK} children(); }}
         module mid() {{ $v = 100; children(); }}
         module run(w) {{ $w = w; s() echo($v); }}
         module run2(w) {{ $w = w; s() mid() echo($v); }}
         run(1); run(1); run(2); run(1); run(2); run(2);
         run2(1); run2(1); run2(2); run2(1);"
    );
    let (s, lines) = same(&src);
    assert!(s.hits >= 3, "{s:?}");
    let e = ["2", "2", "3", "2", "3", "3", "100", "100", "100", "100"];
    assert_eq!(echoed(&lines), e);
}

/// BOSL2's attach and tag pattern, cut down: `attachable` publishes the
/// parent's size in `$parent_size`, `attach` places its children from it,
/// `tag` sets `$tag`, and `show` keeps only its children with that tag.
const ATTACH: &str = r#"
    module attachable(size) {
        $parent_size = size;
        for (i = [0:39]) translate([i, 0, 0]) cube(1);
        cube(size);
        children();
    }
    module attach(f) { translate($parent_size * f) children(); }
    module tag(t) { $tag = t; children(); }
    module show(t) { if ($tag == t) children(); }
    module part(s) { attachable(s) children(); }
"#;

#[test]
fn attach_and_tag_children_see_the_parent() {
    let src = format!(
        r#"{ATTACH}
           for (s = [1, 1, 2, 1, 2, 1, 2])
             part(s) attach(1) tag("a") {{
               show("a") cube($parent_size); show("b") sphere(s); echo($parent_size, $tag);
             }}
           for (t = ["a", "b", "a", "b", "a", "b", "a"])
             part(3) tag(t) {{ show("a") cube(1); show("b") sphere(1); echo($tag); }}"#
    );
    let (s, lines) = same(&src);
    assert!(s.hits >= 2, "{s:?}");
    let (a, b) = ("\"a\"", "\"b\"");
    let e = [a, a, a, a, a, a, a, a, b, a, b, a, b, a];
    assert_eq!(echoed(&lines), e);
}

#[test]
fn parent_module_and_dollar_children_in_children() {
    // `parent_module(1)` in the children names the module that calls them,
    // inside the call; `$children` there is the caller's, lexically.
    // `parent_module(2)` there looks past the call, which is not kept.
    let src = format!(
        "module c() {{ {WORK} children(); }}
         module a() {{ c() echo(parent_module(1), $children); }}
         module b() {{ c() echo(parent_module(1), $children); }}
         module via(n) {{ if (n == 0) a() cube(); else b() {{ cube(); sphere(); }} }}
         for (n = [0, 0, 1, 0, 1, 1]) via(n);
         module p() {{ c() echo(parent_module(2)); }}
         module q() {{ p(); }} module r() {{ p(); }}
         q(); r(); q(); r(); q(); r();"
    );
    let (s, lines) = same(&src);
    assert!(s.hits >= 2, "{s:?}");
    let e = &echoed(&lines)[6..];
    assert_eq!(e, ["\"q\"", "\"r\"", "\"q\"", "\"r\"", "\"q\"", "\"r\""]);
}

#[test]
fn messages_replay_in_order() {
    let src = format!(
        "module e(x) {{ echo(\"in\", x); {WORK} echo(\"out\", x); undefined_fn(); }}
         e(1); e(2); e(1); echo(\"between\"); e(1); e(2); e(2); e(1);"
    );
    let (s, lines) = same(&src);
    assert_eq!(s.hits, 3, "{s:?}");
    assert!(
        lines.iter().any(|l| l.contains("UnknownModule")),
        "{lines:?}"
    );
}

#[test]
fn rands_are_never_replayed() {
    let src = format!(
        "module r() {{ echo(rands(0, 1, 1)); {WORK} }}
         module w() {{ r(); }}
         r(); r(); w(); w(); module z() {{ echo(rands(0, 1, 1, 42)); {WORK} }} z(); z();"
    );
    let (s, lines) = same(&src);
    assert_eq!(s.hits, 0, "{s:?}");
    let echoes: Vec<_> = lines.iter().filter(|l| l.starts_with("Echo ")).collect();
    assert_ne!(echoes[0], echoes[1], "{echoes:?}");
}

#[test]
fn recursion_replays_subtrees() {
    let src = format!(
        "module rec(n) {{ if (n > 0) {{ rec(n - 1); translate([0, 0, n]) rec(n - 1); }} else {{ {WORK} }} }}
         rec(8);"
    );
    let (s, _) = same(&src);
    // At each depth the second call is recorded, and the calls inside it
    // were seen in the first: they replay.
    assert!(s.hits >= 6, "{s:?}");
}

#[test]
fn infinite_recursion_fails_the_same() {
    let src = format!("module inf(n) {{ {WORK} inf(n + 1); }} inf(0);");
    let (s, lines) = same(&src);
    assert_eq!(s.kept, 0, "{s:?}");
    assert!(lines.iter().any(|l| l.contains("Recursion")), "{lines:?}");
}

/// Statements run on the heap and take no native stack and so no frames:
/// the same calls under ever more nested `if`s all have the whole
/// budget, recorded and replayed alike.
#[test]
fn nested_statements_leave_the_budget_to_calls() {
    for n in (0..700).step_by(77) {
        let src = format!(
            "function f(k) = k == 0 ? 0 : 1 + f(k - 1);
             module heavy() {{ echo(f(150)); {WORK} }}
             heavy(); heavy(); {} heavy();",
            "if (true) ".repeat(n)
        );
        let opts = Options {
            frame_limit: 3000,
            ..Options::default()
        };
        let (_, lines) = same_with(&src, &opts);
        assert!(
            !lines.iter().any(|l| l.contains("Recursion")),
            "{n}: {lines:?}"
        );
    }
}

#[test]
fn parent_module_outside_the_call_is_not_keyed() {
    let src = format!(
        "module p() {{ echo(parent_module(1), $parent_modules); {WORK} }}
         module a() p(); module b() p();
         a(); b(); a(); p(); b(); p(); a(); b(); p();"
    );
    let (s, lines) = same(&src);
    // `a()` and `b()` repeat (their `p()` reads only inside them). Inside
    // them `p()` reads its caller's name and is not kept; at the top level
    // `parent_module(1)` finds no caller, which depends only on the depth
    // (`$parent_modules`, in the key), so the second one replays.
    assert_eq!(s.hits, 3, "{s:?} {lines:?}");
}

#[test]
fn function_values_are_not_keyed() {
    let src = format!(
        "module f(g) {{ echo(g(1)); {WORK} }}
         f(function(x) x + 1); f(function(x) x + 2); h = function(x) x * 3; f(h); f(h);
         module k() {{ echo($fv(2)); {WORK} }}
         $fv = function(x) x; k(); k();"
    );
    let (s, _) = same(&src);
    assert_eq!(s.hits, 0, "{s:?}");
}

#[test]
fn deprecation_prints_once() {
    let src = format!(
        "module d() {{ assign(x = 1) cube(x); {WORK} }}
         d(); d(); d();"
    );
    same(&src);
}

#[test]
fn errors_after_replays() {
    let src = format!(
        "module e(x) {{ {WORK} assert(x < 3, \"too big\"); }}
         e(1); e(1); e(1); e(2); e(5); e(1);"
    );
    let (s, lines) = same(&src);
    assert_eq!(s.hits, 1, "{s:?}");
    assert!(lines.iter().any(|l| l.contains("too big")), "{lines:?}");
}

#[test]
fn tags_and_origins_come_from_the_call() {
    let src = format!("module m() {{ {WORK} }}\nm(); #m(); %m(); m();\n!m();");
    let (s, lines) = same(&src);
    assert_eq!(s.hits, 3, "{s:?}");
    assert!(
        lines.is_empty() || lines.iter().all(|l| !l.contains("Root")),
        "{lines:?}"
    );
}

/// BOSL2's pattern: transform modules that update `$m` and call
/// `children()`, and a recursive module below them. The matrices do not
/// commute, so calls at the same depth under different paths see
/// different `$m` values.
const TRACKED: &str = "
    $m = [[1, 0], [0, 1]];
    module tr(k) { $m = $m * [[k, 1], [0, 1]]; translate([k, 0, 0]) children(); }
    module leaf() { for (i = [0:39]) translate([i, 0, 0]) cube(1); }
";

#[test]
fn a_transform_read_only_to_update_itself_keys_on_shape() {
    let src = format!(
        "{TRACKED}
         module t(d) {{ tr(2) if (d > 0) {{ t(d - 1); tr(3) t(d - 1); }} else leaf(); }}
         t(6);"
    );
    let (s, _) = same(&src);
    assert!(s.hits >= 6, "{s:?}");
}

#[test]
fn a_transform_read_any_other_way_keys_on_value() {
    // The leaf prints `$m`: every call sees a different one, so nothing
    // may replay across different values.
    let src = format!(
        "{TRACKED}
         module shown() {{ echo($m); leaf(); }}
         module t(d) {{ tr(2) if (d > 0) {{ t(d - 1); tr(3) t(d - 1); }} else shown(); }}
         t(4); t(4);"
    );
    let (s, lines) = same(&src);
    // Only the second `t(4)`, under the same `$m`, replays whole.
    assert!(s.hits >= 1, "{s:?}");
    assert_eq!(lines.iter().filter(|l| l.starts_with("Echo ")).count(), 32);
}

#[test]
fn a_transform_that_is_not_a_matrix_keys_on_value() {
    // An undefined or ragged `$m` makes `*` warn: its shape is not a
    // matrix's, so its value is the key.
    let src = format!(
        "module tr(k) {{ $m = $m * [[k, 0], [0, 1]]; children(); }}
         module t() {{ tr(2) {WORK} }}
         module u() {{ $m = [[1, 0], [0]]; t(); }}
         module v() {{ $m = [[1, 2], [3, 4]]; t(); }}
         t(); u(); t(); u(); v(); v(); t(); u(); v();"
    );
    let (s, lines) = same(&src);
    assert!(
        lines.iter().any(|l| l.contains("UndefinedOperation")),
        "{lines:?}"
    );
    assert!(s.hits >= 3, "{s:?}");
}

#[test]
fn recursion_with_children_skips_empty_children() {
    // Each `r` frame holds its caller's children, which are empty: the key
    // of `c`'s call inside does not follow them up the recursion, which
    // would cost a walk of every frame above on every call.
    let src = format!(
        "module c() {{ {WORK} children(); }}
         module r(d) {{ c() if (d > 0) r(d - 1); }}
         r(3); r(3); r(3);"
    );
    let (s, _) = same(&src);
    assert!(s.hits >= 1, "{s:?}");
}

#[test]
fn calls_with_children_are_the_same_at_any_thread_count() {
    // Evaluation is serial, but hosts run it on rayon pools of any size
    // (and geometry keys hash on them): reuse must not depend on that.
    let src = format!(
        r#"{ATTACH}
           module t(d) {{ part(d) attach(1) tag("a") if (d > 0) {{ t(d - 1); show("a") t(d - 1); }} }}
           t(4); t(4);"#
    );
    let run = |n: usize| {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(n)
            .build()
            .expect("pool");
        pool.install(|| eval_with(&src, &Options::default()))
    };
    let one = run(1);
    assert!(one.1.hits > 0, "{:?}", one.1);
    assert_eq!(one, run(8));
}

#[test]
fn memory_limit_near_a_replay() {
    // The body builds a big list and drops it. It is recorded, then called
    // again (at the same nesting, where replays are allowed) while a `let`
    // holds another big list: under limits around what the two cost
    // together, a replay must stop exactly where evaluation would.
    let src = format!(
        "module big(n) {{ l = [for (i = [0:n]) [i, i, i]]; echo(len(l)); {WORK} }}
         let (x = 0) big(20000); let (x = 0) big(20000);
         let (x = [for (i = [0:40000]) [i, i]]) big(20000);"
    );
    let (mut stopped, mut replayed) = (0, 0);
    let sizes = [2u64, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 14, 16, 20, 24, 32];
    for mb in sizes {
        // A guard per run: a tripped guard stays tripped.
        let run = |call_memo| {
            let flag = Arc::new(AtomicBool::new(false));
            let limits = Limits {
                memory: Some(mb << 20),
                ..Limits::NONE
            };
            let opts = Options {
                guard: Some(Arc::new(Guard::new(limits, flag.clone(), None))),
                interrupt: Some(flag),
                call_memo,
                ..Options::default()
            };
            eval_with(&src, &opts)
        };
        let ((on, stats), (off, _)) = (run(true), run(false));
        assert_eq!(on, off, "differs at {mb} MB");
        stopped += usize::from(off.flags.0);
        replayed += usize::from(stats.hits > 0);
    }
    assert!(
        stopped > 0 && stopped < sizes.len(),
        "stopped {stopped} times"
    );
    assert!(replayed > 0, "never replayed");
}

#[test]
fn hardwarnings_turn_the_memo_off() {
    let src = format!("module m() {{ {WORK} }} m(); m();");
    let opts = Options {
        hardwarnings: true,
        ..Options::default()
    };
    let (s, _) = same_with(&src, &opts);
    assert_eq!(s, CallStats::default());
}

#[test]
fn repeatable() {
    let src = format!(
        "{TRACKED}
         module t(d) {{ tr(2) if (d > 0) {{ t(d - 1); tr(3) t(d - 1); }} else leaf(); }}
         t(5); t(5);"
    );
    let a = eval_with(&src, &Options::default());
    let b = eval_with(&src, &Options::default());
    assert_eq!(a, b);
}
