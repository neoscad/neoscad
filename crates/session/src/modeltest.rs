//! `neoscad test`: model tests.
//!
//! A test file is a `*_test.scad` or `test_*.scad` file; each top-level
//! `module test_*()` in it is one test, run as the file's only top-level
//! instance ([`crate::Run::entry`]): the file's assignments, definitions,
//! includes and `use`s apply, its other top-level instances do not. A test
//! passes when evaluation reports no error (a failed `assert()` is one)
//! and every expectation holds.
//!
//! Expectations are `// @expect` lines in the comment block right above
//! the test module (the grammar is in `docs/model-tests.md`):
//!
//! ```text
//! // @expect volume 1000±1
//! // @expect bbox [10, 10, 10]±0.01
//! // @expect manifold
//! module test_box() cube(10);
//! ```
//!
//! A test with geometric expectations is rendered once, and its numbers
//! come from the same code as `neoscad measure` and `neoscad check`. Tests
//! run on `jobs` threads; results are in file and line order whatever the
//! thread count (everything but `timings_ms` is identical).

use std::path::{Path, PathBuf};

use geom::Geometry;
use lang::diag::Severity;
use lang::loader::FileSystem;
use serde_json::{Value, json};

use crate::check::{CheckSettings, Level, analyze};
use crate::mesh::Mesh;
use crate::{Cancelled, Run, Session, stats};

/// A numeric expectation: `value`, within `tol` (a fraction of `value`
/// when `relative`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Approx {
    pub value: f64,
    pub tol: f64,
    pub relative: bool,
}

impl Approx {
    fn allows(&self, x: f64) -> bool {
        let tol = if self.relative {
            self.tol * self.value.abs()
        } else {
            self.tol
        };
        (x - self.value).abs() <= tol.max(1e-9)
    }

    /// `{"value", "tolerance"}`, the tolerance in the value's units.
    fn json(&self) -> Value {
        let tol = if self.relative {
            self.tol * self.value.abs()
        } else {
            self.tol
        };
        json!({"value": self.value, "tolerance": tol.max(1e-9)})
    }

    fn show(&self) -> String {
        if self.relative && self.tol == DEFAULT_REL {
            return num(self.value);
        }
        if self.relative {
            format!("{}±{}%", num(self.value), num(self.tol * 100.0))
        } else {
            format!("{}±{}", num(self.value), num(self.tol))
        }
    }
}

/// The tolerance of a number written without one: a millionth of it.
const DEFAULT_REL: f64 = 1e-6;
/// The tolerance of bounding box coordinates written without one (mm).
const DEFAULT_BBOX: f64 = 1e-6;

/// One `@expect` line.
#[derive(Debug, Clone, PartialEq)]
pub enum Expect {
    Volume(Approx),
    Area(Approx),
    /// The bounding box's size (2 or 3 numbers), within an absolute
    /// tolerance.
    BboxSize(Vec<f64>, f64),
    /// The bounding box's corners.
    BboxCorners(Vec<f64>, Vec<f64>, f64),
    Manifold,
    Components(usize),
    /// `check clean` (no error or warning finding) or `check no-error`.
    Check {
        warnings_too: bool,
    },
    /// Parts that must exist (turns `part()` on for the test).
    Parts(Vec<String>),
    NoWarnings,
}

impl Expect {
    /// Whether checking it needs the rendered model.
    fn renders(&self) -> bool {
        !matches!(self, Expect::NoWarnings)
    }
}

fn num(x: f64) -> String {
    lang::number::fmt_g(x)
}

/// Parse a number, or `None`.
fn number(s: &str) -> Option<f64> {
    s.trim().parse::<f64>().ok().filter(|x| x.is_finite())
}

/// `±T`, `+-T` or `±P%` after a value; `None` for no tolerance.
fn tolerance(s: &str) -> Result<Option<(f64, bool)>, String> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(None);
    }
    let rest = s
        .strip_prefix('±')
        .or_else(|| s.strip_prefix("+-"))
        .ok_or_else(|| format!("expected '±TOLERANCE' after the value, got '{s}'"))?;
    let (t, rel) = match rest.trim().strip_suffix('%') {
        Some(p) => (p, true),
        None => (rest, false),
    };
    let t = number(t)
        .filter(|t| *t >= 0.0)
        .ok_or_else(|| format!("bad tolerance '{rest}'"))?;
    Ok(Some(if rel { (t / 100.0, true) } else { (t, false) }))
}

fn approx(s: &str) -> Result<Approx, String> {
    let s = s.trim();
    let split = s.find(['±', '+']).filter(|&i| i > 0).unwrap_or(s.len());
    let (v, t) = s.split_at(split);
    let value = number(v).ok_or_else(|| format!("expected a number, got '{}'", v.trim()))?;
    Ok(match tolerance(t)? {
        Some((tol, relative)) => Approx {
            value,
            tol,
            relative,
        },
        None => Approx {
            value,
            tol: DEFAULT_REL,
            relative: true,
        },
    })
}

/// `[a, b, c]` at the start of `s`: the numbers and the rest.
fn vector(s: &str) -> Result<(Vec<f64>, &str), String> {
    let s = s.trim_start();
    let body = s
        .strip_prefix('[')
        .ok_or_else(|| "expected '['".to_string())?;
    let close = body.find(']').ok_or_else(|| "expected ']'".to_string())?;
    let v: Option<Vec<f64>> = body[..close].split(',').map(number).collect();
    let v = v.ok_or_else(|| format!("expected numbers in [{}]", &body[..close]))?;
    Ok((v, &body[close + 1..]))
}

/// Parse the text after `@expect`.
pub fn parse_expect(s: &str) -> Result<Expect, String> {
    let s = s.trim();
    let (word, rest) = s.split_once(char::is_whitespace).unwrap_or((s, ""));
    let rest = rest.trim();
    let none = |e: Expect| {
        if rest.is_empty() {
            Ok(e)
        } else {
            Err(format!("'{word}' takes nothing after it"))
        }
    };
    match word {
        "volume" => approx(rest).map(Expect::Volume),
        "area" => approx(rest).map(Expect::Area),
        "bbox" => {
            let r = rest.trim_start();
            if let Some(inner) = r.strip_prefix('[')
                && inner.trim_start().starts_with('[')
            {
                // [[x0, y0, z0], [x1, y1, z1]]
                let (lo, after) = vector(inner)?;
                let after = after
                    .trim_start()
                    .strip_prefix(',')
                    .ok_or("expected ',' between the corners")?;
                let (hi, after) = vector(after)?;
                let after = after
                    .trim_start()
                    .strip_prefix(']')
                    .ok_or("expected ']' after the corners")?;
                if lo.len() != hi.len() || !(2..=3).contains(&lo.len()) {
                    return Err("corners must both have 2 or 3 numbers".into());
                }
                let tol = tolerance(after)?;
                if tol.is_some_and(|t| t.1) {
                    return Err("a bbox tolerance is in mm, not %".into());
                }
                return Ok(Expect::BboxCorners(
                    lo,
                    hi,
                    tol.map_or(DEFAULT_BBOX, |t| t.0),
                ));
            }
            let (size, after) = vector(r)?;
            if !(2..=3).contains(&size.len()) {
                return Err("a bbox size has 2 or 3 numbers".into());
            }
            let tol = tolerance(after)?;
            if tol.is_some_and(|t| t.1) {
                return Err("a bbox tolerance is in mm, not %".into());
            }
            Ok(Expect::BboxSize(size, tol.map_or(DEFAULT_BBOX, |t| t.0)))
        }
        "manifold" => none(Expect::Manifold),
        "no-warnings" => none(Expect::NoWarnings),
        "components" => rest
            .parse::<usize>()
            .map(Expect::Components)
            .map_err(|_| format!("expected a count, got '{rest}'")),
        "check" => match rest {
            "clean" => Ok(Expect::Check { warnings_too: true }),
            "no-error" | "no-errors" => Ok(Expect::Check {
                warnings_too: false,
            }),
            _ => Err(format!(
                "expected 'check clean' or 'check no-error', got 'check {rest}'"
            )),
        },
        "parts" => {
            let names: Vec<String> = rest
                .split(',')
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .collect();
            if names.is_empty() {
                Err("expected part names: parts lid,base".into())
            } else {
                Ok(Expect::Parts(names))
            }
        }
        "" => Err("empty @expect".into()),
        w => Err(format!(
            "unknown expectation '{w}' (volume, area, bbox, manifold, components, check, parts, no-warnings)"
        )),
    }
}

/// One test: a module of a test file and its expectations.
#[derive(Debug, Clone)]
pub struct TestCase {
    /// The file as named relative to the request's `cwd`.
    pub file: String,
    pub path: PathBuf,
    /// The module's name, or empty for a file that does not parse.
    pub name: String,
    pub line: u32,
    /// Each `@expect` line's text and what it parsed to.
    pub expects: Vec<(String, Result<Expect, String>)>,
    /// Why the file could not be read or parsed.
    pub file_error: Option<String>,
}

impl TestCase {
    pub fn id(&self) -> String {
        if self.name.is_empty() {
            self.file.clone()
        } else {
            format!("{}::{}", self.file, self.name)
        }
    }
}

/// What to run.
#[derive(Debug, Clone, Default)]
pub struct TestRequest {
    /// Test files, or directories searched for them; default `.`.
    pub paths: Vec<String>,
    pub cwd: Option<PathBuf>,
    /// Only tests whose id (`file::name`) contains this.
    pub filter: Option<String>,
    /// `--enable part` for every test (a test with `@expect parts` has it
    /// anyway).
    pub parts: bool,
    /// OpenSCAD's experimental features (`--enable`) for every test.
    pub features: eval::Features,
    /// Threads to run tests on (at least 1; wasm32 runs them in turn).
    pub jobs: usize,
}

/// A whole run.
#[derive(Debug, Clone)]
pub struct TestReport {
    /// 0 when every test passed (and there was one), else 1.
    pub exit_code: u8,
    /// `docs/model-tests.md` describes it.
    pub json: Value,
}

/// A test file by name.
pub fn is_test_file(name: &str) -> bool {
    name.ends_with("_test.scad") || (name.starts_with("test_") && name.ends_with(".scad"))
}

impl Session {
    /// The tests in the files and directories of a request, in file and
    /// line order.
    pub fn discover_tests(&self, req: &TestRequest) -> Result<Vec<TestCase>, String> {
        let cwd = req.cwd.clone().unwrap_or_else(|| self.cfg.work_dir.clone());
        let paths = if req.paths.is_empty() {
            vec![".".to_string()]
        } else {
            req.paths.clone()
        };
        let files = self.find_files(&paths, &cwd, &is_test_file)?;
        let mut out = Vec::new();
        for (file, path) in files {
            let case = |name: String, line: u32, file_error: Option<String>| TestCase {
                file: file.clone(),
                path: path.clone(),
                name,
                line,
                expects: Vec::new(),
                file_error,
            };
            let text = match self.fs.read(&path) {
                Ok(t) => t,
                Err(e) => {
                    out.push(case(String::new(), 0, Some(format!("cannot read: {e}"))));
                    continue;
                }
            };
            let program = lang::parse_file(path.clone(), text);
            if let Some(d) = program.diags.iter().find(|d| d.is_error()) {
                out.push(case(
                    String::new(),
                    d.line,
                    Some(format!("line {}: {}", d.line, d.message)),
                ));
                continue;
            }
            for d in docs::definitions(&program) {
                if d.kind != docs::Kind::Module || !d.name.starts_with("test_") || d.file != path {
                    continue;
                }
                let mut c = case(d.name.clone(), d.line, None);
                for l in &d.comment {
                    if let Some(rest) = l.trim().strip_prefix("@expect") {
                        c.expects
                            .push((rest.trim().to_string(), parse_expect(rest)));
                    }
                }
                if req
                    .filter
                    .as_ref()
                    .is_none_or(|f| c.id().contains(f.as_str()))
                {
                    out.push(c);
                }
            }
        }
        Ok(out)
    }

    /// Run one test.
    pub fn run_test(
        &self,
        case: &TestCase,
        parts: bool,
        features: eval::Features,
        cwd: &Path,
    ) -> Result<Value, Cancelled> {
        let started = self.now();
        let mut out = json!({
            "id": case.id(),
            "file": case.file,
            "name": case.name,
            "line": case.line,
        });
        if let Some(e) = &case.file_error {
            out["ok"] = json!(false);
            out["failures"] = json!([{"kind": "file", "message": e}]);
            out["expectations"] = json!([]);
            out["diagnostics"] = json!([]);
            out["echo"] = json!([]);
            out["timings_ms"] = json!(0.0);
            return Ok(out);
        }
        let expects: Vec<(&String, &Expect)> = case
            .expects
            .iter()
            .filter_map(|(t, e)| e.as_ref().ok().map(|e| (t, e)))
            .collect();
        let mut failures: Vec<Value> = case
            .expects
            .iter()
            .filter_map(|(t, e)| e.as_ref().err().map(|m| (t, m)))
            .map(|(t, m)| {
                json!({"kind": "expectation-syntax", "expect": t,
                       "message": format!("bad @expect: {m}")})
            })
            .collect();
        let wants_parts = expects.iter().any(|(_, e)| matches!(e, Expect::Parts(_)));
        let mut run = Run::new(case.file.clone());
        run.cwd = Some(cwd.to_path_buf());
        run.entry = Some(case.name.clone());
        run.supersede = false;
        run.parts = parts || wants_parts;
        run.features = features;
        let render = expects.iter().any(|(_, e)| e.renders());
        let (exit_code, log, model) = if render {
            let scheme = render::ColorScheme::cornfield();
            let (r, parts) = self.render_parts(&run, &scheme)?;
            (r.exit_code, r.log, Some((r.geometry, parts)))
        } else {
            let r = self.evaluate(&run, false)?;
            (r.exit_code, r.log, None)
        };
        let errors = log.count(Severity::Error);
        if exit_code != 0 || errors > 0 {
            // The line as OpenSCAD prints it, with its file and line.
            let first = log
                .diagnostics_json()
                .into_iter()
                .find(|d| d["severity"] == "error")
                .and_then(|d| d["text"].as_str().map(str::to_string))
                .unwrap_or_else(|| format!("the model failed (exit code {exit_code})"));
            failures.push(json!({"kind": "error", "message": first}));
        }
        let mut results = Vec::new();
        if exit_code == 0 {
            let clock = || self.now();
            let facts = Facts::new(model.as_ref(), &clock);
            for (text, e) in &expects {
                let j = facts.judge(e, &log);
                let r = json!({"expect": text, "ok": j.ok, "expected": j.expected,
                               "actual": j.actual});
                if !j.ok {
                    let mut f = r.clone();
                    f["kind"] = json!("expect");
                    f["message"] = json!(format!(
                        "@expect {text}: expected {}, got {}",
                        j.shown.0, j.shown.1
                    ));
                    failures.push(f);
                }
                results.push(r);
            }
        }
        out["ok"] = json!(failures.is_empty());
        out["failures"] = json!(failures);
        out["expectations"] = json!(results);
        out["diagnostics"] = json!(log.diagnostics_json());
        out["echo"] = json!(log.echo());
        out["timings_ms"] = json!(((self.now() - started) * 10.0).round() / 10.0);
        Ok(out)
    }

    /// Discover and run tests.
    pub fn test(&self, req: &TestRequest) -> Result<TestReport, Cancelled> {
        let started = self.now();
        let cwd = req.cwd.clone().unwrap_or_else(|| self.cfg.work_dir.clone());
        let cases = match self.discover_tests(req) {
            Ok(c) => c,
            Err(e) => {
                return Ok(TestReport {
                    exit_code: 1,
                    json: json!({"schema": 1, "exit_code": 1, "error": e,
                                 "counts": {"tests": 0, "passed": 0, "failed": 0, "files": 0},
                                 "tests": []}),
                });
            }
        };
        let results = run_all(&cases, req.jobs.max(1), |c| {
            self.run_test(c, req.parts, req.features, &cwd)
        });
        let results: Vec<Value> = results.into_iter().collect::<Result<_, _>>()?;
        let passed = results.iter().filter(|r| r["ok"] == json!(true)).count();
        let failed = results.len() - passed;
        let mut files: Vec<&str> = cases.iter().map(|c| c.file.as_str()).collect();
        files.dedup();
        let exit_code = if failed > 0 || results.is_empty() {
            1
        } else {
            0
        };
        Ok(TestReport {
            exit_code,
            json: json!({
                "schema": 1,
                "exit_code": exit_code,
                "counts": {"tests": results.len(), "passed": passed, "failed": failed,
                           "files": files.len()},
                "tests": results,
                "timings_ms": ((self.now() - started) * 10.0).round() / 10.0,
            }),
        })
    }
}

/// An expectation's outcome.
struct Judged {
    ok: bool,
    expected: Value,
    actual: Value,
    /// Expected and actual as the failure message shows them.
    shown: (String, String),
}

/// Rounded to 1e-6, as `neoscad measure` reports.
fn r6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

fn r6v(v: &[f64]) -> Vec<f64> {
    v.iter().map(|&x| r6(x)).collect()
}

fn nums(v: &[f64]) -> String {
    format!(
        "[{}]",
        v.iter().map(|&x| num(x)).collect::<Vec<_>>().join(", ")
    )
}

/// A 3D model's numbers.
#[derive(Debug, Clone, Copy)]
struct Solid {
    volume: f64,
    area: f64,
    lo: [f64; 3],
    hi: [f64; 3],
    /// Manifold accepts it as a solid.
    valid: bool,
    components: usize,
}

/// What the rendered model measures, computed on first use.
struct Facts<'a> {
    model: Option<&'a (Option<Geometry>, Vec<crate::parts::Part>)>,
    now: &'a dyn Fn() -> f64,
    solid: std::cell::OnceCell<Option<Solid>>,
    analysis: std::cell::OnceCell<crate::check::Analysis>,
}

impl<'a> Facts<'a> {
    fn new(
        model: Option<&'a (Option<Geometry>, Vec<crate::parts::Part>)>,
        now: &'a dyn Fn() -> f64,
    ) -> Self {
        Facts {
            model,
            now,
            solid: std::cell::OnceCell::new(),
            analysis: std::cell::OnceCell::new(),
        }
    }

    fn geometry(&self) -> Option<&Geometry> {
        self.model.and_then(|(g, _)| g.as_ref())
    }

    /// The model rendered to nothing. It measures volume 0, area 0 and 0
    /// components: `@expect volume 0` on an `intersection()` whose parts do
    /// not meet is how an agent asks "no interference", and failing it with
    /// "got an empty model" (the T2 transcript audit) sent it hunting. A
    /// box or `manifold` has no answer here and still fails.
    fn empty(&self) -> bool {
        self.model.is_some() && self.geometry().is_none_or(Geometry::is_empty)
    }

    /// Volume, area, bbox and validity of a 3D model.
    fn solid(&self) -> Option<Solid> {
        *self.solid.get_or_init(|| {
            let g = self.geometry()?;
            if matches!(g, Geometry::Polygon2d(_)) {
                return None;
            }
            let s = stats::solid(g);
            let mesh = Mesh::of_solid(&s);
            let (vol, area, _) = mesh.mass();
            let b = mesh.bbox();
            Some(Solid {
                volume: vol,
                area,
                lo: b.lo,
                hi: b.hi,
                valid: s.is_valid(),
                components: mesh.components().1,
            })
        })
    }

    fn analysis(&self) -> &crate::check::Analysis {
        self.analysis.get_or_init(|| {
            let parts = self.model.map_or(&[][..], |(_, p)| &p[..]);
            analyze(self.geometry(), parts, &CheckSettings::default(), self.now)
        })
    }

    /// The 2D model's bounds and area.
    fn flat(&self) -> Option<([f64; 2], [f64; 2], f64)> {
        let g = self.geometry()?;
        let Geometry::Polygon2d(p) = g else {
            return None;
        };
        let (lo, hi) = p.bounds()?;
        let v = stats::geometry(g, &geom::color::CORNFIELD);
        Some((lo, hi, v["area"].as_f64().unwrap_or(0.0)))
    }

    /// Whether `e` holds, and what was expected and found (as JSON and as
    /// text for the failure message).
    fn judge(&self, e: &Expect, log: &crate::Log) -> Judged {
        let what = || -> String {
            match self.geometry() {
                _ if self.empty() => "an empty model".into(),
                None => "an empty model".into(),
                Some(Geometry::Polygon2d(_)) => "a 2D model".into(),
                Some(_) => "a 3D model".into(),
            }
        };
        let lacking = |expected: Value, shown: String| Judged {
            ok: false,
            expected,
            actual: json!(what()),
            shown: (shown, what()),
        };
        // What an amount reads as: an empty model's is 0, said so, and a
        // solid with no volume is faces pressed together, which is what
        // parts that only touch intersect to.
        let amount = |x: f64| -> String {
            if self.empty() {
                format!("{} ({})", num(x), what())
            } else if self
                .solid()
                .is_some_and(|s| crate::stats::touch_only(s.volume, s.area))
            {
                format!(
                    "{} (a zero-volume result: faces where parts only touch)",
                    num(x)
                )
            } else {
                num(x)
            }
        };
        match e {
            Expect::Volume(a) | Expect::Area(a) => {
                let x = match e {
                    Expect::Volume(_) => self.solid().map(|s| s.volume),
                    _ => self
                        .solid()
                        .map(|s| s.area)
                        .or_else(|| self.flat().map(|f| f.2)),
                };
                match x.or_else(|| self.empty().then_some(0.0)) {
                    Some(x) => Judged {
                        ok: a.allows(x),
                        expected: a.json(),
                        actual: json!(r6(x)),
                        shown: (a.show(), amount(r6(x))),
                    },
                    None => lacking(a.json(), a.show()),
                }
            }
            Expect::BboxSize(size, tol) => {
                let expected = json!({"size": size, "tolerance": tol});
                let shown = format!("{}±{}", nums(size), num(*tol));
                match self.corners(size.len()) {
                    Some((lo, hi)) => {
                        let got: Vec<f64> = lo.iter().zip(&hi).map(|(l, h)| r6(h - l)).collect();
                        Judged {
                            ok: got.iter().zip(size).all(|(g, s)| (g - s).abs() <= *tol),
                            expected,
                            actual: json!({"size": got, "min": r6v(&lo), "max": r6v(&hi)}),
                            shown: (shown, nums(&got)),
                        }
                    }
                    None => lacking(expected, shown),
                }
            }
            Expect::BboxCorners(lo, hi, tol) => {
                let expected = json!({"min": lo, "max": hi, "tolerance": tol});
                let shown = format!("[{}, {}]±{}", nums(lo), nums(hi), num(*tol));
                match self.corners(lo.len()) {
                    Some((glo, ghi)) => Judged {
                        ok: glo.iter().zip(lo).all(|(g, e)| (g - e).abs() <= *tol)
                            && ghi.iter().zip(hi).all(|(g, e)| (g - e).abs() <= *tol),
                        expected,
                        actual: json!({"min": r6v(&glo), "max": r6v(&ghi)}),
                        shown: (
                            shown,
                            format!("[{}, {}]", nums(&r6v(&glo)), nums(&r6v(&ghi))),
                        ),
                    },
                    None => lacking(expected, shown),
                }
            }
            Expect::Manifold => match self.solid() {
                Some(s) => {
                    let bad = self
                        .analysis()
                        .findings
                        .iter()
                        .any(|f| matches!(f.code, "not-manifold" | "not-closed"));
                    let ok = s.valid && !bad;
                    let got = if ok { "manifold" } else { "not manifold" };
                    Judged {
                        ok,
                        expected: json!(true),
                        actual: json!(ok),
                        shown: ("manifold".into(), got.into()),
                    }
                }
                None => lacking(json!(true), "manifold".into()),
            },
            Expect::Components(n) => {
                let got = match self.solid() {
                    Some(s) => Some(s.components),
                    None => self.empty().then_some(0),
                };
                match got {
                    Some(c) => Judged {
                        ok: c == *n,
                        expected: json!(n),
                        actual: json!(c),
                        shown: (n.to_string(), amount(c as f64)),
                    },
                    None => lacking(json!(n), n.to_string()),
                }
            }
            Expect::Check { warnings_too } => {
                let a = self.analysis();
                let bad: Vec<&crate::check::Finding> = a
                    .findings
                    .iter()
                    .filter(|f| {
                        f.level == Level::Error || (*warnings_too && f.level == Level::Warning)
                    })
                    .collect();
                let exp = if *warnings_too {
                    "no errors or warnings"
                } else {
                    "no errors"
                };
                let listed: Vec<String> = bad
                    .iter()
                    .map(|f| format!("{} {}: {}", f.level.name(), f.code, f.message))
                    .collect();
                Judged {
                    ok: bad.is_empty(),
                    expected: json!([]),
                    actual: json!(
                        bad.iter()
                            .map(|f| json!({"severity": f.level.name(), "code": f.code,
                                            "message": f.message, "fix": f.fix}))
                            .collect::<Vec<_>>()
                    ),
                    shown: (exp.into(), listed.join("; ")),
                }
            }
            Expect::Parts(names) => {
                let have: Vec<&str> = self
                    .model
                    .map(|(_, p)| p.iter().map(|p| p.name.as_str()).collect())
                    .unwrap_or_default();
                Judged {
                    ok: names.iter().all(|n| have.contains(&n.as_str())),
                    expected: json!(names),
                    actual: json!(have),
                    shown: (
                        names.join(","),
                        if have.is_empty() {
                            "no parts".into()
                        } else {
                            have.join(",")
                        },
                    ),
                }
            }
            Expect::NoWarnings => {
                let w: Vec<String> = log
                    .lines
                    .iter()
                    // The `use`d-file hint describes what OpenSCAD does,
                    // which the model may rely on; it is not a warning
                    // OpenSCAD gives.
                    .filter(|l| {
                        matches!(l.severity, Some(Severity::Warning | Severity::Deprecated))
                            && l.code != Some(lang::diag::DiagCode::UseSpecialVariables)
                    })
                    .map(|l| l.message.clone())
                    .collect();
                Judged {
                    ok: w.is_empty(),
                    expected: json!([]),
                    actual: json!(w),
                    shown: ("no warnings".into(), w.join("; ")),
                }
            }
        }
    }

    /// The bounding box, as 2 or 3 coordinates per corner.
    fn corners(&self, dims: usize) -> Option<(Vec<f64>, Vec<f64>)> {
        if let Some(s) = self.solid() {
            return (dims == 3).then(|| (s.lo.to_vec(), s.hi.to_vec()));
        }
        let (lo, hi, _) = self.flat()?;
        (dims == 2).then(|| (lo.to_vec(), hi.to_vec()))
    }
}

/// `f` over `items` on up to `jobs` threads, results in order.
#[cfg(not(target_arch = "wasm32"))]
fn run_all<T: Sync, R: Send>(items: &[T], jobs: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    if jobs <= 1 || items.len() <= 1 {
        return items.iter().map(f).collect();
    }
    let next = AtomicUsize::new(0);
    let slots: Vec<Mutex<Option<R>>> = items.iter().map(|_| Mutex::new(None)).collect();
    std::thread::scope(|s| {
        for _ in 0..jobs.min(items.len()) {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(i) else {
                        break;
                    };
                    let r = f(item);
                    *slots[i]
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(r);
                }
            });
        }
    });
    slots
        .into_iter()
        .map(|m| {
            m.into_inner()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .expect("every test ran")
        })
        .collect()
}

/// wasm32 has no threads: in turn.
#[cfg(target_arch = "wasm32")]
fn run_all<T: Sync, R: Send>(items: &[T], _jobs: usize, f: impl Fn(&T) -> R + Sync) -> Vec<R> {
    items.iter().map(f).collect()
}

/// The report of a run as text: a line per test, the failures under it,
/// and a summary.
pub fn text(report: &Value) -> String {
    let mut out = String::new();
    if let Some(e) = report["error"].as_str() {
        out.push_str(&format!("neoscad test: {e}\n"));
        return out;
    }
    let tests = report["tests"].as_array().cloned().unwrap_or_default();
    if tests.is_empty() {
        out.push_str(
            "no tests found: test files are *_test.scad or test_*.scad, tests are `module test_*()`\n",
        );
        return out;
    }
    for t in &tests {
        let ok = t["ok"] == json!(true);
        out.push_str(&format!(
            "test {} ... {}\n",
            t["id"].as_str().unwrap_or(""),
            if ok { "ok" } else { "FAILED" }
        ));
        if !ok {
            for f in t["failures"].as_array().into_iter().flatten() {
                out.push_str(&format!("    {}\n", f["message"].as_str().unwrap_or("")));
            }
        }
    }
    let c = &report["counts"];
    out.push_str(&format!(
        "\ntest result: {}. {} passed; {} failed; {} files\n",
        if report["exit_code"] == json!(0) {
            "ok"
        } else {
            "FAILED"
        },
        c["passed"],
        c["failed"],
        c["files"]
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grammar() {
        assert_eq!(
            parse_expect("volume 1000±1"),
            Ok(Expect::Volume(Approx {
                value: 1000.0,
                tol: 1.0,
                relative: false
            }))
        );
        assert_eq!(
            parse_expect("volume 1000 +- 5%"),
            Ok(Expect::Volume(Approx {
                value: 1000.0,
                tol: 0.05,
                relative: true
            }))
        );
        assert_eq!(
            parse_expect("bbox [10, 10, 10]±0.01"),
            Ok(Expect::BboxSize(vec![10.0; 3], 0.01))
        );
        assert_eq!(
            parse_expect("bbox [[0,0,0],[1,2,3]]"),
            Ok(Expect::BboxCorners(
                vec![0.0; 3],
                vec![1.0, 2.0, 3.0],
                DEFAULT_BBOX
            ))
        );
        assert_eq!(parse_expect("manifold"), Ok(Expect::Manifold));
        assert_eq!(
            parse_expect("check clean"),
            Ok(Expect::Check { warnings_too: true })
        );
        assert_eq!(
            parse_expect("parts lid, base"),
            Ok(Expect::Parts(vec!["lid".into(), "base".into()]))
        );
        assert_eq!(parse_expect("components 2"), Ok(Expect::Components(2)));
        assert!(parse_expect("volum 3").is_err());
        assert!(parse_expect("volume ten").is_err());
        assert!(parse_expect("manifold yes").is_err());
        assert!(parse_expect("bbox [1, 2, 3]±5%").is_err());
    }

    #[test]
    fn test_files() {
        assert!(is_test_file("box_test.scad"));
        assert!(is_test_file("test_box.scad"));
        assert!(!is_test_file("box.scad"));
        assert!(!is_test_file("test_box.stl"));
    }
}
