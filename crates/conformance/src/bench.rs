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
//! Reference results (everything but neoscad) are reused from
//! `progress/bench/ref-cache.json` while their key matches; see
//! `bench_cache.rs`.
//!
//! Results go to `progress/bench/<UTC>-<sha>[-dirty].json` in the audit's
//! schema (`docs/audits/engine-milestone-bench.json`) plus the commit,
//! subject and dirty flag, a line is appended to `progress/bench/index.jsonl`,
//! and a table is printed. `conformance bench-chart` draws a result file.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::bench_cache::{self, Measured, RefCache};
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
    #[serde(default)]
    edit_loop: Option<crate::edit_loop::EditLoop>,
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
    /// Which cached reference results may be reused.
    pub cache: crate::bench_cache::Policy,
    /// `--seed-refs`: seed the cache from these result files and stop.
    pub seed_refs: Vec<PathBuf>,
}

/// One process run (the timing itself is shared with `neoscad bench`, in
/// crates/bench-core, so both measure the same way).
pub(crate) use bench_core::timing::Run;
use bench_core::timing::round;

/// Run `cmd` in `cwd`, timing it (`bench_core::timing::time_run`); its
/// stderr goes to `stderr_to` for diagnosis. Never answered by a running
/// server.
fn time_run(
    cmd: &[String],
    cwd: &Path,
    env: &[(&str, &Path)],
    timeout: Duration,
    stderr_to: &Path,
) -> Result<Run, String> {
    time_run_with(cmd, cwd, env, timeout, stderr_to, false)
}

/// [`time_run`]; with `allow_server` a running `neoscad serve` (named by
/// `NEOSCAD_SOCKET` in `env`) may answer, otherwise the run is cold.
pub(crate) fn time_run_with(
    cmd: &[String],
    cwd: &Path,
    env: &[(&str, &Path)],
    timeout: Duration,
    stderr_to: &Path,
    allow_server: bool,
) -> Result<Run, String> {
    bench_core::timing::time_run(cmd, cwd, &run_env(env, allow_server), timeout, stderr_to)
}

/// `env` for a run: cold runs are cold, never answered by a running server.
fn run_env<'a>(env: &[(&'a str, &'a Path)], allow_server: bool) -> Vec<(&'a str, &'a OsStr)> {
    let mut out: Vec<(&str, &OsStr)> = Vec::new();
    if !allow_server {
        out.push((crate::geometry::NO_SERVER_VAR, OsStr::new("1")));
    }
    out.extend(env.iter().map(|(k, v)| (*k, v.as_os_str())));
    out
}

/// Best-of-N timing of one command (`bench_core::timing::measure`, the
/// audit's method), in this crate's result layout.
fn measure(
    cmd: &[String],
    cwd: &Path,
    env: &[(&str, &Path)],
    runs: u32,
    single_over: f64,
    timeout: Duration,
    stderr_to: &Path,
) -> Result<Value, String> {
    let m = bench_core::timing::measure(
        cmd,
        cwd,
        &run_env(env, false),
        runs,
        single_over,
        timeout,
        stderr_to,
    )?;
    Ok(json!({
        "rc": m.rc,
        "runs_s": m.runs_s,
        "best_s": m.best_s,
        "cpu_s_of_runs": m.cpu_s,
    }))
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
/// flags that decide its outcome.
fn split_scadtests(dir: &Path, out: &Path) -> Result<Vec<(PathBuf, TestFlags)>, String> {
    fs::create_dir_all(out).map_err(|e| e.to_string())?;
    let mut tests = Vec::new();
    for (file, t) in read_scadtests(dir)? {
        // The scripts include `<../std.scad>` relative to tests/; through
        // the library path they find the same files as `<BOSL2/...>`.
        let script = t.script.replace("include <../", "include <BOSL2/");
        let script = script.replace("use <../", "use <BOSL2/");
        let path = out.join(file);
        fs::write(&path, script).map_err(|e| e.to_string())?;
        tests.push((path, t.flags));
    }
    Ok(tests)
}

/// Every test of BOSL2's `.scadtest` files in `dir`, in file order, each
/// with its file name: `<file stem without test_>__<name>.scad`. The
/// format is a TOML subset: `[[test]]` tables of `name = "..."`,
/// `script = '''...'''` and boolean flags.
///
/// Two tests can share a name (`test_utility` has two `test_segs`); the
/// second is `..._2.scad`, as the audit's extraction named it, so that it
/// does not overwrite the first.
pub(crate) fn read_scadtests(dir: &Path) -> Result<Vec<(String, ScadTest)>, String> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| format!("{}: {e}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "scadtest"))
        .collect();
    files.sort();
    let mut tests: Vec<(String, ScadTest)> = Vec::new();
    let mut taken = std::collections::HashSet::new();
    for f in files {
        let stem = f
            .file_stem()
            .map(|s| s.to_string_lossy().trim_start_matches("test_").to_string())
            .unwrap_or_default();
        let text = fs::read_to_string(&f).map_err(|e| format!("{}: {e}", f.display()))?;
        for t in parse_scadtest(&text) {
            let mut file = format!("{stem}__{}.scad", t.name);
            let mut k = 2;
            while !taken.insert(file.clone()) {
                file = format!("{stem}__{}_{k}.scad", t.name);
                k += 1;
            }
            tests.push((file, t));
        }
    }
    Ok(tests)
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TestFlags {
    expect_success: bool,
    assert_no_echoes: bool,
    assert_no_warnings: bool,
}

#[derive(Debug)]
pub(crate) struct ScadTest {
    pub name: String,
    pub script: String,
    flags: TestFlags,
    /// Every key but `script`, with its raw value, in the file's order.
    pub keys: Vec<(String, String)>,
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
                keys: Vec::new(),
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
        if key != "script" {
            t.keys.push((key.to_string(), value.to_string()));
        }
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
    if !opts.seed_refs.is_empty() {
        return seed_refs(ctx, &opts.seed_refs);
    }

    // References: the requested ones that exist.
    for r in opts.refs.iter().chain(&opts.cache.fresh) {
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
        if !cfg.models.contains_key(m) && m != "cold_start" && m != "eval_only" && m != "edit_loop"
        {
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

    // The tree and machine doing the measuring, and each reference's
    // cache-key parts that don't depend on the model.
    let sha = git(&ctx.repo, &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&ctx.repo, &["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    let machine = machine();
    let mut results_src = RefResults {
        cache: RefCache::load(&cache_path(ctx)),
        policy: opts.cache.clone(),
        env: run_env_identity(&work, &libpath),
        machine: machine.clone(),
        binaries: HashMap::new(),
        sha: sha.clone(),
        dirty,
        hits: 0,
        misses: 0,
    };
    let mut identities: HashMap<PathBuf, Value> = HashMap::new();
    for (r, bin) in refs.iter().filter(|(r, _)| r.id != "neoscad") {
        if !identities.contains_key(bin) {
            let version = command_out(&bin.to_string_lossy(), &["--version"]);
            identities.insert(
                bin.clone(),
                bench_cache::binary_identity(bin, version.as_deref())?,
            );
        }
        results_src
            .binaries
            .insert(r.id.clone(), identities[bin].clone());
    }

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
        let identity = model_identity(ctx, &cfg, &text, &m.requires, &m.inputs, &work)?;
        let method = models_method(runs, single_over, timeout);
        let mut results = serde_json::Map::new();
        let mut meshes: BTreeMap<String, MeshStats> = BTreeMap::new();
        for (r, bin) in &refs {
            let stl = out_dir.join(format!("{id}.{}.stl", r.id));
            // Removed even for a cached result, so no STL from an older
            // run sits there looking like this run's.
            let _ = fs::remove_file(&stl);
            let res = results_src.get("model", id, r, &identity, &method, || {
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
                Ok(res)
            })?;
            // A cached result's mesh is compared with this run's neoscad
            // like a fresh one's, from the stats it was stored with.
            if let Some(s) = stats_from_json(&res["mesh"]) {
                meshes.insert(r.id.clone(), s);
            }
            if res["rc"] != json!(0) && res["rc"] != json!("timeout") {
                failures += 1;
            }
            eprintln!(
                "{id:22} {:18} rc={} best={} runs={}{}",
                r.id,
                res["rc"],
                res["best_s"],
                res["runs_s"],
                cached_note(&res)
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
        let identity = bench_cache::model_identity(&cfg.cold_start.source, &BTreeMap::new(), None);
        let cs_runs = cfg.cold_start.runs.max(runs);
        let method = models_method(cs_runs, single_over, timeout);
        let mut results = serde_json::Map::new();
        for (r, bin) in &refs {
            let stl = out_dir.join(format!("cold_start.{}.stl", r.id));
            let res = results_src.get("cold_start", "cold_start", r, &identity, &method, || {
                let mut cmd = vec![bin.to_string_lossy().into_owned()];
                cmd.extend(r.args.iter().cloned());
                cmd.extend([
                    "-o".to_string(),
                    stl.to_string_lossy().into_owned(),
                    input.to_string_lossy().into_owned(),
                ]);
                let log = out_dir.join(format!("cold_start.{}.stderr", r.id));
                measure(&cmd, &work, &env, cs_runs, single_over, timeout, &log)
            })?;
            eprintln!(
                "{:22} {:18} best={}{}",
                "cold_start",
                r.id,
                res["best_s"],
                cached_note(&res)
            );
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
                let identity = eval_identity(ctx, &cfg, &tests)?;
                let method = eval_method(timeout);
                let mut results = serde_json::Map::new();
                for (r, bin) in refs.iter().filter(|(r, _)| r.eval) {
                    let res =
                        results_src.get("eval_only", "eval_only", r, &identity, &method, || {
                            let (mut total, mut cpu, mut passed, mut timeouts) =
                                (0.0, 0.0, 0usize, 0usize);
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
                                passed +=
                                    usize::from(!run.timed_out && test_passed(*flags, &run, &text));
                            }
                            Ok(json!({
                                "total_s": round(total, 3),
                                "cpu_s": round(cpu, 3),
                                "tests": tests.len(),
                                "passed": passed,
                                "timeouts": timeouts,
                            }))
                        })?;
                    eprintln!(
                        "{:22} {:18} total={}s passed={}/{}{}",
                        "eval_only",
                        r.id,
                        res["total_s"],
                        res["passed"],
                        res["tests"],
                        cached_note(&res)
                    );
                    results.insert(r.id.clone(), res);
                }
                extra.insert(
                    "eval_only".into(),
                    json!({"description": cfg.eval_only.description, "results": results}),
                );
            }
        }
    }

    if selected("edit_loop")
        && let Some(el) = &cfg.edit_loop
    {
        let nightly = refs
            .iter()
            .find(|(r, _)| r.id == "nightly-manifold")
            .map(|(r, b)| (b.clone(), r.args.clone()));
        let ctx_el = crate::edit_loop::Setup {
            neoscad: &neo_bin,
            nightly: nightly.as_ref().map(|(b, a)| (b.as_path(), a.as_slice())),
            work: &work,
            libpath: &libpath,
            timeout,
            runs: opts.runs,
        };
        let expand_file = |f: &str| expand(ctx, &cfg, f);
        let missing = |req: &[String]| missing_library(ctx, &cfg, req);
        let v = crate::edit_loop::run(el, &ctx_el, &expand_file, &missing)?;
        extra.insert("edit_loop".into(), v);
    }

    // The record.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let (compact, iso) = crate::record::utc_timestamps(now);
    let short =
        git(&ctx.repo, &["rev-parse", "--short=7", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let subject = git(&ctx.repo, &["log", "-1", "--format=%s"]).unwrap_or_default();
    let branch = git(&ctx.repo, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    let mut binaries = serde_json::Map::new();
    for (r, bin) in &refs {
        let mut info = binary_info(bin, r);
        if r.id == "neoscad" {
            info["commit"] = json!(sha);
            info["build"] = json!("cargo build --release");
        }
        // The executable's identity, as the cache keys it: what a later
        // seeding needs to prove a result came from the same binary.
        if let Some(id) = results_src.binaries.get(&r.id) {
            info["size"] = id["size"].clone();
            info["sha256"] = id["sha256"].clone();
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
            "Wall time of `<binary> [backend flag] -o out.stl model.scad`, run one after another, best of {runs} (one run once a run takes over {single_over} s); {} s timeout per run. cpu_s is user+sys of the child (getrusage). Working directory is target/conformance/bench; OPENSCADPATH points at .reference (which holds BOSL2); NEOSCAD_FONT_DIR and OPENSCAD_FONT_PATH are unset, so each binary uses its own bundled fonts. ASCII STL output for all. mesh is measured from the last run's STL; mesh_vs_neoscad compares volume, area (0.1%) and bounding box with neoscad's. neoscad is measured every run; a reference result may come from progress/bench/ref-cache.json, measured by an earlier run with the same binary, model, method, environment and machine: see its cached and measured_at.",
            timeout.as_secs_f64()
        ),
        "machine": machine,
        "binaries": binaries,
        "libraries": libraries,
        "models": models_json,
        "extra": extra,
    });
    doc["ref_cache"] = json!({
        "file": "progress/bench/ref-cache.json",
        "hits": results_src.hits,
        "misses": results_src.misses,
        "fresh_refs": opts.cache.fresh_all,
        "fresh_ref": opts.cache.fresh,
        "refs_max_age_days": opts.cache.max_age_days,
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
    println!(
        "reference results: {} cached, {} measured (progress/bench/ref-cache.json)",
        results_src.hits, results_src.misses
    );
    println!("wrote {}", path.display());
    Ok(u8::from(failures > 0))
}

/// Where reference results are cached.
fn cache_path(ctx: &Ctx) -> PathBuf {
    ctx.progress_dir().join("bench/ref-cache.json")
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// The environment every run gets (see `time_run_with`), as a cache key
/// part.
fn run_env_identity(work: &Path, libpath: &Path) -> Value {
    let no_server = crate::geometry::NO_SERVER_VAR;
    bench_cache::env_identity(
        work,
        libpath,
        &[(no_server, "1")],
        &bench_cache::inherited_env(&["OPENSCADPATH", no_server]),
    )
}

/// The method of a model (or cold start) result, as a cache key part. The
/// timeout is exact: a timeout under a shorter limit says nothing about a
/// longer one, and a result under a longer limit might not be one under a
/// shorter.
fn models_method(runs: u32, single_over: f64, timeout: Duration) -> Value {
    json!({
        "measure": "best wall time of runs, one run once a run takes over single_run_over_s",
        "output": "ascii stl",
        "runs": runs,
        "single_run_over_s": single_over,
        "timeout_s": timeout.as_secs_f64(),
    })
}

/// The method of an `eval_only` result, as a cache key part.
fn eval_method(timeout: Duration) -> Value {
    json!({
        "measure": "summed wall time of one run per test",
        "output": "echo",
        "timeout_s": timeout.as_secs_f64(),
    })
}

/// The libraries a model with `requires` (or, lacking one, an include)
/// may read: the named ones, or every configured one.
fn model_libraries(ctx: &Ctx, cfg: &Config, requires: &[String]) -> Vec<(String, PathBuf)> {
    cfg.libraries
        .iter()
        .filter(|(name, _)| requires.is_empty() || requires.contains(name))
        .map(|(name, l)| (name.clone(), ctx.repo.join(&l.path)))
        .collect()
}

/// A model's identity for the cache: its text, its generated inputs (by
/// content: they are neoscad's output, so a rebuilt neoscad may change
/// them), and the library corpus it can include from.
fn model_identity(
    ctx: &Ctx,
    cfg: &Config,
    text: &str,
    requires: &[String],
    inputs: &BTreeMap<String, String>,
    work: &Path,
) -> Result<Value, String> {
    let mut hashes = BTreeMap::new();
    for name in inputs.keys() {
        let p = work.join(name);
        let data = fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
        hashes.insert(name.clone(), crate::sha256::hex(&data));
    }
    let libs = bench_cache::reads_libraries(text, requires).then(|| {
        bench_cache::library_fingerprint(
            &ctx.repo.join(".reference"),
            &model_libraries(ctx, cfg, requires),
        )
    });
    Ok(bench_cache::model_identity(text, &hashes, libs.as_ref()))
}

/// The `eval_only` suite's identity: every split test script and its
/// flags, and the corpus they include.
fn eval_identity(ctx: &Ctx, cfg: &Config, tests: &[(PathBuf, TestFlags)]) -> Result<Value, String> {
    let mut all = String::new();
    for (p, flags) in tests {
        let text = fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy())
            .unwrap_or_default();
        all.push_str(&format!("{name}\0{flags:?}\0{text}\0"));
    }
    let libs = bench_cache::library_fingerprint(
        &ctx.repo.join(".reference"),
        &model_libraries(ctx, cfg, &cfg.eval_only.requires),
    );
    Ok(bench_cache::model_identity(
        &all,
        &BTreeMap::new(),
        Some(&libs),
    ))
}

/// Mesh stats back from their JSON (as rounded by `stats_json`: to 1e-4,
/// far inside the comparison's 0.1%).
fn stats_from_json(v: &Value) -> Option<MeshStats> {
    let b = v["bbox"].as_array()?;
    let mut bbox = [0.0; 6];
    for (k, x) in bbox.iter_mut().enumerate() {
        *x = b.get(k)?.as_f64()?;
    }
    Some(MeshStats {
        nv: usize::try_from(v["nv"].as_u64()?).ok()?,
        nf: usize::try_from(v["nf"].as_u64()?).ok()?,
        vol: v["vol"].as_f64()?,
        area: v["area"].as_f64()?,
        bbox,
    })
}

fn cached_note(res: &Value) -> String {
    if res["cached"] == json!(true) {
        format!(
            " (cached, measured {})",
            res["measured_at"].as_str().unwrap_or("?")
        )
    } else {
        String::new()
    }
}

/// Reference results for one run: from the cache where the key matches,
/// else measured and stored.
struct RefResults {
    cache: RefCache,
    policy: bench_cache::Policy,
    env: Value,
    machine: Value,
    /// Binary identity by reference id (all but neoscad).
    binaries: HashMap<String, Value>,
    sha: String,
    dirty: bool,
    hits: usize,
    misses: usize,
}

impl RefResults {
    /// The result of reference `r` on `model`. neoscad always runs `run`:
    /// it is what the benchmark measures. Another reference's result comes
    /// from the cache on a key match; on a miss `run` measures it and the
    /// result is stored, unless it failed (a failure is cheap to repeat and
    /// more likely a broken setup than a property of the binary).
    fn get(
        &mut self,
        kind: &str,
        model: &str,
        r: &RefConfig,
        identity: &Value,
        method: &Value,
        run: impl FnOnce() -> Result<Value, String>,
    ) -> Result<Value, String> {
        if r.id == "neoscad" {
            return run();
        }
        let binary = self.binaries.get(&r.id).cloned().unwrap_or(Value::Null);
        let key = bench_cache::key(
            kind,
            &binary,
            &r.args,
            &self.env,
            identity,
            method,
            &self.machine,
        );
        match self
            .cache
            .lookup(&r.id, model, &key, &self.policy, now_unix())
        {
            Ok(v) => {
                self.hits += 1;
                return Ok(v);
            }
            Err(why) => eprintln!("{model:22} {:18} measuring ({why})", r.id),
        }
        self.misses += 1;
        let mut v = run()?;
        let at = now_unix();
        let m = Measured {
            at_unix: at,
            at: crate::record::utc_timestamps(at).1,
            sha: self.sha.clone(),
            dirty: self.dirty,
            seeded_from: None,
        };
        let cacheable = v
            .get("rc")
            .is_none_or(|rc| *rc == json!(0) || *rc == json!("timeout"));
        if cacheable {
            self.cache.store(&r.id, model, &key, &v, &m)?;
        }
        v["cached"] = json!(false);
        v["measured_at"] = json!(m.at);
        Ok(v)
    }
}

/// `--seed-refs FILE...`: fill the cache from earlier result files, taking
/// only results whose whole key can be reconstructed exactly. A result
/// file records the binaries' paths and versions, the method, the machine
/// and the library commits, but not the executables' hashes or the model
/// files' contents; those are taken from disk now, and accepted only where
/// the file's status-change time (which no tool can set back) proves it is
/// unchanged since before the run began. Anything unprovable is left out
/// and reported: a wrong seed would be believed on every later run.
fn seed_refs(ctx: &Ctx, files: &[PathBuf]) -> Result<u8, String> {
    let mut cache = RefCache::load(&cache_path(ctx));
    let before = cache.len();
    for f in files {
        let path = if f.is_file() {
            f.clone()
        } else {
            ctx.progress_dir().join("bench").join(f)
        };
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let doc: Value = serde_json::from_str(
            &fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?,
        )
        .map_err(|e| format!("{}: {e}", path.display()))?;
        match seed_one(ctx, &doc, &name, &mut cache) {
            Ok((n, rejected)) => {
                println!("{name}: seeded {n} results");
                for why in rejected {
                    println!("  not seeded: {why}");
                }
            }
            Err(why) => println!("{name}: not seeded: {why}"),
        }
    }
    cache.save()?;
    println!(
        "{} entries in progress/bench/ref-cache.json ({} before)",
        cache.len(),
        before
    );
    Ok(0)
}

/// Seed from one result file: the number seeded and what was left out, or
/// why the whole file can't be used.
fn seed_one(
    ctx: &Ctx,
    doc: &Value,
    name: &str,
    cache: &mut RefCache,
) -> Result<(usize, Vec<String>), String> {
    if doc["dirty"] != json!(false) {
        return Err(
            "measured from a dirty tree, so the bench.json and timing code it ran are unknown"
                .into(),
        );
    }
    let sha = doc["sha"].as_str().ok_or("no sha")?;
    if git(
        &ctx.repo,
        &[
            "merge-base",
            "--is-ancestor",
            bench_cache::METHOD_SINCE,
            sha,
        ],
    )
    .is_none()
    {
        return Err(format!(
            "{} predates the current timing code ({})",
            &sha[..sha.len().min(7)],
            &bench_cache::METHOD_SINCE[..7]
        ));
    }
    let cfg_text = git(
        &ctx.repo,
        &["show", &format!("{sha}:conformance/bench.json")],
    )
    .ok_or("no conformance/bench.json at its commit")?;
    let cfg: Config = serde_json::from_str(&cfg_text).map_err(|e| e.to_string())?;
    // The files proven unchanged below are this machine's; the key's
    // machine is the file's own (power included).
    let machine_now = machine();
    for k in ["model", "cores", "memory_gb", "os"] {
        if doc["machine"][k] != machine_now[k] {
            return Err(format!(
                "measured on another machine or OS ({k}: {} vs {})",
                doc["machine"][k], machine_now[k]
            ));
        }
    }
    let default_neo = ctx.default_binary();
    if doc["binaries"]["neoscad"]["binary"] != json!(default_neo.to_string_lossy()) {
        return Err("measured from another checkout or with --binary".into());
    }
    let end = doc["timestamp"]
        .as_str()
        .and_then(bench_cache::unix_from_iso)
        .ok_or("no timestamp")?;
    // The timestamp is the run's end. Files must predate its start, which
    // isn't recorded: bound it by the time the recorded runs took, plus an
    // hour for everything unrecorded (input generation, the edit loop).
    let bound = i64::try_from(end).map_err(|e| e.to_string())?
        - recorded_duration_s(doc).ceil() as i64
        - 3600;
    let unchanged = |p: &Path| bench_cache::ctime(p).is_some_and(|c| c < bound);
    let runs = u32::try_from(doc["runs"].as_u64().ok_or("no runs")?).map_err(|e| e.to_string())?;
    let timeout = Duration::from_secs_f64(doc["timeout_s"].as_f64().ok_or("no timeout_s")?);
    let work = ctx.repo.join("target/conformance/bench");
    let libpath = ctx.repo.join(".reference");
    let env = run_env_identity(&work, &libpath);
    let mut rejected = Vec::new();

    // References whose binary is provably the one that ran.
    let mut binaries: Vec<(RefConfig, Value)> = Vec::new();
    for (rid, info) in doc["binaries"].as_object().ok_or("no binaries")? {
        if rid == "neoscad" {
            continue;
        }
        let check = || -> Result<(RefConfig, Value), String> {
            let r = cfg
                .references
                .iter()
                .find(|r| &r.id == rid)
                .ok_or("not in bench.json at its commit")?;
            if info["args"] != json!(r.args) {
                return Err("other arguments".into());
            }
            let bin = binary_path(ctx, r, None);
            let then = info["binary"]
                .as_str()
                .map(PathBuf::from)
                .unwrap_or_default();
            if fs::canonicalize(&then).ok() != fs::canonicalize(&bin).ok() {
                return Err(format!("binary was {}", then.display()));
            }
            let version = command_out(&bin.to_string_lossy(), &["--version"]);
            let last = version
                .as_deref()
                .and_then(|v| v.lines().last())
                .map(str::to_string);
            if json!(last) != info["version"] {
                return Err(format!("version now {last:?}"));
            }
            let plist = bench_cache::bundle_info_plist(&bin);
            if !unchanged(&bin) || plist.is_some_and(|p| !unchanged(&p)) {
                return Err(format!("{} changed since the run", bin.display()));
            }
            Ok((
                r.clone(),
                bench_cache::binary_identity(&bin, version.as_deref())?,
            ))
        };
        match check() {
            Ok(b) => binaries.push(b),
            Err(why) => rejected.push(format!("{rid}: {why}")),
        }
    }

    // The library corpus, if proven unchanged: same commit, every tracked
    // file and the library directory's listing untouched since the run.
    let prove_libs = |libs: &[(String, PathBuf)]| -> Result<(), String> {
        if !unchanged(&libpath) {
            return Err(format!("{} changed since the run", libpath.display()));
        }
        for (lib, p) in libs {
            let then = doc["libraries"][lib]
                .as_str()
                .and_then(|s| s.split_whitespace().next())
                .ok_or(format!("no {lib} commit recorded"))?;
            let now = git(p, &["rev-parse", "HEAD"]).unwrap_or_default();
            if now != then {
                return Err(format!("{lib} was at {then}, now {now}"));
            }
            let listed = git(p, &["ls-files"]).unwrap_or_default();
            if let Some(f) = listed.lines().find(|f| !unchanged(&p.join(f))) {
                return Err(format!("{lib}/{f} changed since the run"));
            }
        }
        Ok(())
    };

    let measured = Measured {
        at_unix: end,
        at: doc["timestamp"].as_str().unwrap_or_default().to_string(),
        sha: sha.to_string(),
        dirty: false,
        seeded_from: Some(name.to_string()),
    };
    let mut seeded = 0usize;
    let mut seed = |kind: &str, model: &str, identity: &Value, method: &Value, results: &Value| {
        for (r, binary) in &binaries {
            let res = &results[&r.id];
            if res.is_null()
                || !(res
                    .get("rc")
                    .is_none_or(|rc| *rc == json!(0) || *rc == json!("timeout")))
            {
                continue;
            }
            let key = bench_cache::key(
                kind,
                binary,
                &r.args,
                &env,
                identity,
                method,
                &doc["machine"],
            );
            if cache.seed(&r.id, model, &key, res, &measured) {
                seeded += 1;
            }
        }
    };

    let method = models_method(runs, cfg.single_run_over_s, timeout);
    for (id, entry) in doc["models"].as_object().into_iter().flatten() {
        let Some(m) = cfg.models.get(id) else {
            rejected.push(format!("{id}: not in bench.json at its commit"));
            continue;
        };
        let prove = || -> Result<Value, String> {
            let text = match (&m.file, &m.source) {
                (Some(f), _) => {
                    let p = expand(ctx, &cfg, f);
                    if !unchanged(&p) {
                        return Err(format!("{} changed since the run", p.display()));
                    }
                    fs::read_to_string(&p).map_err(|e| e.to_string())?
                }
                (None, Some(s)) => s.clone(),
                (None, None) => return Err("neither file nor source".into()),
            };
            for n in m.inputs.keys() {
                if !unchanged(&work.join(n)) {
                    return Err(format!("input {n} regenerated since the run"));
                }
            }
            if bench_cache::reads_libraries(&text, &m.requires) {
                prove_libs(&model_libraries(ctx, &cfg, &m.requires))?;
            }
            model_identity(ctx, &cfg, &text, &m.requires, &m.inputs, &work)
        };
        match prove() {
            Ok(identity) => seed("model", id, &identity, &method, &entry["results"]),
            Err(why) => rejected.push(format!("{id}: {why}")),
        }
    }
    if let Some(cs) = doc["extra"].get("cold_start") {
        let identity = bench_cache::model_identity(&cfg.cold_start.source, &BTreeMap::new(), None);
        let method = models_method(
            cfg.cold_start.runs.max(runs),
            cfg.single_run_over_s,
            timeout,
        );
        seed(
            "cold_start",
            "cold_start",
            &identity,
            &method,
            &cs["results"],
        );
    }
    if let Some(ev) = doc["extra"].get("eval_only") {
        let prove = || -> Result<Value, String> {
            prove_libs(&model_libraries(ctx, &cfg, &cfg.eval_only.requires))?;
            let tests = split_scadtests(
                &expand(ctx, &cfg, &cfg.eval_only.tests),
                &work.join("bosl2_tests"),
            )?;
            eval_identity(ctx, &cfg, &tests)
        };
        match prove() {
            Ok(identity) => seed(
                "eval_only",
                "eval_only",
                &identity,
                &eval_method(timeout),
                &ev["results"],
            ),
            Err(why) => rejected.push(format!("eval_only: {why}")),
        }
    }
    Ok((seeded, rejected))
}

/// Seconds of process time a result file records: every run of every
/// model (a timeout counted at the limit), cold starts and the eval suite.
fn recorded_duration_s(doc: &Value) -> f64 {
    let timeout = doc["timeout_s"].as_f64().unwrap_or(0.0);
    let runs = |results: &Value| -> f64 {
        results
            .as_object()
            .into_iter()
            .flatten()
            .map(|(_, r)| {
                let walls: f64 = r["runs_s"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|w| w.as_f64().unwrap_or(timeout))
                    .sum();
                walls + r["total_s"].as_f64().unwrap_or(0.0)
            })
            .sum()
    };
    let models: f64 = doc["models"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(_, m)| runs(&m["results"]))
        .sum();
    let extra: f64 = ["cold_start", "eval_only"]
        .iter()
        .map(|k| runs(&doc["extra"][k]["results"]))
        .sum();
    models + extra
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
        // Result files are named `<UTC>-<sha>`; the directory also holds
        // ref-cache.json, which sorts after them and isn't a result.
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(|c: char| c.is_ascii_digit()))
        })
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
    fn cached_mesh_stats_round_trip() {
        let s = MeshStats {
            nv: 8,
            nf: 12,
            vol: 1000.00004,
            area: 600.0,
            bbox: [-5.0, 0.0, 0.0, 5.0, 10.0, 10.12345],
        };
        let back = stats_from_json(&stats_json(&s)).unwrap();
        assert_eq!((back.nv, back.nf), (8, 12));
        assert_eq!(mesh_check(&s, &back), None);
        assert!(stats_from_json(&json!(null)).is_none());
    }

    #[test]
    fn recorded_duration_counts_timeouts_at_the_limit() {
        let doc = json!({
            "timeout_s": 300.0,
            "models": {"a": {"results": {
                "neoscad": {"runs_s": [1.0, 2.0]},
                "cgal": {"runs_s": [null]}}}},
            "extra": {"eval_only": {"results": {"neoscad": {"total_s": 30.0}}}},
        });
        assert_eq!(recorded_duration_s(&doc), 333.0);
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
