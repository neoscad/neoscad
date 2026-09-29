//! OpenSCAD's experimental features that neoscad implements (`--enable
//! textmetrics`, `object-function`, `import-function`, `vector-swizzle`),
//! checked line for line against the 2026.09.23 nightly: every expected
//! block below is the nightly's `-o x.echo` output for the same program
//! and the same `--enable` flags, run in a directory holding the same
//! JSON files, with that directory written as `/nonexistent/` and each
//! message's ` in file x.scad, line N` as ` @N`.
//!
//! The programs cover what the conformance cases do not: echo and `str()`
//! formatting, equality (key order, NaN, methods), indexing, iteration,
//! `len`, the `is_*` functions, every `object()` warning, methods and
//! `this`, text and font metrics across fonts, alignments and directions,
//! nlohmann's JSON error messages, swizzles, and the "not enabled"
//! warnings with the features off.
//!
//! Every program is small in memory; a watchdog aborts the process past
//! 1 GB resident all the same.

use std::path::PathBuf;
use std::sync::{Arc, Once};
use std::time::{Duration, Instant};

use eval::{Features, Options};
use lang::diag::DiagCode;

fn watch_memory() {
    static START: Once = Once::new();
    START.call_once(|| {
        std::thread::spawn(|| {
            loop {
                let mb = rss_mb();
                if mb > 1024 {
                    eprintln!("experimental: {mb} MB resident, over the 1 GB guard; aborting");
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

/// Messages as `LABEL: text @line` (no `@` for a message without a
/// location, as echo lines are).
struct Lines(Vec<String>);

impl eval::Output for Lines {
    fn message(&mut self, m: &eval::Message<'_>) {
        let text = String::from_utf8_lossy(m.text);
        let mut s = format!("{}: {text}", m.diag.severity.openscad_label());
        if m.diag.span.is_some() && m.diag.code != DiagCode::Echo {
            s.push_str(&format!(" @{}", m.diag.line));
        }
        self.0.push(s);
    }
}

/// Evaluate `src` as `/nonexistent/test.scad` with the `--enable` names
/// `features`, the bundled fonts, and `files` in `/nonexistent/`.
fn run(src: &str, features: &[&str], files: &[(&str, &[u8])]) -> Vec<String> {
    watch_memory();
    let fs = lang::vfs::MemFs::new();
    for (name, data) in files {
        fs.insert(format!("/nonexistent/{name}"), data.to_vec());
    }
    let fs: Arc<dyn lang::loader::FileSystem + Send + Sync> = Arc::new(fs);
    let mut db = text::FontDb::with_fs(fs.clone());
    assets::add_fonts(&mut db);
    let opts = Options {
        features: Features::from_names(features),
        fs,
        fonts: Some(Arc::new(db)),
        ..Options::default()
    };
    let program = lang::parse_file(
        PathBuf::from("/nonexistent/test.scad"),
        src.as_bytes().to_vec(),
    );
    assert!(!program.has_syntax_errors(), "syntax error in {src}");
    let mut out = Lines(Vec::new());
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
    assert!(!ev.aborted, "{:?}", out.0);
    out.0
}

fn assert_lines(got: &[String], expected: &str) {
    let got = got.join("\n");
    if got != expected {
        for (i, (g, e)) in got.lines().zip(expected.lines()).enumerate() {
            if g != e {
                panic!("line {}:\n  got:      {g}\n  expected: {e}", i + 1);
            }
        }
        panic!(
            "got {} lines, expected {}:\n{got}",
            got.lines().count(),
            expected.lines().count()
        );
    }
}

/// Objects share their values as lists do, so the walks that had to
/// become linear over shared lists (`==`, printing, the memo digest) must
/// be linear over shared objects too: a tree of depth 40 whose fields are
/// one object is 2^40 paths.
#[test]
fn shared_object_trees_compare_and_print_in_linear_time() {
    let src = r#"function t(v, n) = n == 0 ? v : t(object(a=v, b=v), n - 1);
function u(v, n) = n == 0 ? v : u(object(a=v, b=[v]), n - 1);
x = t(object(z=1), 40);
y = t(object(z=1), 40);
w = t(object(z=2), 40);
echo(x == x, x == y, x == w, x != y);
p = u(object(z=1), 40);
q = u(object(z=1), 40);
echo(p == q, [p] == [q]);
echo(len(str(t(object(z=1), 12))));
"#;
    let t0 = Instant::now();
    let lines = run(src, &["object-function"], &[]);
    let dt = t0.elapsed().as_secs_f64();
    assert!(dt < 10.0, "took {dt:.1} s");
    // `str()` of the depth-12 tree prints its 4096 leaves (the nightly's length).
    assert_eq!(
        lines,
        [
            "ECHO: true, true, false, false",
            "ECHO: true, true",
            "ECHO: 102385",
        ]
    );
}
/// The JSON files `import()` reads in the JSON case, as the nightly read them.
const JSON_FILES: &[(&str, &[u8])] = &[
    (r#"bad1.json"#, b"{\"a\": tru}"),
    (r#"bad10.json"#, b"[\"\\x\"]"),
    (r#"bad11.json"#, b"[\"\\u12\"]"),
    (r#"bad12.json"#, b"[\"\\ud800x\"]"),
    (r#"bad13.json"#, b"[\"\\udc00\"]"),
    (r#"bad14.json"#, b"[\"a\tb\"]"),
    (r#"bad15.json"#, b"[\"abc"),
    (r#"bad16.json"#, b"\xef\xbb[1]"),
    (r#"bad17.json"#, b"{\"a\":1"),
    (r#"bad18.json"#, b"["),
    (r#"bad19.json"#, b"{1:2}"),
    (r#"bad2.json"#, b"{\"a\": 1,}"),
    (r#"bad20.json"#, b"[01]"),
    (r#"bad21.json"#, b"[\"\xc3(\"]"),
    (r#"bad22.json"#, b"nul"),
    (r#"bad23.json"#, b"{\"a\":[1,2}"),
    (r#"bad3.json"#, b"[1,\n  2,\n  @]"),
    (r#"bad4.json"#, b"{\"a\" 1}"),
    (r#"bad5.json"#, b"[1 2]"),
    (r#"bad6.json"#, b"[-]"),
    (r#"bad7.json"#, b"[1.]"),
    (r#"bad8.json"#, b"[1e]"),
    (r#"bad9.json"#, b"[1e+]"),
    (r#"badutf.json"#, b"{\"a\": \"\xff\"}"),
    (r#"bom.json"#, b"\xef\xbb\xbf[1,2]"),
    (r#"comment.json"#, b"// c\n[1]"),
    (r#"d1.json"#, b"{\"b\": 1, \"a\": [1, 2.5, -0, -0.0, 1e400, 12345678901234567890, 123456789012345678901234567890, 0.1, 1E2, true, null, \"x\\u00e9\\ud83d\\ude00\\n\\\"q\"], \"a\": 7, \"\": {}, \"Z\": [], \"long\": 9007199254740993}  trailing garbage"),
    (r#"d2.json"#, b"{\"b\": 1, \"a\": [1, 2.5, -0, -0.0, 1e-400, 12345678901234567890, 123456789012345678901234567890, 0.1, 1E2, true, null, \"x\\u00e9\\ud83d\\ude00\\n\\\"q\\t\\u0000z\"], \"a\": 7.5, \"\": {}, \"Z\": [], \"long\": 9007199254740993, \"neg\": -9223372036854775809, \"big\": -9223372036854775808, \"arr\": [[[]]], \"\\u00e9\": 1, \"e\": 1}"),
    (r#"empty.json"#, b""),
    (r#"nul.json"#, b"\x00"),
    (r#"str.json"#, b"  \"str\"  "),
    (r#"two.json"#, b"[1, 2] [3]"),
    (r#"ws.json"#, b"  \n\n  "),
    (r#"up.JSON"#, b"{\"x\":1}"),
];

#[test]
fn objects_match_the_nightly() {
    let src = r#"o = object(a=1, b="x", c=[1,"y"], d=object(e=undef), f=function(this) this.a);
echo(o);
echo(str("pre", o, "post"));
echo(o["b"], o.b, o.zz, o[0], o[undef], o["f"]());
echo(len(o), len(object()), o ? 1 : 0, object() ? 1 : 0);
echo(o == o, o == object(o), object(a=1) == object(a=1), object(a=1, b=2) == object(b=2, a=1));
echo(o < o);
echo([o] < [o]);
echo(o + 1);
x = object(a=0/0); echo(x == x, x == object(a=0/0), [x] == [x]);
echo(o.f == o.f, o.f == object(o).f);
echo(is_list(o), is_string(o), is_num(o), is_bool(o), is_undef(o), is_function(o));
echo([for (k = o) k], [each o]);
for (k = object(p=1, q=2)) echo(k);
echo(object(o, [["a", 5], ["b"]]), object(o, [["a", 5]]).f());
echo(object(this=1, $fn=3), object($fn=3).$fn);
echo(object(a=1, a=2), object(a=1, [["a", 3]], a=4), object([["k"]]));
echo(object(a="q\"z"), str(object(a="q\"z")));
echo(has_key(o, "a"), has_key(o, "zz"), has_key(o), has_key(1, "a"), has_key(o, 1));
echo(object(1));
echo(object([[true, 5]]));
echo(object([1]), "never", echo("not evaluated") 1);
echo(object([[]]), object([[1, 2, 3]]));
echo(max(o), norm(o));
echo(chr(o), search("a", o));
m = object(a=42, f=function(this) this.a, g=function(n, this) n > 1 ? n * this.g(n - 1) : 1);
echo(m.f(), m.g(5), (m.f)(), m["g"](4));
h = m.f; echo(h());
n = object(m, [["a", 7]]); echo(n.f(), m.f(), n.f == m.f);
k = object(m, [["a", 1]]).f; echo(k());
echo(object(p=function(this) this).p() == object(p=function(this) this).p());
q = object(p=function(this) this); echo(q.p() == q);
echo(object(this=42, f=function(this) this.this).f());
fthis = function(this) this; echo(fthis(), object(f=fthis).f() == undef);
echo(object(f=function(x, this) [x, is_object(this)]).f(9));
echo(object(f=function(this=echo("default") 1) this.z, z=3).f());
echo(object(f=function(this) this.z, z=3).f(99));
big = object([for (i = [0:20]) [str("k", i), i]]);
echo(big.k20, big["k0"], len(big), has_key(big, "k21"));
echo(object(big, [["k3"]]).k4, len(object(big, [["k3"]])));
"#;
    let expected = r#"ECHO: { a = 1; b = "x"; c = [1, "y"]; d = { e = undef; }; f = function(this) this.a; }
ECHO: "pre{ a = 1; b = "x"; c = [1, "y"]; d = { e = undef; }; f = function(this) this.a; }post"
ECHO: "x", "x", undef, undef, undef, 1
ECHO: 5, 0, 1, 0
ECHO: true, false, true, false
WARNING: operation undefined (object < object) @7
ECHO: undef
WARNING: operation undefined (object < object)
	in vector comparison at index 0 @8
ECHO: undef
WARNING: undefined operation (object + number) @9
ECHO: undef
ECHO: true, false, true
ECHO: true, false
ECHO: false, false, false, false, false, false
ECHO: ["a", "b", "c", "d", "f"], [{ a = 1; b = "x"; c = [1, "y"]; d = { e = undef; }; f = function(this) this.a; }]
ECHO: "p"
ECHO: "q"
ECHO: { a = 5; c = [1, "y"]; d = { e = undef; }; f = function(this) this.a; }, 5
ECHO: { this = 1; $fn = 3; }, 3
ECHO: { a = 2; }, { a = 4; }, { }
ECHO: { a = "q"z"; }, "{ a = "q"z"; }"
WARNING: has_key() number of parameters does not match: expected 2, found 1 @19
WARNING: has_key() parameter could not be converted: argument 0: expected object, found number (1) @19
WARNING: has_key() parameter could not be converted: argument 1: expected string, found number (1) @19
ECHO: true, false, undef, undef, undef
WARNING: object(Argument 0 <number>) An unnamed argument must be either <object> or <list>, it is <number>.  @20
ECHO: undef
WARNING: object(Argument 0 [Element 0 [<bool>,value]]) The key of the entry is not <string> but <bool>. In an unnamed list, entries must be [key,value] to set or [key] to delete. The key must be <string>. @21
ECHO: undef
WARNING: object( Argument 0 [Element 0 <number>] ) Entry type is not a list, it is <number>. In an unnamed list, entries must be [key,value] to set or [key] to delete. The key must be <string>. @22
ECHO: "not evaluated"
ECHO: undef, "never", 1
WARNING: object(Argument 0 [Element 0 []]) Entry is empty. In an unnamed list, entries must be [key,value] to set or [key] to delete. The key must be <string>. @23
WARNING: object(Argument 0 [Element 0 [...]]) Entry length is 3, must be 1 [key] or 2 [key,value]. In an unnamed list, entries must be [key,value] to set or [key] to delete. The key must be <string>. @23
ECHO: undef, undef
WARNING: max() parameter could not be converted: argument 0: expected number, found object ({ a = 1; b = "x"; c = [1, "y"]; d = { e = undef; }; f = function(this) this.a; }) @24
WARNING: norm() parameter could not be converted: argument 0: expected vector, found object ({ a = 1; b = "x"; c = [1, "y"]; d = { e = undef; }; f = function(this) this.a; }) @24
ECHO: undef, undef
ECHO: "", []
ECHO: 42, 120, 42, 24
ECHO: 42
ECHO: 7, 42, false
ECHO: 1
ECHO: false
ECHO: true
ECHO: 42
ECHO: undef, false
ECHO: [9, true]
ECHO: 3
ECHO: 3
ECHO: 20, 0, 21, false
ECHO: 4, 20"#;
    assert_lines(
        &run(src, &["object-function", "textmetrics"], &[]),
        expected,
    );
}

#[test]
fn text_metrics_match_the_nightly() {
    let src = r#"echo(textmetrics("Hello, World!"));
echo(textmetrics("Wg", size=7.5, font="Liberation Serif:style=Bold Italic"));
echo(textmetrics("abc", font="Liberation Mono", spacing=1.5, halign="center", valign="center"));
echo(textmetrics("abc", halign="right", valign="top"));
echo(textmetrics("abc", halign="left", valign="bottom"));
echo(textmetrics("abc", halign="middle", valign="up"));
echo(textmetrics("Vertical", direction="ttb"));
echo(textmetrics("Vertical", direction="btt", halign="center", valign="center"));
echo(textmetrics("Vertical", direction="ttb", valign="baseline"));
echo(textmetrics("rtl text", direction="rtl"));
echo(textmetrics("   "));
echo(textmetrics(""));
echo(textmetrics("x", em=12));
echo(textmetrics("x", em=12, size=3));
echo(textmetrics("AVAVA", font="Liberation Sans:style=Bold"));
echo(textmetrics("fi fl ffi"));
echo(textmetrics("éèê ЖИ"));
echo(textmetrics("مرحبا"));
echo(textmetrics("x", font="No Such Font Anywhere"));
echo(textmetrics(size=4));
echo(textmetrics("q", 4, "Liberation Mono"));
echo(fontmetrics());
echo(fontmetrics(3));
echo(fontmetrics(font="Liberation Serif"));
echo(fontmetrics(font="Liberation Mono:style=Bold", size=2.5));
echo(fontmetrics(font="Liberation Sans Narrow"));
echo(fontmetrics(em=10));
echo(fontmetrics(font="No Such Font Anywhere"));
echo(is_object(textmetrics("a")), is_object(1), is_object(undef));
tm = textmetrics("OpenSCAD", size=10);
echo(tm.size.x, tm.advance[0], tm["ascent"] - tm.descent);
"#;
    let expected = r#"ECHO: { position = [1.1392, -1.7792]; size = [76.8444, 11.8464]; ascent = 10.0672; descent = -1.7792; offset = [0, 0]; advance = [79.244, 0]; }
ECHO: { position = [0.7152, -2.2176]; size = [13.4813, 9.0384]; ascent = 6.8208; descent = -2.2176; offset = [0, 0]; advance = [14.4705, 0]; }
ECHO: { position = [-17.8891, -5.104]; size = [31.4681, 10.208]; ascent = 10.0672; descent = -0.1408; offset = [-18.7531, -4.9632]; advance = [37.5062, 0]; }
ECHO: { position = [-21.8043, -10.208]; size = [21.439, 10.208]; ascent = 10.0672; descent = -0.1408; offset = [-22.3931, -10.0672]; advance = [22.3931, 0]; }
ECHO: { position = [0.5888, 0]; size = [21.439, 10.208]; ascent = 10.0672; descent = -0.1408; offset = [0, 0.1408]; advance = [22.3931, 0]; }
WARNING: Unknown value for the halign parameter (use "left", "right" or "center"): 'middle' @6
WARNING: Unknown value for the valign parameter (use "baseline", "bottom", "top" or "center"): 'up' @6
ECHO: { position = [0.5888, -0.1408]; size = [21.439, 10.208]; ascent = 10.0672; descent = -0.1408; offset = [0, 0]; advance = [22.3931, 0]; }
ECHO: { position = [-4.5743, -102.79]; size = [9.1456, 101.054]; ascent = 10.0672; descent = -0.1408; offset = [0, 0]; advance = [0, -104.275]; }
ECHO: { position = [-4.5743, -50.3947]; size = [9.1456, 101.05]; ascent = 10.0672; descent = -0.1408; offset = [0, 52.1376]; advance = [0, -104.275]; }
WARNING: Don't use valign="baseline" with vertical layouts @9
ECHO: { position = [-4.5743, -102.79]; size = [9.1456, 101.054]; ascent = 10.0672; descent = -0.1408; offset = [0, 0]; advance = [0, -104.275]; }
ECHO: { position = [0.2048, -0.1408]; size = [37.3817, 10.208]; ascent = 10.0672; descent = -0.1408; offset = [0, 0]; advance = [37.8148, 0]; }
ECHO: { position = [0, 0]; size = [0, 0]; ascent = 0; descent = 0; offset = [0, 0]; advance = [11.5764, 0]; }
ECHO: { position = [0, 0]; size = [0, 0]; ascent = 0; descent = 0; offset = [0, 0]; advance = [0, 0]; }
ECHO: { position = [0.13271, 0]; size = [5.73972, 6.34245]; ascent = 6.34245; descent = 0; offset = [0, 0]; advance = [6.00005, 0]; }
WARNING: textmetrics: "size" ignored when "em" is set
ECHO: { position = [0.13271, 0]; size = [5.73972, 6.34245]; ascent = 6.34245; descent = 0; offset = [0, 0]; advance = [6.00005, 0]; }
ECHO: { position = [0.3456, 0]; size = [43.783, 9.5552]; ascent = 9.5552; descent = 0; offset = [0, 0]; advance = [44.4947, 0]; }
ECHO: { position = [0.192, 0]; size = [31.0317, 10.0672]; ascent = 10.0672; descent = 0; offset = [0, 0]; advance = [32.159, 0]; }
ECHO: { position = [0.5888, -0.1408]; size = [48.1119, 10.368]; ascent = 10.2272; descent = -0.1408; offset = [0, 0]; advance = [49.8385, 0]; }
ECHO: { position = [1.3888, 0]; size = [49.302, 9.5552]; ascent = 9.5552; descent = 0; offset = [0, 0]; advance = [52.0835, 0]; }
ECHO: { position = [0.1536, 0]; size = [6.6432, 7.3408]; ascent = 7.3408; descent = 0; offset = [0, 0]; advance = [6.9445, 0]; }
ECHO: { position = [0, 0]; size = [0, 0]; ascent = 0; descent = 0; offset = [0, 0]; advance = [0, 0]; }
ECHO: { position = [0.37376, -1.15456]; size = [2.47552, 4.1344]; ascent = 2.97984; descent = -1.15456; offset = [0, 0]; advance = [3.33388, 0]; }
ECHO: { nominal = { ascent = 12.5733; descent = -2.9433; }; max = { ascent = 13.6109; descent = -4.2114; }; interline = 15.9709; font = { family = "Liberation Sans"; style = "Regular"; }; }
ECHO: { nominal = { ascent = 3.77199; descent = -0.88299; }; max = { ascent = 4.08327; descent = -1.26342; }; interline = 4.79127; font = { family = "Liberation Sans"; style = "Regular"; }; }
ECHO: { nominal = { ascent = 12.3766; descent = -3.0043; }; max = { ascent = 13.6312; descent = -4.2114; }; interline = 15.9709; font = { family = "Liberation Serif"; style = "Regular"; }; }
ECHO: { nominal = { ascent = 2.8907; descent = -1.04268; }; max = { ascent = 3.43153; descent = -1.30717; }; interline = 3.93337; font = { family = "Liberation Mono"; style = "Bold"; }; }
ECHO: { nominal = { ascent = 12.5733; descent = -2.9433; }; max = { ascent = 13.6109; descent = -4.2114; }; interline = 15.9709; font = { family = "Liberation Sans"; style = "Regular"; }; }
ECHO: { nominal = { ascent = 9.05278; descent = -2.11918; }; max = { ascent = 9.79985; descent = -3.03221; }; interline = 11.499; font = { family = "Liberation Sans"; style = "Regular"; }; }
ECHO: { nominal = { ascent = 12.5733; descent = -2.9433; }; max = { ascent = 13.6109; descent = -4.2114; }; interline = 15.9709; font = { family = "Liberation Sans"; style = "Regular"; }; }
ECHO: true, false, false
ECHO: 71.2506, 72.5639, 12.5888"#;
    // The advance adds each glyph's `advance * spacing` in one expression,
    // which OpenSCAD's arm64 build fuses and its x86_64 build does not
    // (`eval::fma`). For "abc" at spacing 1.5 that lands either side of a
    // rounding boundary: the nightly's arm64 slice echoes 37.5062 and its
    // x86_64 slice, run under Rosetta on the same program, 37.5061.
    // neoscad follows the platform, so each architecture gets its own
    // slice's answer; every other line is the same on both.
    let expected = if cfg!(target_arch = "aarch64") {
        expected.to_string()
    } else {
        expected.replace("advance = [37.5062, 0]", "advance = [37.5061, 0]")
    };
    assert_lines(&run(src, &["textmetrics"], &[]), &expected);
}

#[test]
fn json_import_matches_the_nightly() {
    let src = r#"echo(import("d2.json"));
for (f = ["bad1","bad2","bad3","bad4","bad5","bad6","bad7","bad8","bad9","bad10","bad11","bad12","bad13","bad14","bad15","bad16","bad17","bad18","bad19","bad20","bad21","bad22","bad23","ws","empty","nul","comment","badutf","d1"]) echo(import(str(f, ".json")));
echo(import("bom.json"), import("str.json"), import("two.json"), import("up.JSON"));
echo(import("missing.json"));
echo(import("missing.rose"));
echo(import("missing", type="json"));
echo(import());
echo(import(5));
echo(import("d2.json", "yaml"));
echo(import("str.json", foo=1));
echo(import("."));
echo(import(".json"));
d = import("d2.json"); echo(d.a, d[""], d["é"], is_object(d.arr), len(d), [for (k = d) k]);
"#;
    let expected = r#"ECHO: {  = { }; Z = []; a = 7.5; arr = [[[]]]; b = 1; big = -9.22337e+18; e = 1; long = 9.0072e+15; neg = -9.22337e+18; é = 1; }
WARNING: Failed to parse file '/nonexistent/bad1.json': [json.exception.parse_error.101] parse error at line 1, column 10: syntax error while parsing value - invalid literal; last read: '"a": tru}' @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/bad2.json': [json.exception.parse_error.101] parse error at line 1, column 9: syntax error while parsing object key - unexpected '}'; expected string literal @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/bad3.json': [json.exception.parse_error.101] parse error at line 3, column 3: syntax error while parsing value - invalid literal; last read: '2,<U+000A>  @' @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/bad4.json': [json.exception.parse_error.101] parse error at line 1, column 6: syntax error while parsing object separator - unexpected number literal; expected ':' @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/bad5.json': [json.exception.parse_error.101] parse error at line 1, column 4: syntax error while parsing array - unexpected number literal; expected ']' @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/bad6.json': [json.exception.parse_error.101] parse error at line 1, column 3: syntax error while parsing value - invalid number; expected digit after '-'; last read: '-]' @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/bad7.json': [json.exception.parse_error.101] parse error at line 1, column 4: syntax error while parsing value - invalid number; expected digit after '.'; last read: '1.]' @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/bad8.json': [json.exception.parse_error.101] parse error at line 1, column 4: syntax error while parsing value - invalid number; expected '+', '-', or digit after exponent; last read: '1e]' @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/bad9.json': [json.exception.parse_error.101] parse error at line 1, column 5: syntax error while parsing value - invalid number; expected digit after exponent sign; last read: '1e+]' @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/bad10.json': [json.exception.parse_error.101] parse error at line 1, column 4: syntax error while parsing value - invalid string: forbidden character after backslash; last read: '"\x' @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/bad11.json': [json.exception.parse_error.101] parse error at line 1, column 7: syntax error while parsing value - invalid string: '\u' must be followed by 4 hex digits; last read: '"\u12"' @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/bad12.json': [json.exception.parse_error.101] parse error at line 1, column 9: syntax error while parsing value - invalid string: surrogate U+D800..U+DBFF must be followed by U+DC00..U+DFFF; last read: '"\ud800x' @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/bad13.json': [json.exception.parse_error.101] parse error at line 1, column 8: syntax error while parsing value - invalid string: surrogate U+DC00..U+DFFF must follow U+D800..U+DBFF; last read: '"\udc00' @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/bad14.json': [json.exception.parse_error.101] parse error at line 1, column 4: syntax error while parsing value - invalid string: control character U+0009 (HT) must be escaped to \u0009 or \t; last read: '"a<U+0009>' @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/bad15.json': [json.exception.parse_error.101] parse error at line 1, column 6: syntax error while parsing value - invalid string: missing closing quote; last read: '"abc' @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/bad16.json': [json.exception.parse_error.101] parse error at line 1, column 3: syntax error while parsing value - invalid BOM; must be 0xEF 0xBB 0xBF if given; last read: '�[' @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/bad17.json': [json.exception.parse_error.101] parse error at line 1, column 7: syntax error while parsing object - unexpected end of input; expected '}' @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/bad18.json': [json.exception.parse_error.101] parse error at line 1, column 2: syntax error while parsing value - unexpected end of input; expected '[', '{', or a literal @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/bad19.json': [json.exception.parse_error.101] parse error at line 1, column 2: syntax error while parsing object key - unexpected number literal; expected string literal @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/bad20.json': [json.exception.parse_error.101] parse error at line 1, column 3: syntax error while parsing array - unexpected number literal; expected ']' @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/bad21.json': [json.exception.parse_error.101] parse error at line 1, column 4: syntax error while parsing value - invalid string: ill-formed UTF-8 byte; last read: '"�(' @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/bad22.json': [json.exception.parse_error.101] parse error at line 1, column 4: syntax error while parsing value - invalid literal; last read: 'nul' @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/bad23.json': [json.exception.parse_error.101] parse error at line 1, column 10: syntax error while parsing array - unexpected '}'; expected ']' @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/ws.json': [json.exception.parse_error.101] parse error at line 3, column 3: syntax error while parsing value - unexpected end of input; expected '[', '{', or a literal @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/empty.json': [json.exception.parse_error.101] parse error at line 1, column 1: attempting to parse an empty input; check that your input string or stream contains the expected JSON @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/nul.json': [json.exception.parse_error.101] parse error at line 1, column 1: attempting to parse an empty input; check that your input string or stream contains the expected JSON @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/comment.json': [json.exception.parse_error.101] parse error at line 1, column 1: syntax error while parsing value - invalid literal; last read: '/' @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/badutf.json': [json.exception.parse_error.101] parse error at line 1, column 8: syntax error while parsing value - invalid string: ill-formed UTF-8 byte; last read: '"�' @2
ECHO: undef
WARNING: Failed to parse file '/nonexistent/d1.json': [json.exception.out_of_range.406] number overflow parsing '1e400' @2
ECHO: undef
ECHO: [1, 2], "str", [1, 2], { x = 1; }
WARNING: Could not read file '/nonexistent/missing.json' @4
ECHO: undef
WARNING: Unsupported file extension '.rose' while trying to import 'missing.rose' @5
ECHO: undef
WARNING: Could not read file '/nonexistent/missing' @6
ECHO: undef
WARNING: No file extension or type while trying to import '' @7
ECHO: undef
WARNING: No file extension or type while trying to import '' @8
ECHO: undef
WARNING: Unsupported file type 'yaml' while trying to import 'd2.json' @9
ECHO: undef
WARNING: variable "foo" not specified as parameter @10
ECHO: "str"
WARNING: No file extension or type while trying to import '.' @11
ECHO: undef
WARNING: No file extension or type while trying to import '.json' @12
ECHO: undef
ECHO: 7.5, { }, 1, false, 10, ["", "Z", "a", "arr", "b", "big", "e", "long", "neg", "é"]"#;
    assert_lines(
        &run(src, &["import-function", "textmetrics"], JSON_FILES),
        expected,
    );
}

#[test]
fn swizzle_matches_the_nightly() {
    let src = r#"v = [10, 20, 30, 40];
echo(v.x, v.w, v.xy, v.yx, v.xxxx, v.rgba, v.bgr, v.xyzwx, v.xr, v.q, v.xq, v.X, v.RGB);
echo([1].xy, [1, 2].zw, [].x, [].xy, "abc".xy, [0:1:5].begin, [0:1:5].xy, undef.xy);
echo([[1, 2], [3, 4]].yx, [1, 2, 3].zyx.yx);
p = [3, 4, 5]; echo(p.xy * 2, norm(p.xy));
"#;
    let expected = r#"ECHO: 10, 40, [10, 20], [20, 10], [10, 10, 10, 10], [10, 20, 30, 40], [30, 20, 10], undef, undef, undef, undef, undef, undef
ECHO: [1, undef], [undef, undef], undef, [undef, undef], undef, 0, undef, undef
ECHO: [[3, 4], [1, 2]], [2, 3]
ECHO: [6, 8], 5"#;
    assert_lines(&run(src, &["vector-swizzle"], &[]), expected);
}

#[test]
fn features_off_match_the_nightly() {
    let src = r#"v = [10, 20, 30, 40];
echo(v.xy, v.w, v.x);
echo(object(a=1));
echo(has_key(1, "a"));
echo(is_object(1));
echo(textmetrics("a"));
echo(fontmetrics());
echo(import("x.json"));
"#;
    let expected = r#"ECHO: undef, undef, 10
WARNING: Experimental builtin function 'object' is not enabled @3
WARNING: Ignoring unknown function 'object' @3
ECHO: undef
WARNING: Experimental builtin function 'has_key' is not enabled @4
WARNING: Ignoring unknown function 'has_key' @4
ECHO: undef
WARNING: Experimental builtin function 'is_object' is not enabled @5
WARNING: Ignoring unknown function 'is_object' @5
ECHO: undef
WARNING: Experimental builtin function 'textmetrics' is not enabled @6
WARNING: Ignoring unknown function 'textmetrics' @6
ECHO: undef
WARNING: Experimental builtin function 'fontmetrics' is not enabled @7
WARNING: Ignoring unknown function 'fontmetrics' @7
ECHO: undef
WARNING: Experimental builtin function 'import' is not enabled @8
WARNING: Ignoring unknown function 'import' @8
ECHO: undef"#;
    assert_lines(&run(src, &[], &[]), expected);
}
