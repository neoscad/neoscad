//! `conformance diff`: differential testing against a reference OpenSCAD.
//!
//! Runs the reference binary (by default the pinned nightly) and neoscad on
//! each input with the same arguments, then compares what the chosen
//! format makes comparable:
//!
//! - the exit status (both succeed or both fail);
//! - when both succeed, the output file, byte for byte after the format's
//!   normalisation;
//! - the diagnostics on stderr that the format is responsible for. For
//!   `ast` that is the scanner and parser messages; evaluation output
//!   (`ECHO:`, evaluation warnings), which OpenSCAD also prints because it
//!   evaluates before exporting, belongs to `echo`. For `echo` every
//!   message goes into the output file (OpenSCAD's `Echostream`), so the
//!   file is compared even when both runs fail. For `csg` none: its stderr
//!   is the same parse and evaluation output that `ast` and `echo` already
//!   compare, and counting it again would file one echo difference under
//!   two formats.
//!
//! `csg` output is compared after removing `, timestamp = N`, as the
//! regression harness does (`normalize.rs`): the value is a file's
//! modification time, which is not what the comparison is about.
//!
//! Mismatches are grouped into categories so a run's result reads as a
//! short table. The full report goes to `target/conformance/diff-<format>.json`.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rayon::prelude::*;
use serde::Serialize;

use crate::ctx::Ctx;

/// The nightly OpenSCAD documented in CLAUDE.md.
pub const DEFAULT_REFERENCE: &str = "/Applications/OpenSCAD.app/Contents/MacOS/OpenSCAD";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Ast,
    Echo,
    Csg,
}

impl Format {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "ast" => Ok(Format::Ast),
            "echo" => Ok(Format::Echo),
            "csg" => Ok(Format::Csg),
            other => Err(format!(
                "unsupported --format '{other}' (supported: ast, echo, csg)"
            )),
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Format::Ast => "ast",
            Format::Echo => "echo",
            Format::Csg => "csg",
        }
    }

    /// The output as compared.
    fn normalize(self, out: Vec<u8>) -> Vec<u8> {
        match self {
            Format::Csg => {
                crate::normalize::strip_timestamps(&String::from_utf8_lossy(&out)).into_bytes()
            }
            Format::Ast | Format::Echo => out,
        }
    }

    /// Does this stderr line belong to the phase the format covers?
    fn owns_message(self, line: &str) -> bool {
        match self {
            Format::Ast => {
                const PARSE_PHASE: &[&str] = &[
                    "Parser error",
                    "Can't parse file",
                    "Can't find include file",
                    "Can't open include file",
                    "Can't open library",
                    "new lines in 'include<>'",
                    "new lines 'use<>'",
                    "Undefined escape sequence",
                    "cannot be represented precisely",
                    "Hexadecimal constant",
                    "Variable names starting with digits",
                    "was assigned on line",
                ];
                PARSE_PHASE.iter().any(|p| line.contains(p))
            }
            // Everything is in the output file.
            Format::Echo => false,
            // Covered by `ast` and `echo` (see the module docs).
            Format::Csg => false,
        }
    }
}

#[derive(Debug)]
pub struct DiffOptions {
    pub format: Format,
    pub reference: PathBuf,
    pub binary: Option<PathBuf>,
    pub paths: Vec<PathBuf>,
    pub jobs: Option<usize>,
    pub timeout: Duration,
    pub verbose: bool,
}

#[derive(Debug, Clone, Serialize)]
struct RunResult {
    /// `Some(code)` on exit, `None` on timeout or signal.
    code: Option<i32>,
    output: Option<Vec<u8>>,
    messages: Vec<String>,
    ms: f64,
}

/// One input's result: `mismatch` is `(category, detail)`.
#[derive(Debug)]
struct Outcome {
    file: String,
    mismatch: Option<(&'static str, String)>,
    ref_ms: f64,
    neo_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
struct CaseReport {
    file: String,
    category: &'static str,
    detail: String,
}

/// Every `.scad` under the given roots, sorted.
fn collect(paths: &[PathBuf]) -> Vec<PathBuf> {
    fn walk(p: &Path, out: &mut Vec<PathBuf>) {
        if p.is_dir() {
            if let Ok(rd) = fs::read_dir(p) {
                for e in rd.flatten() {
                    walk(&e.path(), out);
                }
            }
        } else if p.extension().is_some_and(|e| e == "scad") {
            out.push(p.to_path_buf());
        }
    }
    let mut out = Vec::new();
    for p in paths {
        walk(p, &mut out);
    }
    out.sort();
    out.dedup();
    out
}

fn run_one(
    bin: &Path,
    input: &Path,
    out_file: &Path,
    format: Format,
    env: &[(&str, PathBuf)],
    timeout: Duration,
) -> RunResult {
    let _ = fs::remove_file(out_file);
    let err_path = out_file.with_extension(format!("{}.stderr", format.suffix()));
    let started = Instant::now();
    let result = (|| -> Result<Option<i32>, String> {
        let err_file = File::create(&err_path).map_err(|e| e.to_string())?;
        let mut cmd = Command::new(bin);
        cmd.arg(input)
            .arg("-o")
            .arg(out_file)
            .current_dir(input.parent().unwrap_or(Path::new(".")))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(err_file);
        for (k, v) in env {
            cmd.env(k, v);
        }
        let mut child = cmd
            .spawn()
            .map_err(|e| format!("cannot run {}: {e}", bin.display()))?;
        let mut poll = Duration::from_micros(500);
        loop {
            match child.try_wait() {
                Ok(Some(s)) => return Ok(s.code()),
                Ok(None) if started.elapsed() > timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(None);
                }
                Ok(None) => {
                    std::thread::sleep(poll);
                    poll = (poll * 2).min(Duration::from_millis(20));
                }
                Err(e) => return Err(e.to_string()),
            }
        }
    })();
    let ms = started.elapsed().as_secs_f64() * 1000.0;
    let mut stderr = String::new();
    if let Ok(mut f) = File::open(&err_path) {
        let mut b = Vec::new();
        let _ = f.read_to_end(&mut b);
        stderr = String::from_utf8_lossy(&b).into_owned();
    }
    let messages = stderr
        .lines()
        .filter(|l| format.owns_message(l))
        .map(String::from)
        .collect();
    let code = result.unwrap_or(None);
    let output = if code == Some(0) || format == Format::Echo {
        fs::read(out_file).ok()
    } else {
        None
    };
    let output = output.map(|o| format.normalize(o));
    RunResult {
        code,
        output,
        messages,
        ms,
    }
}

fn first_difference(a: &[u8], b: &[u8]) -> String {
    let (a, b) = (String::from_utf8_lossy(a), String::from_utf8_lossy(b));
    let al: Vec<&str> = a.lines().collect();
    let bl: Vec<&str> = b.lines().collect();
    let i = al
        .iter()
        .zip(&bl)
        .position(|(x, y)| x != y)
        .unwrap_or(al.len().min(bl.len()));
    let clip = |s: Option<&&str>| -> String {
        s.map_or("<end>".into(), |s| s.chars().take(140).collect())
    };
    format!(
        "line {}: ref `{}` / neo `{}`",
        i + 1,
        clip(al.get(i)),
        clip(bl.get(i))
    )
}

fn classify(r: &RunResult, n: &RunResult) -> Option<(&'static str, String)> {
    let ok = |x: &RunResult| x.code == Some(0);
    match (r.code, n.code) {
        (None, _) => return Some(("reference-timeout", String::new())),
        (_, None) => return Some(("neoscad-timeout-or-crash", String::new())),
        _ => {}
    }
    if ok(r) != ok(n) {
        let first = |x: &RunResult| x.messages.first().cloned().unwrap_or_default();
        return Some((
            if ok(r) {
                "neoscad-rejects"
            } else {
                "neoscad-accepts"
            },
            format!(
                "ref exit {:?} `{}` / neo exit {:?} `{}`",
                r.code,
                first(r),
                n.code,
                first(n)
            ),
        ));
    }
    if ok(r) || (r.output.is_some() && n.output.is_some()) {
        let (a, b) = (
            r.output.as_deref().unwrap_or(&[]),
            n.output.as_deref().unwrap_or(&[]),
        );
        if a != b {
            return Some(("output-differs", first_difference(a, b)));
        }
    }
    if r.messages != n.messages {
        let i = r
            .messages
            .iter()
            .zip(&n.messages)
            .position(|(x, y)| x != y)
            .unwrap_or(r.messages.len().min(n.messages.len()));
        let get = |v: &Vec<String>| v.get(i).cloned().unwrap_or_else(|| "<none>".into());
        return Some((
            "messages-differ",
            format!("ref `{}` / neo `{}`", get(&r.messages), get(&n.messages)),
        ));
    }
    None
}

pub fn diff(ctx: &Ctx, opts: &DiffOptions) -> Result<u8, String> {
    let binary = opts.binary.clone().unwrap_or_else(|| ctx.default_binary());
    if !binary.is_file() {
        return Err(format!(
            "{} not found; build it with `cargo build --release`",
            binary.display()
        ));
    }
    if !opts.reference.is_file() {
        return Err(format!(
            "reference binary {} not found (use --binary-ref)",
            opts.reference.display()
        ));
    }
    let binary = binary.canonicalize().map_err(|e| e.to_string())?;
    let roots = if opts.paths.is_empty() {
        ["tests/data/scad", "examples", "libraries/MCAD"]
            .iter()
            .map(|p| ctx.ref_root.join(p))
            .collect()
    } else {
        opts.paths.clone()
    };
    // Inputs run with their own directory as the working directory, so
    // relative paths would no longer resolve.
    let roots: Vec<PathBuf> = roots
        .iter()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()))
        .collect();
    let files = collect(&roots);
    if files.is_empty() {
        return Err("no .scad files found".into());
    }
    let out_dir = ctx
        .repo
        .join("target/conformance/diff")
        .join(opts.format.suffix());
    let _ = fs::remove_dir_all(&out_dir);
    fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let env = [
        ("OPENSCAD_FONT_PATH", ctx.ref_root.join("tests/data/ttf")),
        ("OPENSCADPATH", ctx.ref_root.join("libraries")),
    ];

    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opts.jobs.unwrap_or(0))
        .build()
        .map_err(|e| e.to_string())?;
    let started = Instant::now();
    let rel = |p: &Path| {
        p.strip_prefix(&ctx.ref_root)
            .unwrap_or(p)
            .to_string_lossy()
            .into_owned()
    };
    let results: Vec<Outcome> = pool.install(|| {
        files
            .par_iter()
            .enumerate()
            .map(|(i, f)| {
                let r = run_one(
                    &opts.reference,
                    f,
                    &out_dir.join(format!("{i}-ref.{}", opts.format.suffix())),
                    opts.format,
                    &env,
                    opts.timeout,
                );
                let n = run_one(
                    &binary,
                    f,
                    &out_dir.join(format!("{i}-neo.{}", opts.format.suffix())),
                    opts.format,
                    &env,
                    opts.timeout,
                );
                Outcome {
                    file: rel(f),
                    mismatch: classify(&r, &n),
                    ref_ms: r.ms,
                    neo_ms: n.ms,
                }
            })
            .collect()
    });
    let wall = started.elapsed();

    let mut categories: BTreeMap<&str, Vec<&Outcome>> = BTreeMap::new();
    for r in &results {
        if let Some((c, _)) = &r.mismatch {
            categories.entry(c).or_default().push(r);
        }
    }
    let pass = results.iter().filter(|r| r.mismatch.is_none()).count();
    let total = results.len();
    println!(
        "format {}: {pass}/{total} match ({:.1}%)",
        opts.format.suffix(),
        100.0 * pass as f64 / total as f64
    );
    for (c, list) in &categories {
        println!("  {:>5}  {c}", list.len());
        let show = if opts.verbose { list.len() } else { 3 };
        for o in list.iter().take(show) {
            let detail = o.mismatch.as_ref().map(|w| w.1.as_str()).unwrap_or("");
            println!("         {}  {detail}", o.file);
        }
        if list.len() > show {
            println!("         ... {} more (-v lists all)", list.len() - show);
        }
    }
    let (ref_ms, neo_ms): (f64, f64) = results
        .iter()
        .fold((0.0, 0.0), |(a, b), r| (a + r.ref_ms, b + r.neo_ms));
    println!(
        "{:.1}s wall; process time: reference {:.1}s, neoscad {:.1}s; reference {}",
        wall.as_secs_f64(),
        ref_ms / 1000.0,
        neo_ms / 1000.0,
        opts.reference.display()
    );

    let report: Vec<CaseReport> = results
        .iter()
        .filter_map(|o| {
            o.mismatch.as_ref().map(|(c, d)| CaseReport {
                file: o.file.clone(),
                category: c,
                detail: d.clone(),
            })
        })
        .collect();
    let json_path = ctx.repo.join(format!(
        "target/conformance/diff-{}.json",
        opts.format.suffix()
    ));
    let json = serde_json::json!({ "format": opts.format.suffix(), "total": total, "pass": pass, "mismatches": report });
    fs::write(
        &json_path,
        serde_json::to_string_pretty(&json).map_err(|e| e.to_string())? + "\n",
    )
    .map_err(|e| format!("{}: {e}", json_path.display()))?;
    println!("report: {}", json_path.display());
    Ok(u8::from(pass != total))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn res(code: Option<i32>, out: &str, msgs: &[&str]) -> RunResult {
        RunResult {
            code,
            output: (code == Some(0)).then(|| out.as_bytes().to_vec()),
            messages: msgs.iter().map(|s| s.to_string()).collect(),
            ms: 0.0,
        }
    }

    #[test]
    fn classifies_mismatches() {
        assert_eq!(
            classify(&res(Some(0), "a", &[]), &res(Some(0), "a", &[])),
            None
        );
        assert_eq!(
            classify(&res(Some(0), "a", &[]), &res(Some(0), "b", &[]))
                .unwrap()
                .0,
            "output-differs"
        );
        assert_eq!(
            classify(&res(Some(1), "", &["x"]), &res(Some(0), "a", &[]))
                .unwrap()
                .0,
            "neoscad-accepts"
        );
        assert_eq!(
            classify(&res(Some(0), "a", &[]), &res(Some(1), "", &[]))
                .unwrap()
                .0,
            "neoscad-rejects"
        );
        assert_eq!(
            classify(&res(Some(1), "", &["x"]), &res(Some(1), "", &["y"]))
                .unwrap()
                .0,
            "messages-differ"
        );
        assert_eq!(
            classify(&res(None, "", &[]), &res(Some(0), "", &[]))
                .unwrap()
                .0,
            "reference-timeout"
        );
    }

    #[test]
    fn ast_owns_parse_messages_only() {
        assert!(Format::Ast.owns_message("ERROR: Parser error: syntax error in file x, line 1"));
        assert!(!Format::Ast.owns_message("ECHO: 1"));
        assert!(
            !Format::Ast.owns_message("WARNING: Ignoring unknown module 'x' in file a, line 1")
        );
    }
}
