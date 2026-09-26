//! `conformance run`: execute the manifest's runnable cases against a
//! binary, compare outputs, gate on the baseline, optionally record.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rayon::prelude::*;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::ctx::{Ctx, relpath};
use crate::manifest::{Case, Manifest, Runner, TIER_NAMES};
use crate::normalize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pass,
    Fail,
    Skip,
    Pending,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Outcome {
    pub id: String,
    pub tier: u8,
    pub status: Status,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Wall time of the neoscad process, for cases that ran.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ms: Option<f64>,
    /// First differing lines, shown with --verbose; not recorded.
    #[serde(skip)]
    pub excerpt: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Counts {
    pub pass: usize,
    pub fail: usize,
    pub skip: usize,
    pub pending: usize,
    pub total: usize,
}

impl Counts {
    fn add(&mut self, s: Status) {
        self.total += 1;
        match s {
            Status::Pass => self.pass += 1,
            Status::Fail => self.fail += 1,
            Status::Skip => self.skip += 1,
            Status::Pending => self.pending += 1,
        }
    }
}

#[derive(Debug)]
pub struct RunOptions {
    pub tiers: Vec<u8>,
    pub filter: Option<String>,
    pub verbose: bool,
    pub timeout: Duration,
    pub jobs: Option<usize>,
    pub binary: Option<PathBuf>,
    pub update_baseline: bool,
    pub record: bool,
}

/// Everything a finished run produced; `record` turns it into a snapshot.
#[derive(Debug)]
pub struct RunReport {
    pub outcomes: Vec<Outcome>,
    pub per_tier: BTreeMap<u8, Counts>,
    pub wall: Duration,
    pub binary: PathBuf,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Baseline {
    #[serde(default)]
    comment: String,
    passing: Vec<String>,
}

const BASELINE_COMMENT: &str = "Test ids that passed when this file was last updated. \
`conformance run` fails if any of them stops passing. Regenerate with \
`conformance run --update-baseline`; never edit by hand to hide a regression.";

/// Returns the process exit code.
pub fn run(ctx: &Ctx, opts: &RunOptions) -> Result<i32, String> {
    if opts.record && (opts.filter.is_some() || !opts.tiers.is_empty()) {
        return Err("--record needs a full run; drop --tier/--filter".into());
    }
    let manifest = Manifest::load(&ctx.manifest_path())?;
    let ref_commit = ctx.reference_commit();
    if ref_commit != manifest.reference.commit {
        eprintln!(
            "warning: manifest was generated from reference {} but the checkout is at {}; run `conformance manifest`",
            short(&manifest.reference.commit),
            short(&ref_commit)
        );
    }
    let mcad = ctx.ref_root.join("libraries/MCAD");
    if fs::read_dir(&mcad).map(|mut d| d.next().is_none()).unwrap_or(true) {
        eprintln!(
            "warning: {} is empty, so tests that use MCAD cannot pass; fetch it with\n  git -C {} submodule update --init libraries/MCAD",
            mcad.display(),
            crate::ctx::REF_REL
        );
    }

    let binary = opts.binary.clone().unwrap_or_else(|| ctx.default_binary());
    if !binary.is_file() {
        return Err(format!(
            "{} not found; build it with `cargo build --release`",
            binary.display()
        ));
    }
    let binary = binary.canonicalize().map_err(|e| e.to_string())?;

    crate::prepare::prepare(ctx, &manifest)?;

    let selected: Vec<&Case> = manifest
        .tests
        .iter()
        .filter(|c| opts.tiers.is_empty() || opts.tiers.contains(&c.tier))
        .filter(|c| opts.filter.as_ref().is_none_or(|f| c.id.contains(f.as_str())))
        .collect();

    let env = Env::new(ctx, &manifest, &binary, opts.timeout);
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opts.jobs.unwrap_or(0))
        .build()
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    let outcomes: Vec<Outcome> = pool.install(|| {
        selected
            .par_iter()
            .map(|c| match c.runner {
                Runner::Text => env.run_text(c),
                Runner::Pending => outcome(c, Status::Pending, None),
                Runner::Skip => outcome(c, Status::Skip, c.skip_reason.clone()),
            })
            .collect()
    });
    let wall = start.elapsed();

    let mut per_tier: BTreeMap<u8, Counts> = BTreeMap::new();
    for o in &outcomes {
        per_tier.entry(o.tier).or_default().add(o.status);
    }

    if opts.verbose {
        print_failures(&outcomes);
    }
    print_summary(&per_tier, wall, &binary);

    // Regression gate.
    let baseline_path = ctx.baseline_path();
    let baseline: Baseline = match fs::read_to_string(&baseline_path) {
        Ok(t) => serde_json::from_str(&t).map_err(|e| format!("{}: {e}", baseline_path.display()))?,
        Err(_) => Baseline::default(),
    };
    let by_id: BTreeMap<&str, &Outcome> = outcomes.iter().map(|o| (o.id.as_str(), o)).collect();
    let known: BTreeSet<&str> = manifest.tests.iter().map(|c| c.id.as_str()).collect();
    let mut regressions = Vec::new();
    for id in &baseline.passing {
        if !known.contains(id.as_str()) {
            eprintln!("warning: baseline test {id} is not in the manifest");
        } else if let Some(o) = by_id.get(id.as_str())
            && o.status != Status::Pass
        {
            regressions.push((o.id.clone(), o.reason.clone().unwrap_or_else(|| "?".into())));
        }
    }

    if opts.update_baseline {
        // A partial run only updates the ids it ran, so `--tier 1
        // --update-baseline` cannot silently drop tier-0 passes.
        let mut passing: BTreeSet<String> = baseline
            .passing
            .iter()
            .filter(|id| !by_id.contains_key(id.as_str()) && known.contains(id.as_str()))
            .cloned()
            .collect();
        passing.extend(outcomes.iter().filter(|o| o.status == Status::Pass).map(|o| o.id.clone()));
        let b = Baseline { comment: BASELINE_COMMENT.into(), passing: passing.into_iter().collect() };
        let text = serde_json::to_string_pretty(&b).map_err(|e| e.to_string())? + "\n";
        fs::write(&baseline_path, text).map_err(|e| format!("{}: {e}", baseline_path.display()))?;
        println!("baseline: {} passing ids written to {}", b.passing.len(), baseline_path.display());
    }

    let report = RunReport { outcomes, per_tier, wall, binary };
    if opts.record {
        let dir = crate::record::record(ctx, &manifest, &report)?;
        println!("recorded {}", dir.display());
    }

    if !regressions.is_empty() && !opts.update_baseline {
        eprintln!("\nREGRESSION: {} baseline test(s) no longer pass:", regressions.len());
        for (id, reason) in &regressions {
            eprintln!("  {id} ({reason})");
        }
        return Ok(1);
    }
    Ok(0)
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

fn outcome(c: &Case, status: Status, reason: Option<String>) -> Outcome {
    Outcome { id: c.id.clone(), tier: c.tier, status, reason, ms: None, excerpt: Vec::new() }
}

/// Per-run constants shared by every case.
struct Env {
    ref_root: PathBuf,
    ref_str: String,
    work_dir: PathBuf,
    actual_dir: PathBuf,
    binary: PathBuf,
    /// Runtime path from the working directory to the reference `tests/`,
    /// substituted for `../../tests` in expected files.
    runtime_tests: String,
    timeout: Duration,
    /// Manifest-wide `OPENSCAD_TEST_EXCLUDE_LINE`.
    default_exclude: Option<String>,
    font_path: PathBuf,
    library_path: PathBuf,
}

impl Env {
    fn new(ctx: &Ctx, manifest: &Manifest, binary: &Path, timeout: Duration) -> Self {
        let work_dir = ctx.work_dir();
        Self {
            ref_root: ctx.ref_root.clone(),
            ref_str: ctx.ref_str(),
            runtime_tests: relpath(&ctx.ref_root.join("tests"), &work_dir),
            work_dir,
            actual_dir: ctx.actual_dir(),
            binary: binary.to_path_buf(),
            timeout,
            default_exclude: manifest.default_exclude_line.clone(),
            font_path: ctx.ref_root.join("tests/data/ttf"),
            library_path: ctx.ref_root.join("libraries"),
        }
    }

    /// One text case, following `run_test` + `compare_default` in
    /// test_cmdline_tool.py: the tool must exit 0, then the normalised
    /// output must equal the normalised expected file.
    fn run_text(&self, c: &Case) -> Outcome {
        let fail = |reason: String| outcome(c, Status::Fail, Some(reason));
        let (Some(input), Some(expected)) = (&c.input, &c.expected) else {
            return fail("manifest case lacks input or expected path".into());
        };
        let input = self.ref_root.join(input);
        let expected = self.ref_root.join(expected);
        if !input.is_file() {
            return fail(format!("missing input {}", input.display()));
        }
        // ctest names outputs `<basename>-actual.<suffix>`; the basename is
        // the id without its group prefix.
        let basename = c.id.strip_prefix(&format!("{}_", c.group)).unwrap_or(&c.id);
        let out_dir = self.actual_dir.join(&c.group);
        let actual = out_dir.join(format!("{basename}-actual.{}", c.suffix));
        let stderr_path = out_dir.join(format!("{basename}-actual.{}.stderr", c.suffix));

        let started = Instant::now();
        let run = self.spawn_and_wait(c, &input, &actual, &stderr_path, &out_dir);
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        let mut o = match run {
            Err(reason) => fail(reason),
            Ok(()) => self.compare(c, &expected, &actual),
        };
        o.ms = Some((ms * 10.0).round() / 10.0);
        o
    }

    fn spawn_and_wait(
        &self,
        c: &Case,
        input: &Path,
        actual: &Path,
        stderr_path: &Path,
        out_dir: &Path,
    ) -> Result<(), String> {
        fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
        // test_cmdline_tool.py opens the output file before running the
        // tool, so it exists (empty) even if the tool never writes it.
        let out_file = File::create(actual).map_err(|e| format!("{}: {e}", actual.display()))?;
        let err_file = File::create(stderr_path).map_err(|e| e.to_string())?;

        let mut cmd = Command::new(&self.binary);
        cmd.current_dir(&self.work_dir)
            .env("OPENSCAD_FONT_PATH", &self.font_path)
            .env("OPENSCADPATH", &self.library_path)
            .stderr(err_file);
        if c.stdio {
            let stdin = File::open(input).map_err(|e| e.to_string())?;
            cmd.arg("-").stdin(stdin).stdout(out_file);
        } else {
            cmd.arg(input).stdin(Stdio::null()).stdout(Stdio::null());
        }
        for a in &c.args {
            cmd.arg(
                a.replace("{REF}", &self.ref_str)
                    .replace("{OPENSCAD}", &self.binary.to_string_lossy()),
            );
        }
        if c.stdio {
            cmd.args(["-o", "-"]);
        } else {
            cmd.arg("-o").arg(actual);
        }

        let mut child = cmd.spawn().map_err(|e| format!("cannot run {}: {e}", self.binary.display()))?;
        let start = Instant::now();
        // Start polling finely so the recorded time of a fast case is not
        // dominated by the poll interval, then back off for slow ones.
        let mut poll = Duration::from_micros(200);
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if start.elapsed() > self.timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("timeout after {:.0}s", self.timeout.as_secs_f64()));
                }
                Ok(None) => {
                    std::thread::sleep(poll);
                    poll = (poll * 2).min(Duration::from_millis(10));
                }
                Err(e) => return Err(format!("wait failed: {e}")),
            }
        };
        if !status.success() {
            let first = File::open(stderr_path)
                .ok()
                .and_then(|f| BufReader::new(f).lines().map_while(Result::ok).find(|l| !l.trim().is_empty()))
                .unwrap_or_default();
            let code = status.code().map_or("signal".to_string(), |c| format!("exit {c}"));
            return Err(if first.is_empty() { code } else { format!("{code}: {first}") });
        }
        Ok(())
    }

    /// The exclusion regex for `c`; mirrors `Manifest::exclude_line`.
    fn exclude_line<'a>(&'a self, c: &'a Case) -> Option<&'a str> {
        match c.exclude_line.as_deref() {
            Some("") => None,
            Some(r) => Some(r),
            None => self.default_exclude.as_deref(),
        }
    }

    fn compare(&self, c: &Case, expected: &Path, actual: &Path) -> Outcome {
        let fail = |reason: String| outcome(c, Status::Fail, Some(reason));
        let exclude = match self.exclude_line(c).map(Regex::new).transpose() {
            Ok(r) => r,
            Err(e) => return fail(format!("bad exclude regex: {e}")),
        };
        let exp_text = match normalize::read_text(expected) {
            Ok(t) => t,
            Err(_) => return fail(format!("missing expected output {}", expected.display())),
        };
        let act_text = normalize::read_text(actual).unwrap_or_default();
        let exp = normalize::normalized_lines(&exp_text, Some(&self.runtime_tests), exclude.as_ref());
        let act = normalize::normalized_lines(&act_text, None, exclude.as_ref());
        match normalize::compare(&exp, &act) {
            Ok(()) => outcome(c, Status::Pass, None),
            Err(m) => {
                let mut o = fail(format!(
                    "output differs at line {} (expected {} lines, got {})",
                    m.line, m.expected_lines, m.actual_lines
                ));
                o.excerpt = m.excerpt;
                o
            }
        }
    }
}

fn print_failures(outcomes: &[Outcome]) {
    for o in outcomes.iter().filter(|o| o.status == Status::Fail) {
        println!("FAIL {} - {}", o.id, o.reason.as_deref().unwrap_or(""));
        for l in &o.excerpt {
            let l: String = l.chars().take(160).collect();
            println!("    {l}");
        }
    }
    if outcomes.iter().any(|o| o.status == Status::Fail) {
        println!();
    }
}

fn print_summary(per_tier: &BTreeMap<u8, Counts>, wall: Duration, binary: &Path) {
    println!("{:<4} {:<9} {:>6} {:>6} {:>6} {:>8} {:>6}", "tier", "name", "pass", "fail", "skip", "pending", "total");
    let mut all = Counts::default();
    for (t, c) in per_tier {
        println!(
            "{:<4} {:<9} {:>6} {:>6} {:>6} {:>8} {:>6}",
            t,
            TIER_NAMES.get(usize::from(*t)).copied().unwrap_or("?"),
            c.pass,
            c.fail,
            c.skip,
            c.pending,
            c.total
        );
        all.pass += c.pass;
        all.fail += c.fail;
        all.skip += c.skip;
        all.pending += c.pending;
        all.total += c.total;
    }
    println!(
        "{:<4} {:<9} {:>6} {:>6} {:>6} {:>8} {:>6}",
        "all", "", all.pass, all.fail, all.skip, all.pending, all.total
    );
    println!("{:.2}s wall, binary {}", wall.as_secs_f64(), binary.display());
}
