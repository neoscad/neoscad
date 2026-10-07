//! `conformance exact`: the stop-rule measurement of exact STEP export
//! (`docs/audits/exact-geometry-rust.md`, section 10, gate 1).
//!
//! Every model is exported with `neoscad --enable exact --format json -o
//! x.step` in its own process, and the `exact` section of the JSON report
//! is tabulated per corpus:
//!
//! - `cases`: the audit's 28 cases (`conformance/extensions/exact`) at
//!   several resolutions. Explicit `$fn` would keep polygons, so the
//!   resolutions are `$fa`/`$fs` settings: OpenSCAD's defaults, `$fs=0.5`,
//!   a coarse `$fa=60` (5 fragments, 8 in the export render) and a fine
//!   `$fa=3; $fs=0.2`. Each case's `// volume:` comment is the closed
//!   form that gate 3 holds the exact volume to (1e-6).
//! - `conformance`: the distinct inputs of OpenSCAD's `render-manifold`
//!   tests. Models whose render is 2D, empty or an error are not counted.
//! - `bosl2`: every Nth BOSL2 documentation example (`examples_x`, from
//!   `conformance bosl2-corpus`).
//! - `bench`: the models of `conformance/bench.json`.
//!
//! A model is **eligible** when nothing in it fell back to facets (an
//! ellipse from a non-uniform scale aside): the stop rule's "no mesh-only
//! construct". Its outcome is **valid** (the export passed reconstruction,
//! validation and the volume and box cross-checks), **failed** (a reported
//! export error), or **killed** (over the time or the 2 GB memory guard,
//! which cannot tell the render's share from the export's).
//!
//! With `--occt PATH` (or `MESHBREP_OCCT_CHECK`), every written file is
//! read back by OCCT (`crates/meshbrep/oracle`): valid closed solids (one
//! per separate body) with no free edges, and OCCT's volume within 1e-6 of
//! ours.
//!
//! Results go to `target/conformance/exact/results-<corpora>.json` (never
//! `progress/`).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::ctx::Ctx;

/// What `conformance exact` was asked to do.
#[derive(Debug)]
pub struct ExactOptions {
    pub corpora: Vec<String>,
    pub bosl2_every: usize,
    pub jobs: usize,
    pub timeout: Duration,
    pub binary: Option<PathBuf>,
    pub occt: Option<PathBuf>,
    pub filter: Option<String>,
}

/// The process guard: a child above this resident size is killed. Two
/// runaway sweeps have filled the owner's swap before (CLAUDE.md).
const MEMORY_GUARD_KIB: u64 = 2 * 1024 * 1024;

const RESOLUTIONS: [(&str, &str); 4] = [
    ("default", ""),
    ("fs0.5", "$fs=0.5"),
    ("fa60", "$fa=60"),
    ("fine", "$fa=3;$fs=0.2"),
];

#[derive(Debug, Clone)]
struct Model {
    corpus: &'static str,
    id: String,
    file: PathBuf,
    define: Option<String>,
    reference_volume: Option<f64>,
}

#[derive(Debug, Clone, Default)]
struct Outcome {
    /// `valid`, `failed`, `killed`, or `not3d` (not counted).
    status: &'static str,
    eligible: bool,
    exact: Option<Value>,
    message: String,
    step: Option<PathBuf>,
    wall_ms: f64,
    occt: Option<Value>,
}

fn models(ctx: &Ctx, opts: &ExactOptions, work: &Path) -> Result<Vec<Model>, String> {
    let mut out = Vec::new();
    let want = |c: &str| opts.corpora.is_empty() || opts.corpora.iter().any(|x| x == c);
    if want("cases") {
        let dir = ctx.repo.join("conformance/extensions/exact");
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map_err(|e| format!("{}: {e}", dir.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "scad"))
            .collect();
        files.sort();
        for f in files {
            let text = std::fs::read_to_string(&f).map_err(|e| e.to_string())?;
            let reference_volume = text
                .lines()
                .find_map(|l| l.strip_prefix("// volume: "))
                .and_then(|v| v.trim().parse().ok());
            let stem = f.file_stem().unwrap().to_string_lossy().into_owned();
            for (name, define) in RESOLUTIONS {
                out.push(Model {
                    corpus: "cases",
                    id: format!("{stem}@{name}"),
                    file: f.clone(),
                    define: (!define.is_empty()).then(|| define.to_string()),
                    reference_volume,
                });
            }
        }
    }
    if want("conformance") {
        let manifest: Value = serde_json::from_str(
            &std::fs::read_to_string(ctx.manifest_path()).map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let inputs: BTreeSet<String> = manifest["tests"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|t| t["group"] == "render-manifold")
            .filter_map(|t| t["input"].as_str().map(str::to_string))
            .collect();
        for i in inputs {
            out.push(Model {
                corpus: "conformance",
                id: i.trim_start_matches("tests/data/scad/").to_string(),
                file: ctx.ref_root.join(&i),
                define: None,
                reference_volume: None,
            });
        }
    }
    if want("bosl2") {
        let dir = ctx.repo.join(".reference/BOSL2/examples_x");
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .map_err(|e| {
                format!(
                    "{}: {e} (write it with `conformance bosl2-corpus`)",
                    dir.display()
                )
            })?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|x| x == "scad"))
            .collect();
        files.sort();
        for f in files.into_iter().step_by(opts.bosl2_every.max(1)) {
            out.push(Model {
                corpus: "bosl2",
                id: f.file_stem().unwrap().to_string_lossy().into_owned(),
                file: f,
                define: None,
                reference_volume: None,
            });
        }
    }
    if want("bench") {
        let cfg: Value = serde_json::from_str(
            &std::fs::read_to_string(ctx.repo.join("conformance/bench.json"))
                .map_err(|e| e.to_string())?,
        )
        .map_err(|e| e.to_string())?;
        let dir = work.join("bench");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        for (id, m) in cfg["models"].as_object().into_iter().flatten() {
            let file = if let Some(src) = m["source"].as_str() {
                let p = dir.join(format!("{id}.scad"));
                std::fs::write(&p, src).map_err(|e| e.to_string())?;
                p
            } else if let Some(f) = m["file"].as_str() {
                PathBuf::from(f.replace("{REF}", &ctx.ref_str()).replace(
                    "{BOSL2}",
                    &ctx.repo.join(".reference/BOSL2").to_string_lossy(),
                ))
            } else {
                continue;
            };
            // Inputs a model imports are generated by neoscad itself, as
            // `conformance bench` does.
            for (name, src) in m["inputs"].as_object().into_iter().flatten() {
                let target = dir.join(name);
                if !target.exists() {
                    let gen_src = dir.join(format!("{name}.scad"));
                    std::fs::write(&gen_src, src.as_str().unwrap_or(""))
                        .map_err(|e| e.to_string())?;
                    let bin = binary(ctx, opts);
                    let st = Command::new(&bin)
                        .arg("-o")
                        .arg(&target)
                        .arg(&gen_src)
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .status()
                        .map_err(|e| e.to_string())?;
                    if !st.success() {
                        return Err(format!("generating {name} failed"));
                    }
                }
            }
            out.push(Model {
                corpus: "bench",
                id: id.clone(),
                file,
                define: None,
                reference_volume: None,
            });
        }
    }
    if let Some(f) = &opts.filter {
        out.retain(|m| m.id.contains(f.as_str()));
    }
    Ok(out)
}

fn binary(ctx: &Ctx, opts: &ExactOptions) -> PathBuf {
    opts.binary.clone().unwrap_or_else(|| ctx.default_binary())
}

/// The resident size of a process in KiB (`ps`), or 0 when unknown.
fn rss_kib(pid: u32) -> u64 {
    Command::new("ps")
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()
        .and_then(|o| String::from_utf8_lossy(&o.stdout).trim().parse().ok())
        .unwrap_or(0)
}

/// Runs `cmd` to its end with its stdout captured, killing it past
/// `timeout` or above the memory guard. Returns the output, the wall
/// time in ms, and why it was killed.
fn guarded(cmd: &mut Command, timeout: Duration) -> (String, f64, Option<String>) {
    let started = Instant::now();
    let mut child = match cmd.stdout(Stdio::piped()).spawn() {
        Ok(c) => c,
        Err(e) => return (String::new(), 0.0, Some(format!("spawn: {e}"))),
    };
    let pid = child.id();
    let stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut s = String::new();
        if let Some(mut o) = stdout {
            use std::io::Read;
            let _ = o.read_to_string(&mut s);
        }
        s
    });
    let mut killed = None;
    loop {
        match child.try_wait() {
            Ok(Some(_)) | Err(_) => break,
            Ok(None) => {}
        }
        if started.elapsed() > timeout {
            killed = Some("timeout".to_string());
        } else if rss_kib(pid) > MEMORY_GUARD_KIB {
            killed = Some("memory guard (2 GB)".to_string());
        }
        if killed.is_some() {
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    let wall_ms = started.elapsed().as_secs_f64() * 1000.0;
    (reader.join().unwrap_or_default(), wall_ms, killed)
}

/// OCCT's read-back of one file, under the same guard (OCCT's reader
/// is not bounded by anything of ours).
fn occt_one(check: &Path, step: &Path) -> Option<Value> {
    let (text, _, killed) = guarded(
        Command::new(check).arg(step).stderr(Stdio::null()),
        Duration::from_secs(120),
    );
    if let Some(k) = killed {
        return Some(json!({ "valid": false, "killed": k }));
    }
    text.lines()
        .find(|l| l.starts_with('{'))
        .and_then(|l| serde_json::from_str(l).ok())
}

/// Faceted modules that are not mesh-only: a curved primitive under a
/// non-uniform scale (an ellipse STEP export cannot write yet).
fn mesh_only(modules: &[Value]) -> bool {
    modules
        .iter()
        .filter_map(Value::as_str)
        .any(|m| m != "sphere" && m != "cylinder")
}

fn run_one(ctx: &Ctx, opts: &ExactOptions, work: &Path, m: &Model) -> Outcome {
    let step = work
        .join("out")
        .join(m.corpus)
        .join(format!("{}.step", m.id.replace(['/', '@'], "_")));
    let _ = std::fs::create_dir_all(step.parent().unwrap());
    let _ = std::fs::remove_file(&step);
    let mut cmd = Command::new(binary(ctx, opts));
    cmd.arg("--enable")
        .arg("exact")
        .arg("--format")
        .arg("json")
        // The run's own limits: memory in MiB, time in seconds. The guard
        // below backs them up from outside.
        .arg("--limit")
        .arg("memory=2000")
        .arg("--limit")
        .arg(format!("time={}", opts.timeout.as_secs().max(1)));
    if let Some(d) = &m.define {
        cmd.arg("-D").arg(d);
    }
    cmd.arg("-o")
        .arg(&step)
        .arg(&m.file)
        .current_dir(m.file.parent().unwrap_or(Path::new(".")))
        .env("OPENSCADPATH", ctx.repo.join(".reference"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let (text, wall_ms, killed) = guarded(&mut cmd, opts.timeout + Duration::from_secs(10));
    if let Some(k) = killed {
        return Outcome {
            status: "killed",
            message: k,
            wall_ms,
            ..Outcome::default()
        };
    }
    let report: Option<Value> = text
        .lines()
        .rev()
        .find(|l| l.starts_with('{'))
        .and_then(|l| serde_json::from_str(l).ok());
    let Some(report) = report else {
        return Outcome {
            status: "killed",
            message: "no report (crashed?)".into(),
            wall_ms,
            ..Outcome::default()
        };
    };
    let Some(exact) = report.get("exact").cloned() else {
        // The render did not give a non-empty 3D object (or failed): the
        // exporter was never reached.
        let limit = report["diagnostics"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|d| d["code"].as_str().is_some_and(|c| c.contains("limit")));
        return Outcome {
            status: if limit { "killed" } else { "not3d" },
            message: if limit {
                "resource limit in the render".into()
            } else {
                String::new()
            },
            wall_ms,
            ..Outcome::default()
        };
    };
    let ok = exact["ok"].as_bool().unwrap_or(false);
    let faceted = exact["substitutions"]["faceted_modules"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    Outcome {
        status: if ok { "valid" } else { "failed" },
        eligible: !mesh_only(&faceted),
        message: exact["error"].as_str().unwrap_or("").to_string(),
        step: ok.then_some(step),
        exact: Some(exact),
        wall_ms,
        occt: None,
    }
}

fn num(v: &Value, k: &str) -> f64 {
    v[k].as_f64().unwrap_or(f64::NAN)
}

fn pct(n: usize, d: usize) -> String {
    if d == 0 {
        "-".into()
    } else {
        format!("{:.1}%", 100.0 * n as f64 / d as f64)
    }
}

fn median(mut v: Vec<f64>) -> f64 {
    v.retain(|x| x.is_finite());
    if v.is_empty() {
        return f64::NAN;
    }
    v.sort_by(f64::total_cmp);
    v[v.len() / 2]
}

pub fn command(ctx: &Ctx, opts: &ExactOptions) -> Result<u8, String> {
    let work = ctx.repo.join("target/conformance/exact");
    std::fs::create_dir_all(&work).map_err(|e| e.to_string())?;
    let bin = binary(ctx, opts);
    if !bin.is_file() {
        return Err(format!(
            "no binary at {} (cargo build --release)",
            bin.display()
        ));
    }
    let list = models(ctx, opts, &work)?;
    eprintln!("exact: {} models, {} jobs", list.len(), opts.jobs);
    let occt = opts
        .occt
        .clone()
        .or_else(|| std::env::var_os("MESHBREP_OCCT_CHECK").map(PathBuf::from))
        .filter(|p| p.is_file());
    let next = Mutex::new(0usize);
    let results: Mutex<Vec<Option<Outcome>>> = Mutex::new(vec![None; list.len()]);
    std::thread::scope(|s| {
        for _ in 0..opts.jobs.max(1) {
            s.spawn(|| {
                loop {
                    let i = {
                        let mut n = next.lock().expect("next");
                        let i = *n;
                        *n += 1;
                        i
                    };
                    let Some(m) = list.get(i) else { break };
                    let mut o = run_one(ctx, opts, &work, m);
                    if let (Some(check), Some(step)) = (&occt, &o.step) {
                        o.occt = occt_one(check, step);
                    }
                    if o.status == "failed" || o.status == "killed" {
                        eprintln!("  {} {}: {} {}", m.corpus, m.id, o.status, o.message);
                    }
                    results.lock().expect("results")[i] = Some(o);
                }
            });
        }
    });
    let outcomes: Vec<(Model, Outcome)> = list
        .into_iter()
        .zip(results.into_inner().expect("results"))
        .map(|(m, o)| (m, o.unwrap_or_default()))
        .collect();
    // Tabulate.
    let mut summary = serde_json::Map::new();
    println!(
        "{:12} {:>6} {:>5} {:>8} {:>16} {:>9} {:>8} {:>7} {:>6} {:>10} {:>10} {:>10} {:>10} {:>8}",
        "corpus",
        "models",
        "3D",
        "eligible",
        "valid(eligible)",
        "fallback",
        "failed",
        "killed",
        "exact%",
        "verr med",
        "verr max",
        "recon/rnd",
        "total/rnd",
        "OCCT ok"
    );
    let corpora: Vec<&str> = {
        let mut seen = Vec::new();
        for (m, _) in &outcomes {
            if !seen.contains(&m.corpus) {
                seen.push(m.corpus);
            }
        }
        seen
    };
    let mut gate3 = Vec::new();
    for c in corpora.iter().copied().chain(std::iter::once("all")) {
        let rows: Vec<&(Model, Outcome)> = outcomes
            .iter()
            .filter(|(m, _)| c == "all" || m.corpus == c)
            .collect();
        let counted: Vec<&&(Model, Outcome)> =
            rows.iter().filter(|(_, o)| o.status != "not3d").collect();
        let eligible: Vec<&&(Model, Outcome)> = counted
            .iter()
            .filter(|(_, o)| o.eligible || o.status == "killed")
            .copied()
            .collect();
        let valid_eligible = eligible.iter().filter(|(_, o)| o.status == "valid").count();
        let fallback_only = counted
            .iter()
            .filter(|(_, o)| {
                o.status == "valid"
                    && o.exact
                        .as_ref()
                        .is_some_and(|e| e["exact_faces"].as_u64() == Some(0))
            })
            .count();
        let with_fallback = counted
            .iter()
            .filter(|(_, o)| o.status == "valid" && !o.eligible)
            .count();
        let failed = counted.iter().filter(|(_, o)| o.status == "failed").count();
        let killed = counted.iter().filter(|(_, o)| o.status == "killed").count();
        let valid: Vec<&Value> = counted
            .iter()
            .filter(|(_, o)| o.status == "valid")
            .filter_map(|(_, o)| o.exact.as_ref())
            .collect();
        let (mut ef, mut ft) = (0.0, 0.0);
        for e in &valid {
            ef += num(e, "exact_faces");
            ft += num(e, "faces");
        }
        let verrs: Vec<f64> = valid.iter().map(|e| num(e, "volume_error")).collect();
        let vmax = verrs
            .iter()
            .copied()
            .filter(|x| x.is_finite())
            .fold(0.0, f64::max);
        let stage = |e: &Value, k: &str| e["timings_ms"][k].as_f64().unwrap_or(0.0);
        let ratio = |e: &Value, total: bool| {
            let r = num(e, "normal_render_ms").max(0.01);
            let mut t = stage(e, "reconstruct") + stage(e, "check") + stage(e, "write");
            if total {
                t += stage(e, "export_render");
            }
            t / r
        };
        let recon: Vec<f64> = valid.iter().map(|e| ratio(e, false)).collect();
        let total: Vec<f64> = valid.iter().map(|e| ratio(e, true)).collect();
        let occt_rows: Vec<&Value> = counted
            .iter()
            .filter_map(|(_, o)| o.occt.as_ref())
            .collect();
        let occt_ok = counted
            .iter()
            .filter(|(_, o)| {
                o.occt.as_ref().is_some_and(|r| {
                    let ours = o.exact.as_ref().map_or(f64::NAN, |e| num(e, "volume"));
                    // A model of several separate bodies is several
                    // solids, which is right; each must be closed.
                    r["valid"] == true
                        && r["solids"].as_u64().is_some_and(|n| n >= 1)
                        && r["shells"] == r["closed_shells"]
                        && r["free_edges"].as_u64() == Some(0)
                        && ((num(r, "volume") - ours).abs() <= 1e-6 * ours.abs())
                })
            })
            .count();
        println!(
            "{:12} {:>6} {:>5} {:>8} {:>16} {:>9} {:>8} {:>7} {:>6} {:>10.1e} {:>10.1e} {:>10.2} {:>10.2} {:>8}",
            c,
            rows.len(),
            counted.len(),
            eligible.len(),
            format!("{valid_eligible} ({})", pct(valid_eligible, eligible.len())),
            format!("{with_fallback}/{fallback_only}"),
            failed,
            killed,
            if ft > 0.0 {
                format!("{:.1}", 100.0 * ef / ft)
            } else {
                "-".into()
            },
            median(verrs.clone()),
            vmax,
            median(recon.clone()),
            median(total.clone()),
            if occt.is_some() {
                format!("{occt_ok}/{}", occt_rows.len())
            } else {
                "-".into()
            },
        );
        summary.insert(
            c.to_string(),
            json!({
                "models": rows.len(),
                "three_d": counted.len(),
                "eligible": eligible.len(),
                "valid_eligible": valid_eligible,
                "valid_with_fallback": with_fallback,
                "fallback_only": fallback_only,
                "failed": failed,
                "killed": killed,
                "exact_face_fraction": if ft > 0.0 { ef / ft } else { f64::NAN },
                "volume_error_median": median(verrs),
                "volume_error_max": vmax,
                "recon_over_render_median": median(recon.clone()),
                "recon_over_render_max": recon.iter().copied().fold(0.0, f64::max),
                "total_over_render_median": median(total),
                "occt_ok": occt_ok,
                "occt_read": occt_rows.len(),
            }),
        );
    }
    // Gate 3 on the cases with a closed form.
    for (m, o) in &outcomes {
        if let (Some(r), Some(e)) = (m.reference_volume, o.exact.as_ref())
            && o.status == "valid"
        {
            gate3.push(((num(e, "volume") - r).abs() / r, m.id.clone()));
        }
    }
    gate3.sort_by(|a, b| b.0.total_cmp(&a.0));
    if let Some((worst, id)) = gate3.first() {
        let over = gate3.iter().filter(|(x, _)| *x > 1e-6).count();
        println!(
            "gate 3 (closed forms): {} cases, worst {worst:.1e} ({id}), {over} over 1e-6",
            gate3.len()
        );
    }
    // Gate 5 on the benchmark models.
    for (m, o) in outcomes.iter().filter(|(m, _)| m.corpus == "bench") {
        let Some(e) = &o.exact else {
            println!("bench {:22} {} {}", m.id, o.status, o.message);
            continue;
        };
        let st = |k: &str| e["timings_ms"][k].as_f64().unwrap_or(0.0);
        println!(
            "bench {:22} {:7} render {:9.1} ms  export render {:9.1}  reconstruct {:8.1}  check {:8.1}  write {:7.1}  (recon+check+write)/render {:.2}  faceted {}",
            m.id,
            o.status,
            num(e, "normal_render_ms"),
            st("export_render"),
            st("reconstruct"),
            st("check"),
            st("write"),
            (st("reconstruct") + st("check") + st("write")) / num(e, "normal_render_ms").max(0.01),
            e["substitutions"]["faceted_modules"]
        );
    }
    let per_model: Vec<Value> = outcomes
        .iter()
        .map(|(m, o)| {
            json!({
                "corpus": m.corpus,
                "id": m.id,
                "status": o.status,
                "eligible": o.eligible,
                "message": o.message,
                "wall_ms": o.wall_ms,
                "exact": o.exact,
                "occt": o.occt,
            })
        })
        .collect();
    let failures: BTreeMap<String, usize> = outcomes
        .iter()
        .filter(|(_, o)| o.status == "failed")
        .fold(BTreeMap::new(), |mut acc, (_, o)| {
            // The failure class: the message up to its first number.
            let class: String = o
                .message
                .split(|c: char| c.is_ascii_digit())
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            *acc.entry(class).or_default() += 1;
            acc
        });
    if !failures.is_empty() {
        println!("failure classes:");
        for (k, n) in &failures {
            println!("  {n:4}  {k}");
        }
    }
    let tag = if opts.corpora.is_empty() {
        "all".to_string()
    } else {
        opts.corpora.join("-")
    };
    let path = work.join(format!("results-{tag}.json"));
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&json!({
            "summary": summary,
            "failure_classes": failures,
            "models": per_model,
        }))
        .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    eprintln!("exact: wrote {}", path.display());
    Ok(0)
}
