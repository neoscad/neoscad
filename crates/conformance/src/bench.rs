//! `conformance bench`: the benchmark series of the engine milestone audit
//! (docs/audits/engine-milestone.md, "Benchmark baseline"), made
//! permanent.
//!
//! Every model in `conformance/bench.json` is exported to ASCII STL by
//! each reference binary (neoscad, the nightly with `--backend=manifold`
//! and `--backend=cgal`, OpenSCAD 2021.01), one run after another, and the
//! best wall time of N runs is kept (a single run once one takes longer
//! than `single_run_over_s`). Each run also records its CPU time (user +
//! system of the child, from `getrusage`), and the last run's STL is
//! measured (vertices, triangles, volume, area, bounding box) so that a
//! faster but wrong result shows: every reference's mesh is compared with
//! neoscad's, and a difference beyond rounding is flagged.
//!
//! Two extra metrics: cold start (a trivial model, many runs) and
//! evaluation only (BOSL2's test suite exported to `.echo`, summed).
//!
//! Results go to `progress/bench/<UTC>-<sha>[-dirty].json` in the audit's
//! schema (`docs/audits/engine-milestone-bench.json`) plus the commit,
//! subject and dirty flag, a line is appended to `progress/bench/index.jsonl`,
//! and a table is printed. `conformance bench-chart` draws a result file.

use std::collections::{BTreeMap, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::ctx::{Ctx, git};

/// `conformance/bench.json`.
#[derive(Debug, Deserialize)]
struct Config {
    runs: u32,
    single_run_over_s: f64,
    timeout_s: f64,
    references: Vec<RefConfig>,
    libraries: BTreeMap<String, LibConfig>,
    cold_start: ColdStart,
    eval_only: EvalOnly,
    models: BTreeMap<String, ModelConfig>,
}

#[derive(Debug, Clone, Deserialize)]
struct RefConfig {
    id: String,
    binary: String,
    args: Vec<String>,
    quick: bool,
    eval: bool,
}

#[derive(Debug, Deserialize)]
struct LibConfig {
    path: String,
    clone: String,
}

#[derive(Debug, Deserialize)]
struct ColdStart {
    description: String,
    source: String,
    runs: u32,
}

#[derive(Debug, Deserialize)]
struct EvalOnly {
    description: String,
    #[serde(default)]
    requires: Vec<String>,
    tests: String,
}

#[derive(Debug, Deserialize)]
struct ModelConfig {
    description: String,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    requires: Vec<String>,
    /// Files the model imports: name to the OpenSCAD source neoscad
    /// exports it from.
    #[serde(default)]
    inputs: BTreeMap<String, String>,
}

#[derive(Debug)]
pub struct BenchOptions {
    /// Model ids (and `cold_start`, `eval_only`) to run; all if empty.
    pub only: Vec<String>,
    /// Reference ids to run; all (or the quick ones) if empty.
    pub refs: Vec<String>,
    /// neoscad and the nightly's Manifold backend only.
    pub quick: bool,
    pub timeout: Option<f64>,
    pub runs: Option<u32>,
    /// neoscad binary (default target/release/neoscad).
    pub binary: Option<PathBuf>,
}

/// One process run.
#[derive(Debug, Clone, Copy)]
struct Run {
    /// Exit code; `None` for a timeout (or death by signal).
    code: Option<i32>,
    timed_out: bool,
    wall_s: f64,
    cpu_s: f64,
}

/// User plus system time of every waited-for child so far.
fn children_cpu_s() -> f64 {
    #[cfg(unix)]
    {
        use nix::sys::resource::{UsageWho, getrusage};
        if let Ok(u) = getrusage(UsageWho::RUSAGE_CHILDREN) {
            let t = |tv: nix::sys::time::TimeVal| tv.tv_sec() as f64 + tv.tv_usec() as f64 / 1e6;
            return t(u.user_time()) + t(u.system_time());
        }
    }
    0.0
}

/// Run `cmd` in `cwd`, timing it. The child is polled with short sleeps
/// (at most 1 ms, far less for short runs) so the measured wall time is
/// within a small fraction of the process's; its stderr goes to
/// `stderr_to` for diagnosis.
fn time_run(
    cmd: &[String],
    cwd: &Path,
    env: &[(&str, &Path)],
    timeout: Duration,
    stderr_to: &Path,
) -> Result<Run, String> {
    let err = File::create(stderr_to).map_err(|e| format!("{}: {e}", stderr_to.display()))?;
    let mut c = Command::new(&cmd[0]);
    c.args(&cmd[1..])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(err)
        .env_remove("NEOSCAD_FONT_DIR")
        .env_remove("OPENSCAD_FONT_PATH");
    for (k, v) in env {
        c.env(k, v);
    }
    let cpu0 = children_cpu_s();
    let start = Instant::now();
    let mut child = c.spawn().map_err(|e| format!("{}: {e}", cmd[0]))?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let wall_s = start.elapsed().as_secs_f64();
                return Ok(Run {
                    code: status.code(),
                    timed_out: false,
                    wall_s,
                    cpu_s: children_cpu_s() - cpu0,
                });
            }
            Ok(None) => {
                let e = start.elapsed();
                if e > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(Run {
                        code: None,
                        timed_out: true,
                        wall_s: e.as_secs_f64(),
                        cpu_s: children_cpu_s() - cpu0,
                    });
                }
                // Poll at about 1% of the elapsed time, 50 us to 1 ms.
                let nap = (e / 100).clamp(Duration::from_micros(50), Duration::from_millis(1));
                std::thread::sleep(nap);
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// Best-of-N timing of one command: the audit's method.
fn measure(
    cmd: &[String],
    cwd: &Path,
    env: &[(&str, &Path)],
    runs: u32,
    single_over: f64,
    timeout: Duration,
    stderr_to: &Path,
) -> Result<Value, String> {
    let mut walls: Vec<Value> = Vec::new();
    let mut cpus: Vec<Value> = Vec::new();
    let mut rc = json!(null);
    let mut best: Option<f64> = None;
    for _ in 0..runs.max(1) {
        let r = time_run(cmd, cwd, env, timeout, stderr_to)?;
        if r.timed_out {
            rc = json!("timeout");
            walls.push(json!(null));
            break;
        }
        walls.push(json!(round(r.wall_s, 4)));
        cpus.push(json!(round(r.cpu_s, 3)));
        match r.code {
            Some(0) => {
                rc = json!(0);
                best = Some(best.map_or(r.wall_s, |b: f64| b.min(r.wall_s)));
            }
            Some(c) => {
                rc = json!(c);
                best = None;
                break;
            }
            None => {
                rc = json!("signal");
                best = None;
                break;
            }
        }
        if r.wall_s > single_over {
            break;
        }
    }
    Ok(json!({
        "rc": rc,
        "runs_s": walls,
        "best_s": best.map(|b| round(b, 4)),
        "cpu_s_of_runs": cpus,
    }))
}

fn round(x: f64, places: i32) -> f64 {
    let f = 10f64.powi(places);
    (x * f).round() / f
}

/// What the audit's `meshstat.py` measures in an STL file.
#[derive(Debug, Clone, PartialEq)]
pub struct MeshStats {
    pub nv: usize,
    pub nf: usize,
    pub vol: f64,
    pub area: f64,
    /// min x, y, z, max x, y, z.
    pub bbox: [f64; 6],
}

/// Read an ASCII or binary STL and measure it.
pub fn mesh_stats(data: &[u8]) -> Option<MeshStats> {
    let mut tris: Vec<[[f64; 3]; 3]> = Vec::new();
    let ascii = data.starts_with(b"solid") && {
        let head = &data[..data.len().min(512)];
        head.windows(5).any(|w| w == b"facet") || data.len() < 84
    };
    if ascii {
        let text = std::str::from_utf8(data).ok()?;
        let mut cur: Vec<[f64; 3]> = Vec::with_capacity(3);
        for l in text.lines() {
            let l = l.trim_start();
            if let Some(rest) = l.strip_prefix("vertex") {
                let v: Vec<f64> = rest
                    .split_whitespace()
                    .filter_map(|t| t.parse().ok())
                    .collect();
                if v.len() != 3 {
                    return None;
                }
                cur.push([v[0], v[1], v[2]]);
                if cur.len() == 3 {
                    tris.push([cur[0], cur[1], cur[2]]);
                    cur.clear();
                }
            }
        }
    } else {
        if data.len() < 84 {
            return None;
        }
        let n = u32::from_le_bytes(data[80..84].try_into().ok()?) as usize;
        for k in 0..n {
            let off = 84 + 50 * k;
            let rec = data.get(off..off + 50)?;
            let f = |i: usize| {
                f64::from(f32::from_le_bytes(
                    rec[i * 4..i * 4 + 4].try_into().expect("4 bytes"),
                ))
            };
            let v = |j: usize| [f(3 + 3 * j), f(4 + 3 * j), f(5 + 3 * j)];
            tris.push([v(0), v(1), v(2)]);
        }
    }
    let mut seen: HashMap<[u64; 3], ()> = HashMap::new();
    let mut vol = 0.0;
    let mut area = 0.0;
    let mut bbox = [
        f64::INFINITY,
        f64::INFINITY,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
        f64::NEG_INFINITY,
    ];
    for [a, b, c] in &tris {
        for p in [a, b, c] {
            seen.insert(p.map(f64::to_bits), ());
            for k in 0..3 {
                bbox[k] = bbox[k].min(p[k]);
                bbox[k + 3] = bbox[k + 3].max(p[k]);
            }
        }
        vol += (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
            + a[2] * (b[0] * c[1] - b[1] * c[0]))
            / 6.0;
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let w = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let cr = [
            u[1] * w[2] - u[2] * w[1],
            u[2] * w[0] - u[0] * w[2],
            u[0] * w[1] - u[1] * w[0],
        ];
        area += 0.5 * (cr[0] * cr[0] + cr[1] * cr[1] + cr[2] * cr[2]).sqrt();
    }
    if tris.is_empty() {
        bbox = [0.0; 6];
    }
    Some(MeshStats {
        nv: seen.len(),
        nf: tris.len(),
        vol,
        area,
        bbox,
    })
}

fn stats_json(s: &MeshStats) -> Value {
    json!({
        "nv": s.nv,
        "nf": s.nf,
        "tris": s.nf,
        "vol": round(s.vol, 4),
        "area": round(s.area, 4),
        "bbox": s.bbox.iter().map(|x| round(*x, 4)).collect::<Vec<_>>(),
    })
}

/// Whether `other` describes the same solid as `neo`, up to what
/// different triangulations and float output can change: volume and area
/// within 0.1%, bounding box within 0.1% of its largest side. `None` when
/// they agree, else what differs. Vertex and triangle counts are not
/// compared: kernels legitimately mesh the same solid differently.
pub fn mesh_check(neo: &MeshStats, other: &MeshStats) -> Option<String> {
    let rel = |a: f64, b: f64| (a - b).abs() / a.abs().max(b.abs()).max(1e-12);
    let mut why = Vec::new();
    if rel(neo.vol, other.vol) > 1e-3 {
        why.push(format!("volume {:.4} vs {:.4}", other.vol, neo.vol));
    }
    if rel(neo.area, other.area) > 1e-3 {
        why.push(format!("area {:.4} vs {:.4}", other.area, neo.area));
    }
    let size = (0..3)
        .map(|k| neo.bbox[k + 3] - neo.bbox[k])
        .fold(0.0f64, f64::max)
        .max(1e-9);
    if (0..6).any(|k| (neo.bbox[k] - other.bbox[k]).abs() > 1e-3 * size) {
        why.push("bounding box".to_string());
    }
    (!why.is_empty()).then(|| why.join(", "))
}

/// Split BOSL2's `.scadtest` files into one `.scad` per test, with the
/// flags that decide its outcome. The format is a TOML subset: `[[test]]`
/// tables of `name = "..."`, `script = '''...'''` and boolean flags.
fn split_scadtests(dir: &Path, out: &Path) -> Result<Vec<(PathBuf, TestFlags)>, String> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "scadtest"))
        .collect();
    files.sort();
    fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let mut tests = Vec::new();
    for f in files {
        let stem = f
            .file_stem()
            .map(|s| s.to_string_lossy().trim_start_matches("test_").to_string())
            .unwrap_or_default();
        let text = fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
        for t in parse_scadtest(&text) {
            // The scripts include `<../std.scad>` relative to tests/; through
            // the library path they find the same files as `<BOSL2/...>`.
            let script = t.script.replace("include <../", "include <BOSL2/");
            let script = script.replace("use <../", "use <BOSL2/");
            let path = out.join(format!("{stem}__{}.scad", t.name));
            fs::write(&path, script).map_err(|e| e.to_string())?;
            tests.push((path, t.flags));
        }
    }
    Ok(tests)
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct TestFlags {
    expect_success: bool,
    assert_no_echoes: bool,
    assert_no_warnings: bool,
}

#[derive(Debug)]
struct ScadTest {
    name: String,
    script: String,
    flags: TestFlags,
}

fn parse_scadtest(text: &str) -> Vec<ScadTest> {
    let mut out: Vec<ScadTest> = Vec::new();
    let mut lines = text.lines();
    while let Some(l) = lines.next() {
        let l = l.trim_end();
        if l == "[[test]]" {
            out.push(ScadTest {
                name: String::new(),
                script: String::new(),
                flags: TestFlags {
                    expect_success: true,
                    assert_no_echoes: true,
                    assert_no_warnings: true,
                },
            });
            continue;
        }
        let Some(t) = out.last_mut() else { continue };
        let Some((key, value)) = l.split_once(" = ") else {
            continue;
        };
        match (key, value) {
            ("name", v) => t.name = v.trim_matches('"').to_string(),
            ("script", "'''") => {
                let mut body = String::new();
                for s in lines.by_ref() {
                    if s.trim_end() == "'''" {
                        break;
                    }
                    body.push_str(s);
                    body.push('\n');
                }
                t.script = body;
            }
            ("expect_success", v) => t.flags.expect_success = v == "true",
            ("assert_no_echoes", v) => t.flags.assert_no_echoes = v == "true",
            ("assert_no_warnings", v) => t.flags.assert_no_warnings = v == "true",
            _ => {}
        }
    }
    out
}

/// BOSL2's pass rule (the audit's `bosl_tests.py`, after BOSL2's runner):
/// a test expected to succeed must exit 0 with no `ERROR:`/`TRACE:` lines
/// and, unless allowed, no echoes or warnings; one expected to fail must
/// not succeed.
fn test_passed(flags: TestFlags, run: &Run, echo: &str) -> bool {
    let has = |p: &[&str]| echo.lines().any(|l| p.iter().any(|x| l.starts_with(x)));
    let ok_exit = run.code == Some(0) && !has(&["ERROR:", "TRACE:"]);
    if flags.expect_success {
        ok_exit
            && (!flags.assert_no_echoes || !has(&["ECHO"]))
            && (!flags.assert_no_warnings || !has(&["WARNING"]))
    } else {
        !ok_exit
    }
}

/// `sysctl -n KEY`.
fn sysctl(key: &str) -> Option<String> {
    let o = Command::new("sysctl").args(["-n", key]).output().ok()?;
    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
    (o.status.success() && !s.is_empty()).then_some(s)
}

fn command_out(cmd: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(cmd).args(args).output().ok()?;
    let s = String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr);
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// The machine, as far as macOS reports it.
fn machine() -> Value {
    let cores = match (
        sysctl("hw.ncpu"),
        sysctl("hw.perflevel0.physicalcpu"),
        sysctl("hw.perflevel1.physicalcpu"),
    ) {
        (Some(n), Some(p), Some(e)) => format!("{n} ({p} performance + {e} efficiency)"),
        (Some(n), _, _) => n,
        _ => std::thread::available_parallelism()
            .map_or(1, |n| n.get())
            .to_string(),
    };
    let memory_gb = sysctl("hw.memsize")
        .and_then(|m| m.parse::<f64>().ok())
        .map(|b| (b / f64::from(1u32 << 30)).round());
    let os = match (
        command_out("sw_vers", &["-productVersion"]),
        command_out("sw_vers", &["-buildVersion"]),
    ) {
        (Some(v), Some(b)) => format!("macOS {v} ({b})"),
        _ => std::env::consts::OS.to_string(),
    };
    let power = command_out("pmset", &["-g", "batt"]).map(|s| {
        if s.contains("'AC Power'") {
            "AC".to_string()
        } else if s.contains("'Battery Power'") {
            "battery".to_string()
        } else {
            "unknown".to_string()
        }
    });
    json!({
        "model": match (sysctl("hw.model"), sysctl("machdep.cpu.brand_string")) {
            (Some(m), Some(c)) => format!("{m} ({c})"),
            (Some(m), None) => m,
            _ => std::env::consts::ARCH.to_string(),
        },
        "cores": cores,
        "memory_gb": memory_gb,
        "os": os,
        "power": power,
    })
}

/// A reference's binary with `{REPO}` expanded.
fn binary_path(ctx: &Ctx, r: &RefConfig, override_neo: Option<&Path>) -> PathBuf {
    if r.id == "neoscad"
        && let Some(b) = override_neo
    {
        return b.to_path_buf();
    }
    PathBuf::from(r.binary.replace("{REPO}", &ctx.repo.to_string_lossy()))
}

fn binary_info(bin: &Path, r: &RefConfig) -> Value {
    let version = command_out(&bin.to_string_lossy(), &["--version"])
        .map(|v| v.lines().last().unwrap_or_default().to_string());
    let archs = command_out("lipo", &["-archs", &bin.to_string_lossy()]);
    let rosetta = cfg!(target_arch = "aarch64")
        && archs
            .as_deref()
            .is_some_and(|a| !a.split_whitespace().any(|x| x == "arm64"));
    let mut v = json!({
        "binary": bin.to_string_lossy(),
        "version": version,
        "args": r.args,
        "arch": archs,
    });
    if rosetta {
        v["note"] = json!("x86_64 only: runs under Rosetta 2 on this machine");
    }
    v
}

/// The `{REF}`/`{BOSL2}` form of a model path.
fn expand(ctx: &Ctx, cfg: &Config, p: &str) -> PathBuf {
    let bosl = cfg
        .libraries
        .get("BOSL2")
        .map(|l| ctx.repo.join(&l.path))
        .unwrap_or_default();
    PathBuf::from(
        p.replace("{REF}", &ctx.ref_root.to_string_lossy())
            .replace("{BOSL2}", &bosl.to_string_lossy()),
    )
}

/// The first missing library of `requires`, with its clone command.
fn missing_library(ctx: &Ctx, cfg: &Config, requires: &[String]) -> Option<String> {
    for lib in requires {
        match cfg.libraries.get(lib) {
            Some(l) if ctx.repo.join(&l.path).is_dir() => {}
            Some(l) => {
                return Some(format!(
                    "{lib} not found at {}; clone it with `{}`",
                    l.path, l.clone
                ));
            }
            None => return Some(format!("unknown library {lib}")),
        }
    }
    None
}

pub fn bench(ctx: &Ctx, opts: &BenchOptions) -> Result<u8, String> {
    let cfg_path = ctx.repo.join("conformance/bench.json");
    let cfg: Config = serde_json::from_str(
        &fs::read_to_string(&cfg_path).map_err(|e| format!("{}: {e}", cfg_path.display()))?,
    )
    .map_err(|e| format!("{}: {e}", cfg_path.display()))?;
    let runs = opts.runs.unwrap_or(cfg.runs);
    let timeout = Duration::from_secs_f64(opts.timeout.unwrap_or(cfg.timeout_s));
    let single_over = cfg.single_run_over_s;

    // References: the requested ones that exist.
    for r in &opts.refs {
        if !cfg.references.iter().any(|c| &c.id == r) {
            return Err(format!("unknown reference '{r}'"));
        }
    }
    let mut skipped: BTreeMap<String, String> = BTreeMap::new();
    let mut refs: Vec<(RefConfig, PathBuf)> = Vec::new();
    for r in &cfg.references {
        let wanted = if opts.refs.is_empty() {
            !opts.quick || r.quick
        } else {
            opts.refs.contains(&r.id)
        };
        if !wanted {
            continue;
        }
        let bin = binary_path(ctx, r, opts.binary.as_deref());
        if !bin.is_file() {
            if r.id == "neoscad" {
                return Err(format!(
                    "{} not found; build it with `cargo build --release`",
                    bin.display()
                ));
            }
            eprintln!("note: skipping {}: {} not found", r.id, bin.display());
            skipped.insert(r.id.clone(), format!("{} not found", bin.display()));
            continue;
        }
        refs.push((r.clone(), bin));
    }
    if !refs.iter().any(|(r, _)| r.id == "neoscad") {
        return Err(
            "the neoscad reference must be part of the run (speedups are against it)".into(),
        );
    }
    for m in &opts.only {
        if !cfg.models.contains_key(m) && m != "cold_start" && m != "eval_only" {
            return Err(format!("unknown model '{m}'"));
        }
    }
    let selected = |id: &str| opts.only.is_empty() || opts.only.iter().any(|m| m == id);

    let work = ctx.repo.join("target/conformance/bench");
    let out_dir = work.join("out");
    fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let libpath = ctx.repo.join(".reference");
    let env: [(&str, &Path); 1] = [("OPENSCADPATH", &libpath)];
    let neo_bin = refs
        .iter()
        .find(|(r, _)| r.id == "neoscad")
        .map(|(_, b)| b.clone())
        .expect("checked above");

    let mut models_json = serde_json::Map::new();
    let mut skipped_models: BTreeMap<String, String> = BTreeMap::new();
    let mut failures = 0usize;
    for (id, m) in &cfg.models {
        if !selected(id) {
            continue;
        }
        if let Some(why) = missing_library(ctx, &cfg, &m.requires) {
            eprintln!("note: skipping {id}: {why}");
            skipped_models.insert(id.clone(), why);
            continue;
        }
        let text = match (&m.file, &m.source) {
            (Some(f), _) => {
                let p = expand(ctx, &cfg, f);
                fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?
            }
            (None, Some(s)) => s.clone(),
            (None, None) => return Err(format!("model {id} has neither file nor source")),
        };
        let input = work.join(format!("{id}.scad"));
        fs::write(&input, &text).map_err(|e| e.to_string())?;
        for (name, source) in &m.inputs {
            let target = work.join(name);
            if !target.is_file() {
                eprintln!("generating {name} with neoscad");
                let gen_path = work.join(format!("{name}.scad"));
                fs::write(&gen_path, source).map_err(|e| e.to_string())?;
                let st = Command::new(&neo_bin)
                    .arg("-o")
                    .arg(&target)
                    .arg(&gen_path)
                    .current_dir(&work)
                    .stderr(Stdio::null())
                    .status()
                    .map_err(|e| e.to_string())?;
                if !st.success() {
                    return Err(format!("generating {name} failed"));
                }
            }
        }
        let mut results = serde_json::Map::new();
        let mut meshes: BTreeMap<String, MeshStats> = BTreeMap::new();
        for (r, bin) in &refs {
            let stl = out_dir.join(format!("{id}.{}.stl", r.id));
            let _ = fs::remove_file(&stl);
            let mut cmd = vec![bin.to_string_lossy().into_owned()];
            cmd.extend(r.args.iter().cloned());
            cmd.extend([
                "-o".to_string(),
                stl.to_string_lossy().into_owned(),
                input.to_string_lossy().into_owned(),
            ]);
            let log = out_dir.join(format!("{id}.{}.stderr", r.id));
            let mut res = measure(&cmd, &work, &env, runs, single_over, timeout, &log)?;
            let mesh = (res["rc"] == json!(0))
                .then(|| fs::read(&stl).ok().and_then(|d| mesh_stats(&d)))
                .flatten();
            res["mesh"] = mesh.as_ref().map_or(json!(null), stats_json);
            if let Some(s) = mesh {
                meshes.insert(r.id.clone(), s);
            }
            if res["rc"] != json!(0) && res["rc"] != json!("timeout") {
                failures += 1;
            }
            eprintln!(
                "{id:22} {:18} rc={} best={} runs={}",
                r.id, res["rc"], res["best_s"], res["runs_s"]
            );
            results.insert(r.id.clone(), res);
        }
        // Sanity: every other reference's mesh against neoscad's.
        let mut flags = Vec::new();
        if let Some(neo) = meshes.get("neoscad") {
            for (rid, s) in &meshes {
                if rid == "neoscad" {
                    continue;
                }
                let check = mesh_check(neo, s);
                if let Some(why) = &check {
                    flags.push(format!("{rid}: {why}"));
                }
                if let Some(r) = results.get_mut(rid) {
                    r["mesh_vs_neoscad"] = json!(check.as_deref().unwrap_or("ok"));
                }
            }
        }
        let mut entry = json!({"description": m.description, "results": results});
        if !flags.is_empty() {
            eprintln!("  mesh differs: {}", flags.join("; "));
            entry["mesh_flags"] = json!(flags);
        }
        models_json.insert(id.clone(), entry);
    }

    // Extra metrics.
    let mut extra = serde_json::Map::new();
    if selected("cold_start") {
        let input = work.join("cold_start.scad");
        fs::write(&input, &cfg.cold_start.source).map_err(|e| e.to_string())?;
        let mut results = serde_json::Map::new();
        for (r, bin) in &refs {
            let stl = out_dir.join(format!("cold_start.{}.stl", r.id));
            let mut cmd = vec![bin.to_string_lossy().into_owned()];
            cmd.extend(r.args.iter().cloned());
            cmd.extend([
                "-o".to_string(),
                stl.to_string_lossy().into_owned(),
                input.to_string_lossy().into_owned(),
            ]);
            let log = out_dir.join(format!("cold_start.{}.stderr", r.id));
            let res = measure(
                &cmd,
                &work,
                &env,
                cfg.cold_start.runs.max(runs),
                single_over,
                timeout,
                &log,
            )?;
            eprintln!("{:22} {:18} best={}", "cold_start", r.id, res["best_s"]);
            results.insert(r.id.clone(), res);
        }
        extra.insert(
            "cold_start".into(),
            json!({"description": cfg.cold_start.description, "results": results}),
        );
    }
    if selected("eval_only") {
        match missing_library(ctx, &cfg, &cfg.eval_only.requires) {
            Some(why) => {
                eprintln!("note: skipping eval_only: {why}");
                skipped_models.insert("eval_only".into(), why);
            }
            None => {
                let tests = split_scadtests(
                    &expand(ctx, &cfg, &cfg.eval_only.tests),
                    &work.join("bosl2_tests"),
                )?;
                let mut results = serde_json::Map::new();
                for (r, bin) in refs.iter().filter(|(r, _)| r.eval) {
                    let (mut total, mut cpu, mut passed, mut timeouts) = (0.0, 0.0, 0usize, 0usize);
                    let echo = out_dir.join(format!("eval.{}.echo", r.id));
                    let log = out_dir.join(format!("eval.{}.stderr", r.id));
                    for (path, flags) in &tests {
                        let _ = fs::remove_file(&echo);
                        let mut cmd = vec![bin.to_string_lossy().into_owned()];
                        cmd.extend(r.args.iter().cloned());
                        cmd.extend([
                            "-o".to_string(),
                            echo.to_string_lossy().into_owned(),
                            path.to_string_lossy().into_owned(),
                        ]);
                        let run = time_run(&cmd, &work, &env, timeout, &log)?;
                        total += run.wall_s;
                        cpu += run.cpu_s;
                        timeouts += usize::from(run.timed_out);
                        let text = fs::read_to_string(&echo).unwrap_or_default();
                        passed += usize::from(!run.timed_out && test_passed(*flags, &run, &text));
                    }
                    eprintln!(
                        "{:22} {:18} total={total:.2}s passed={passed}/{}",
                        "eval_only",
                        r.id,
                        tests.len()
                    );
                    results.insert(
                        r.id.clone(),
                        json!({
                            "total_s": round(total, 3),
                            "cpu_s": round(cpu, 3),
                            "tests": tests.len(),
                            "passed": passed,
                            "timeouts": timeouts,
                        }),
                    );
                }
                extra.insert(
                    "eval_only".into(),
                    json!({"description": cfg.eval_only.description, "results": results}),
                );
            }
        }
    }

    // The record.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let (compact, iso) = crate::record::utc_timestamps(now);
    let sha = git(&ctx.repo, &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let short =
        git(&ctx.repo, &["rev-parse", "--short=7", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let subject = git(&ctx.repo, &["log", "-1", "--format=%s"]).unwrap_or_default();
    let branch = git(&ctx.repo, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    let dirty = git(&ctx.repo, &["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    let mut binaries = serde_json::Map::new();
    for (r, bin) in &refs {
        let mut info = binary_info(bin, r);
        if r.id == "neoscad" {
            info["commit"] = json!(sha);
            info["build"] = json!("cargo build --release");
        }
        binaries.insert(r.id.clone(), info);
    }
    let mut libraries = serde_json::Map::new();
    for (name, l) in &cfg.libraries {
        let p = ctx.repo.join(&l.path);
        if let Some(c) = git(&p, &["log", "-1", "--format=%H (%cs)"]) {
            libraries.insert(name.clone(), json!(c));
        }
    }
    let mut doc = json!({
        "schema": 1,
        "measured": &iso[..10],
        "timestamp": iso,
        "sha": sha,
        "short_sha": short,
        "branch": branch,
        "subject": subject,
        "dirty": dirty,
        "quick": opts.quick,
        "runs": runs,
        "timeout_s": timeout.as_secs_f64(),
        "method": format!(
            "Wall time of `<binary> [backend flag] -o out.stl model.scad`, run one after another, best of {runs} (one run once a run takes over {single_over} s); {} s timeout per run. cpu_s is user+sys of the child (getrusage). Working directory is target/conformance/bench; OPENSCADPATH points at .reference (which holds BOSL2); NEOSCAD_FONT_DIR and OPENSCAD_FONT_PATH are unset, so each binary uses its own bundled fonts. ASCII STL output for all. mesh is measured from the last run's STL; mesh_vs_neoscad compares volume, area (0.1%) and bounding box with neoscad's.",
            timeout.as_secs_f64()
        ),
        "machine": machine(),
        "binaries": binaries,
        "libraries": libraries,
        "models": models_json,
        "extra": extra,
    });
    if !skipped.is_empty() {
        doc["skipped_references"] = json!(skipped);
    }
    if !skipped_models.is_empty() {
        doc["skipped_models"] = json!(skipped_models);
    }
    let summary = geomeans(&doc);
    doc["geomean_speedup"] = summary.clone();

    let bench_dir = ctx.progress_dir().join("bench");
    fs::create_dir_all(&bench_dir).map_err(|e| e.to_string())?;
    let base = format!("{compact}-{short}{}", if dirty { "-dirty" } else { "" });
    let mut path = bench_dir.join(format!("{base}.json"));
    let mut n = 2;
    while path.exists() {
        path = bench_dir.join(format!("{base}-{n}.json"));
        n += 1;
    }
    let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())? + "\n";
    fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    let line = json!({
        "file": path.file_name().map(|f| f.to_string_lossy().into_owned()),
        "timestamp": iso,
        "sha": sha,
        "dirty": dirty,
        "subject": subject,
        "quick": opts.quick,
        "references": refs.iter().map(|(r, _)| r.id.clone()).collect::<Vec<_>>(),
        "geomean_speedup": summary,
    });
    let mut index = OpenOptions::new()
        .create(true)
        .append(true)
        .open(bench_dir.join("index.jsonl"))
        .map_err(|e| e.to_string())?;
    writeln!(index, "{line}").map_err(|e| e.to_string())?;

    print_table(&doc);
    println!("wrote {}", path.display());
    Ok(u8::from(failures > 0))
}

/// The speedup of neoscad against each reference over the models where
/// both finished: the geometric mean of `reference time / neoscad time`.
/// Timeouts and failures on either side are left out (and listed), since
/// a timeout has no time to divide.
pub fn geomeans(doc: &Value) -> Value {
    let mut out = serde_json::Map::new();
    let Some(models) = doc["models"].as_object() else {
        return Value::Object(out);
    };
    let refs: Vec<String> = doc["binaries"]
        .as_object()
        .map(|b| b.keys().filter(|k| *k != "neoscad").cloned().collect())
        .unwrap_or_default();
    for r in refs {
        let (mut log_sum, mut n) = (0.0, 0usize);
        let mut excluded = Vec::new();
        for (id, m) in models {
            let res = &m["results"];
            if res.get(&r).is_none() {
                continue;
            }
            match (
                res["neoscad"]["best_s"].as_f64(),
                res[&r]["best_s"].as_f64(),
            ) {
                (Some(a), Some(b)) if a > 0.0 => {
                    log_sum += (b / a).ln();
                    n += 1;
                }
                _ => excluded.push(id.clone()),
            }
        }
        let value = (n > 0).then(|| round((log_sum / n as f64).exp(), 3));
        out.insert(
            r,
            json!({"value": value, "models": n, "excluded_timeouts_or_failures": excluded}),
        );
    }
    Value::Object(out)
}

fn fmt_s(v: &Value) -> String {
    if v["rc"] == json!("timeout") {
        return "timeout".into();
    }
    match v["best_s"].as_f64() {
        Some(x) => format!("{x:.3}"),
        None => format!("rc {}", v["rc"]),
    }
}

fn print_table(doc: &Value) {
    let refs: Vec<String> = doc["binaries"]
        .as_object()
        .map(|b| b.keys().cloned().collect())
        .unwrap_or_default();
    // neoscad first, then the others in the configured order they appear.
    let order = [
        "neoscad",
        "nightly-manifold",
        "nightly-cgal",
        "openscad-2021.01",
    ];
    let refs: Vec<&str> = order
        .iter()
        .copied()
        .filter(|r| refs.iter().any(|x| x == r))
        .collect();
    print!("{:24}", "model");
    for r in &refs {
        print!(" {r:>17}");
    }
    println!();
    let rows = doc["models"]
        .as_object()
        .into_iter()
        .flatten()
        .chain(doc["extra"].as_object().into_iter().flatten());
    for (id, m) in rows {
        let flag = if m.get("mesh_flags").is_some() {
            " !"
        } else {
            ""
        };
        print!("{:24}", format!("{id}{flag}"));
        for r in &refs {
            let v = &m["results"][*r];
            let s = if v.is_null() {
                "-".to_string()
            } else if let Some(t) = v["total_s"].as_f64() {
                format!("{t:.2} ({}/{})", v["passed"], v["tests"])
            } else {
                fmt_s(v)
            };
            print!(" {s:>17}");
        }
        println!();
    }
    println!("geometric mean of reference time / neoscad time (timeouts excluded):");
    if let Some(g) = doc["geomean_speedup"].as_object() {
        for (r, v) in g {
            println!(
                "  {r:18} {}  over {} models; excluded: {}",
                v["value"], v["models"], v["excluded_timeouts_or_failures"]
            );
        }
    }
}

/// The newest result under progress/bench.
pub fn latest(ctx: &Ctx) -> Result<PathBuf, String> {
    let dir = ctx.progress_dir().join("bench");
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "json"))
        .collect();
    files.sort();
    files
        .pop()
        .ok_or_else(|| format!("no benchmark results in {}", dir.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stl_stats_of_a_cube() {
        let stl = "solid OpenSCAD_Model\n  facet normal 0 0 -1\n    outer loop\n      vertex 0 0 0\n      vertex 0 1 0\n      vertex 1 1 0\n    endloop\n  endfacet\nendsolid OpenSCAD_Model\n";
        let s = mesh_stats(stl.as_bytes()).unwrap();
        assert_eq!((s.nv, s.nf), (3, 1));
        assert!((s.area - 0.5).abs() < 1e-12);
        assert_eq!(s.bbox, [0.0, 0.0, 0.0, 1.0, 1.0, 0.0]);
    }

    #[test]
    fn mesh_check_flags_only_real_differences() {
        let a = MeshStats {
            nv: 8,
            nf: 12,
            vol: 1000.0,
            area: 600.0,
            bbox: [0.0, 0.0, 0.0, 10.0, 10.0, 10.0],
        };
        // Different meshing of the same solid: fine.
        let b = MeshStats {
            nv: 26,
            nf: 48,
            vol: 1000.00001,
            ..a.clone()
        };
        assert_eq!(mesh_check(&a, &b), None);
        let c = MeshStats {
            vol: 1007.3,
            ..a.clone()
        };
        assert_eq!(
            mesh_check(&a, &c).as_deref(),
            Some("volume 1007.3000 vs 1000.0000")
        );
        let d = MeshStats {
            bbox: [0.0, 0.0, 0.0, 10.0, 10.0, 11.0],
            ..a.clone()
        };
        assert_eq!(mesh_check(&a, &d).as_deref(), Some("bounding box"));
    }

    #[test]
    fn scadtest_tables_split_into_scripts() {
        let text = "[[test]]\nname = \"test_a\"\nscript = '''\ninclude <../std.scad>\nassert(true);\n'''\n\n[[test]]\nname = \"test_b\"\nexpect_success = false\nscript = '''\nassert(false);\n'''\n";
        let t = parse_scadtest(text);
        assert_eq!(t.len(), 2);
        assert_eq!(t[0].name, "test_a");
        assert_eq!(t[0].script, "include <../std.scad>\nassert(true);\n");
        assert!(t[0].flags.expect_success);
        assert!(!t[1].flags.expect_success);
    }

    #[test]
    fn geomean_excludes_timeouts() {
        let doc = json!({
            "binaries": {"neoscad": {}, "ref": {}},
            "models": {
                "a": {"results": {"neoscad": {"best_s": 1.0}, "ref": {"best_s": 4.0}}},
                "b": {"results": {"neoscad": {"best_s": 2.0}, "ref": {"best_s": 2.0}}},
                "c": {"results": {"neoscad": {"best_s": 1.0}, "ref": {"rc": "timeout", "best_s": null}}},
            }
        });
        let g = geomeans(&doc);
        assert_eq!(g["ref"]["value"], json!(2.0));
        assert_eq!(g["ref"]["models"], json!(2));
        assert_eq!(g["ref"]["excluded_timeouts_or_failures"], json!(["c"]));
    }
}
