//! `conformance run`: execute the manifest's runnable cases against a
//! binary, compare outputs, gate on the baseline, optionally record.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use rayon::prelude::*;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::ctx::{Ctx, relpath};
use crate::geometry::{CaseEnv, GeometryEnv, Result3};
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
    /// For a compared image (tier 4): which rule it met and its score.
    #[serde(skip)]
    pub image: Option<ImageScore>,
}

/// How a tier 4 image compared with the expected one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageScore {
    /// OpenSCAD's own `image_compare` accepted it.
    pub exact: bool,
    /// The perceptual rule accepted it.
    pub perceptual: bool,
    /// Percentage of pixels with a channel 8 or more apart.
    pub percent_over: f64,
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
    /// Renders tier 3 meshes (the pinned OpenSCAD nightly).
    pub renderer: PathBuf,
    pub update_baseline: bool,
    pub record: bool,
    /// With `record`, also write the snapshot's grid.png.
    pub grid: bool,
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
    if fs::read_dir(&mcad)
        .map(|mut d| d.next().is_none())
        .unwrap_or(true)
    {
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
        .filter(|c| {
            opts.filter
                .as_ref()
                .is_none_or(|f| c.id.contains(f.as_str()))
        })
        .collect();

    let mut env = Env::new(ctx, &manifest, &binary, opts.timeout);
    if selected.iter().any(|c| c.runner == Runner::Geometry) {
        env.geometry = Some(GeometryEnv::new(ctx, &opts.renderer)?);
    }
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(opts.jobs.unwrap_or(0))
        .build()
        .map_err(|e| e.to_string())?;
    let start = Instant::now();
    // ctest runs a test after the tests it `DEPENDS` on; the only such
    // tests are the relative-output checks, which read the file their
    // `_run` test wrote. They run in a second pass.
    let (first, second): (Vec<&Case>, Vec<&Case>) = selected
        .iter()
        .partition(|c| !(c.runner == Runner::Script && crate::script::is_dependent(c)));
    let run_one = |c: &&Case| match c.runner {
        Runner::Text => env.run_text(c),
        Runner::Geometry => env.run_geometry(c),
        Runner::Script => crate::script::run(&env, c),
        Runner::Image => env.run_image(c),
        Runner::Pending => outcome(c, Status::Pending, c.pending_reason.clone()),
        Runner::Skip => outcome(c, Status::Skip, c.skip_reason.clone()),
    };
    let mut outcomes: Vec<Outcome> = pool.install(|| {
        let mut o: Vec<Outcome> = first.par_iter().map(run_one).collect();
        o.extend(second.par_iter().map(run_one).collect::<Vec<_>>());
        o
    });
    // Report in manifest order whatever the passes.
    let order: BTreeMap<&str, usize> = selected
        .iter()
        .enumerate()
        .map(|(i, c)| (c.id.as_str(), i))
        .collect();
    outcomes.sort_by_key(|o| order.get(o.id.as_str()).copied().unwrap_or(usize::MAX));
    let wall = start.elapsed();

    let mut per_tier: BTreeMap<u8, Counts> = BTreeMap::new();
    for o in &outcomes {
        per_tier.entry(o.tier).or_default().add(o.status);
    }

    if opts.verbose {
        print_failures(&outcomes);
    }
    if let Some(g) = &env.geometry {
        print_geometry_report(&manifest, &outcomes, g);
    }
    if outcomes.iter().any(|o| o.image.is_some()) {
        print_image_report("tier 4 images", &outcomes);
    }
    print_summary(&per_tier, wall, &binary);

    // Regression gate.
    let baseline_path = ctx.baseline_path();
    let baseline: Baseline = match fs::read_to_string(&baseline_path) {
        Ok(t) => {
            serde_json::from_str(&t).map_err(|e| format!("{}: {e}", baseline_path.display()))?
        }
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
        passing.extend(
            outcomes
                .iter()
                .filter(|o| o.status == Status::Pass)
                .map(|o| o.id.clone()),
        );
        let b = Baseline {
            comment: BASELINE_COMMENT.into(),
            passing: passing.into_iter().collect(),
        };
        let text = serde_json::to_string_pretty(&b).map_err(|e| e.to_string())? + "\n";
        fs::write(&baseline_path, text).map_err(|e| format!("{}: {e}", baseline_path.display()))?;
        println!(
            "baseline: {} passing ids written to {}",
            b.passing.len(),
            baseline_path.display()
        );
    }

    let report = RunReport {
        outcomes,
        per_tier,
        wall,
        binary,
    };
    if opts.record {
        let dir = crate::record::record(ctx, &manifest, &report, opts.grid)?;
        println!("recorded {}", dir.display());
    }

    if !regressions.is_empty() && !opts.update_baseline {
        eprintln!(
            "\nREGRESSION: {} baseline test(s) no longer pass:",
            regressions.len()
        );
        for (id, reason) in &regressions {
            eprintln!("  {id} ({reason})");
        }
        return Ok(1);
    }
    Ok(0)
}

/// `conformance images`: every render-mode PNG case drawn by neoscad and
/// scored as tier 4 scores images. Tier 3's direct renders (a PNG of the
/// input with `--render`, no script) are included: their expected images
/// come from the same OpenSCAD renderer, so they measure neoscad's
/// renderer on far more models than tier 4's own cases.
pub fn survey_images(
    ctx: &Ctx,
    filter: Option<&str>,
    verbose: bool,
    timeout: Duration,
    jobs: Option<usize>,
    binary: Option<PathBuf>,
    previews: bool,
) -> Result<u8, String> {
    let manifest = Manifest::load(&ctx.manifest_path())?;
    let binary = binary.unwrap_or_else(|| ctx.default_binary());
    let binary = binary
        .canonicalize()
        .map_err(|e| format!("{}: {e}", binary.display()))?;
    crate::prepare::prepare(ctx, &manifest)?;
    let direct_render = |c: &Case| {
        c.runner == Runner::Geometry
            && c.script.is_none()
            && c.suffix == "png"
            && c.args.iter().any(|a| a.starts_with("--render"))
    };
    // A pending OpenCSG preview, drawn from the rendered geometry: shows
    // how far render mode alone gets on them (cameras, colour schemes).
    let preview = |c: &Case| {
        previews
            && c.runner == Runner::Pending
            && c.tier == 4
            && c.expected.is_some()
            && c.input.is_some()
            && c.suffix == "png"
            && !c
                .args
                .iter()
                .any(|a| crate::manifest::is_view_option(a) || a.starts_with("--preview"))
    };
    let selected: Vec<&Case> = manifest
        .tests
        .iter()
        .filter(|c| c.runner == Runner::Image || direct_render(c) || preview(c))
        .filter(|c| filter.is_none_or(|f| c.id.contains(f)))
        .collect();
    let mut env = Env::new(ctx, &manifest, &binary, timeout);
    env.actual_dir = ctx.repo.join("target/conformance/images");
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(jobs.unwrap_or(0))
        .build()
        .map_err(|e| e.to_string())?;
    let outcomes: Vec<Outcome> =
        pool.install(|| selected.par_iter().map(|c| env.run_image(c)).collect());
    let pending: BTreeSet<&str> = selected
        .iter()
        .filter(|c| c.runner == Runner::Pending)
        .map(|c| c.id.as_str())
        .collect();
    for tier in [3u8, 4] {
        let of_tier: Vec<Outcome> = outcomes
            .iter()
            .filter(|o| o.tier == tier && !pending.contains(o.id.as_str()))
            .cloned()
            .collect();
        if !of_tier.is_empty() {
            print_image_report(&format!("tier {tier} render-mode images"), &of_tier);
        }
    }
    let previews: Vec<Outcome> = outcomes
        .iter()
        .filter(|o| pending.contains(o.id.as_str()))
        .cloned()
        .collect();
    if !previews.is_empty() {
        print_image_report(
            "tier 4 previews drawn from the rendered geometry (pending; not a pass)",
            &previews,
        );
    }
    let mut worst: Vec<&Outcome> = outcomes
        .iter()
        .filter(|o| o.status == Status::Fail)
        .collect();
    worst.sort_by(|a, b| {
        let key = |o: &Outcome| o.image.map_or(f64::INFINITY, |s| s.percent_over);
        key(b).total_cmp(&key(a))
    });
    let shown = if verbose {
        worst.len()
    } else {
        worst.len().min(20)
    };
    if shown > 0 {
        println!(
            "failing both rules{}:",
            if verbose { "" } else { " (worst 20)" }
        );
    }
    for o in &worst[..shown] {
        println!("  {} - {}", o.id, o.reason.as_deref().unwrap_or(""));
    }
    println!("images written to {}", env.actual_dir.display());
    Ok(0)
}

fn short(sha: &str) -> &str {
    &sha[..sha.len().min(8)]
}

pub(crate) fn outcome(c: &Case, status: Status, reason: Option<String>) -> Outcome {
    Outcome {
        id: c.id.clone(),
        tier: c.tier,
        status,
        reason,
        ms: None,
        excerpt: Vec::new(),
        image: None,
    }
}

/// Per-run constants shared by every case.
pub(crate) struct Env {
    pub ref_root: PathBuf,
    pub ref_str: String,
    pub work_dir: PathBuf,
    pub actual_dir: PathBuf,
    pub binary: PathBuf,
    /// Runtime path from the working directory to the reference `tests/`,
    /// substituted for `../../tests` in expected files.
    runtime_tests: String,
    pub timeout: Duration,
    /// Manifest-wide `OPENSCAD_TEST_EXCLUDE_LINE`.
    default_exclude: Option<String>,
    pub font_path: PathBuf,
    pub library_path: PathBuf,
    geometry: Option<GeometryEnv>,
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
            geometry: None,
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

    /// One tier 3 geometry case (see `geometry.rs`). A failure of a case
    /// listed in `conformance/tier3-limits.json` is a known artefact of the
    /// pipeline, so it is reported as skipped with the listed reason.
    fn run_geometry(&self, c: &Case) -> Outcome {
        let Some(image) = &self.geometry else {
            return outcome(c, Status::Fail, Some("no renderer configured".into()));
        };
        let env = CaseEnv {
            ref_root: &self.ref_root,
            ref_str: &self.ref_str,
            work_dir: &self.work_dir,
            actual_dir: &self.actual_dir,
            binary: &self.binary,
            timeout: self.timeout,
            font_path: &self.font_path,
            library_path: &self.library_path,
        };
        let started = Instant::now();
        let result = image.run(&env, c);
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        let mut o = match result {
            Result3::Pass => outcome(c, Status::Pass, None),
            Result3::Fail(reason) => match image.limits.get(&c.id) {
                Some(limit) => outcome(c, Status::Skip, Some(format!("harness limit: {limit}"))),
                None => outcome(c, Status::Fail, Some(reason)),
            },
        };
        o.ms = Some((ms * 10.0).round() / 10.0);
        o
    }

    /// One tier 4 image (`Runner::Image`): neoscad draws the PNG with the
    /// test's arguments, which must exit 0, and the image passes tier 4's
    /// rule: OpenSCAD's `image_compare` accepts it, or at most
    /// `PERCEPTUAL_LIMIT_PERCENT` of its pixels differ by the tolerance or
    /// more ([`image_compare::Comparison::perceptual_pass`]). The second
    /// rule exists because the image comes from neoscad's renderer, not
    /// OpenSCAD's: see crates/conformance/README.md, "Tier 4".
    pub(crate) fn run_image(&self, c: &Case) -> Outcome {
        let fail = |reason: String| outcome(c, Status::Fail, Some(reason));
        let (Some(input), Some(expected)) = (&c.input, &c.expected) else {
            return fail("manifest case lacks input or expected path".into());
        };
        let input = self.ref_root.join(input);
        let expected = self.ref_root.join(expected);
        let basename = c.id.strip_prefix(&format!("{}_", c.group)).unwrap_or(&c.id);
        let out_dir = self.actual_dir.join(&c.group);
        let actual = out_dir.join(format!("{basename}-actual.png"));
        let stderr_path = out_dir.join(format!("{basename}-actual.png.stderr"));
        let started = Instant::now();
        let run = self.spawn_and_wait(c, &input, &actual, &stderr_path, &out_dir);
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        let mut o = match run {
            Err(reason) => fail(reason),
            Ok(()) => match crate::image_compare::compare_files(&expected, &actual) {
                Err(e) => fail(e),
                Ok(cmp) => {
                    let score = ImageScore {
                        exact: cmp.passed(),
                        perceptual: cmp.perceptual_pass(),
                        percent_over: cmp.percent_pixels_over(),
                    };
                    let mut o = if score.exact || score.perceptual {
                        outcome(c, Status::Pass, None)
                    } else {
                        fail(format!(
                            "image differs: {:.3}% of pixels over tolerance; {}",
                            score.percent_over,
                            cmp.describe()
                        ))
                    };
                    o.image = Some(score);
                    o
                }
            },
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
        let mut cmd = Command::new(&self.binary);
        cmd.current_dir(&self.work_dir)
            .env("OPENSCAD_FONT_PATH", &self.font_path)
            .env_remove(crate::geometry::FONT_DIR_VAR)
            .env("OPENSCADPATH", &self.library_path);
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
        crate::geometry::exec(&mut cmd, self.timeout, stderr_path).map_err(|f| f.reason)
    }

    /// The exclusion regex for `c`; mirrors `Manifest::exclude_line`.
    fn exclude_line<'a>(&'a self, c: &'a Case) -> Option<&'a str> {
        match c.exclude_line.as_deref() {
            Some("") => None,
            Some(r) => Some(r),
            None => self.default_exclude.as_deref(),
        }
    }

    /// `compare_with_expected`: `compare_json` for `.json` outputs, the
    /// normalised text comparison (`compare_default`) otherwise.
    pub(crate) fn compare(&self, c: &Case, expected: &Path, actual: &Path) -> Outcome {
        let fail = |reason: String| outcome(c, Status::Fail, Some(reason));
        if c.suffix == "json" {
            return match compare_json(expected, actual) {
                Ok(()) => outcome(c, Status::Pass, None),
                Err(reason) => fail(reason),
            };
        }
        let exclude = match self.exclude_line(c).map(Regex::new).transpose() {
            Ok(r) => r,
            Err(e) => return fail(format!("bad exclude regex: {e}")),
        };
        let exp_text = match normalize::read_text(expected) {
            Ok(t) => t,
            Err(_) => return fail(format!("missing expected output {}", expected.display())),
        };
        let act_text = normalize::read_text(actual).unwrap_or_default();
        let exp =
            normalize::normalized_lines(&exp_text, Some(&self.runtime_tests), exclude.as_ref());
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

/// `compare_json`: both files parsed, then compared as Python compares
/// the parsed values, where `5` equals `5.0` and object key order does
/// not matter.
fn compare_json(expected: &Path, actual: &Path) -> Result<(), String> {
    let read = |p: &Path| -> Result<serde_json::Value, String> {
        let text = fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))
    };
    let exp = read(expected).map_err(|e| format!("missing expected output: {e}"))?;
    let act = read(actual)?;
    if json_eq(&exp, &act) {
        Ok(())
    } else {
        Err("json differs".into())
    }
}

fn json_eq(a: &serde_json::Value, b: &serde_json::Value) -> bool {
    use serde_json::Value as J;
    match (a, b) {
        (J::Number(x), J::Number(y)) => x.as_f64() == y.as_f64(),
        (J::Array(x), J::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| json_eq(p, q))
        }
        (J::Object(x), J::Object(y)) => {
            x.len() == y.len()
                && x.iter()
                    .all(|(k, v)| y.get(k).is_some_and(|w| json_eq(v, w)))
        }
        _ => a == b,
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

/// Tier 3 geometry results by registration group and dimension, then the
/// failure reasons (with the exit/stderr detail cut off so similar causes
/// group together).
fn print_geometry_report(manifest: &Manifest, outcomes: &[Outcome], image: &GeometryEnv) {
    let cases: BTreeMap<&str, &Case> = manifest.tests.iter().map(|c| (c.id.as_str(), c)).collect();
    let mut by_cat: BTreeMap<(String, &str), Counts> = BTreeMap::new();
    let mut reasons: BTreeMap<String, usize> = BTreeMap::new();
    let mut limit_passes = Vec::new();
    for o in outcomes {
        let Some(c) = cases.get(o.id.as_str()) else {
            continue;
        };
        if c.runner != Runner::Geometry {
            continue;
        }
        by_cat
            .entry(crate::geometry::category(c))
            .or_default()
            .add(o.status);
        if o.status == Status::Fail {
            // Group by cause: drop locations and the numbers of image diffs.
            let r = o.reason.as_deref().unwrap_or("?");
            let r = r.split(" (in file").next().unwrap_or(r);
            let r = if r.starts_with("image differs") {
                "image differs"
            } else {
                r
            };
            let r: String = r.chars().take(90).collect();
            *reasons.entry(r).or_default() += 1;
        }
        if o.status == Status::Pass && image.limits.contains_key(&o.id) {
            limit_passes.push(o.id.as_str());
        }
    }
    println!(
        "tier 3 geometry cases (renderer {}):",
        image.renderer.display()
    );
    println!(
        "  {:<36} {:>3} {:>6} {:>6} {:>6} {:>6}",
        "group", "dim", "pass", "fail", "skip", "total"
    );
    let mut dims: BTreeMap<&str, Counts> = BTreeMap::new();
    for ((g, d), c) in &by_cat {
        println!(
            "  {:<36} {:>3} {:>6} {:>6} {:>6} {:>6}",
            g, d, c.pass, c.fail, c.skip, c.total
        );
        let t = dims.entry(d).or_default();
        t.pass += c.pass;
        t.fail += c.fail;
        t.skip += c.skip;
        t.total += c.total;
    }
    for (d, c) in &dims {
        println!(
            "  {:<36} {:>3} {:>6} {:>6} {:>6} {:>6}",
            "all", d, c.pass, c.fail, c.skip, c.total
        );
    }
    let mut reasons: Vec<_> = reasons.into_iter().collect();
    reasons.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    println!("  failure reasons:");
    for (r, n) in reasons.iter().take(25) {
        println!("  {n:>5}  {r}");
    }
    if !limit_passes.is_empty() {
        println!(
            "  listed as harness limits but passed with this binary: {}",
            limit_passes.join(", ")
        );
    }
    println!();
}

/// Images by the rule they passed, and how far off the others are: the
/// distribution of the share of pixels over the tolerance.
pub(crate) fn print_image_report(title: &str, outcomes: &[Outcome]) {
    let scores: Vec<(&str, ImageScore)> = outcomes
        .iter()
        .filter_map(|o| o.image.map(|s| (o.id.as_str(), s)))
        .collect();
    let exact = scores.iter().filter(|(_, s)| s.exact).count();
    let perceptual = scores.iter().filter(|(_, s)| s.perceptual).count();
    let either = scores
        .iter()
        .filter(|(_, s)| s.exact || s.perceptual)
        .count();
    let only_perceptual = scores
        .iter()
        .filter(|(_, s)| s.perceptual && !s.exact)
        .count();
    let only_exact = scores
        .iter()
        .filter(|(_, s)| s.exact && !s.perceptual)
        .count();
    let errors = outcomes
        .iter()
        .filter(|o| o.image.is_none() && o.status == Status::Fail)
        .count();
    println!(
        "{title}: {} compared, {errors} failed before comparing",
        scores.len()
    );
    println!(
        "  image_compare {exact}, perceptual (<= {}% of pixels over tolerance) {perceptual}, either {either}",
        crate::image_compare::PERCEPTUAL_LIMIT_PERCENT
    );
    println!("  only image_compare {only_exact}, only perceptual {only_perceptual}");
    let buckets = [0.0, 0.1, 0.5, 1.0, 2.0, 5.0, 10.0, f64::INFINITY];
    let mut counts = vec![0usize; buckets.len()];
    for (_, s) in &scores {
        let i = buckets
            .iter()
            .position(|&b| s.percent_over <= b)
            .unwrap_or(buckets.len() - 1);
        counts[i] += 1;
    }
    println!("  % of pixels over tolerance:");
    let mut lo = String::from("0");
    for (b, n) in buckets.iter().zip(&counts) {
        let label = if *b == 0.0 {
            "= 0".to_string()
        } else if b.is_infinite() {
            format!("> {lo}")
        } else {
            format!("({lo}, {b}]")
        };
        println!("    {label:<12} {n:>5}");
        if !b.is_infinite() {
            lo = b.to_string();
        }
    }
    println!();
}

fn print_summary(per_tier: &BTreeMap<u8, Counts>, wall: Duration, binary: &Path) {
    println!(
        "{:<4} {:<9} {:>6} {:>6} {:>6} {:>8} {:>6}",
        "tier", "name", "pass", "fail", "skip", "pending", "total"
    );
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
    println!(
        "{:.2}s wall, binary {}",
        wall.as_secs_f64(),
        binary.display()
    );
}
