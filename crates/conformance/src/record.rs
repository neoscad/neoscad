//! `conformance run --record`: write a progress snapshot, as specified in
//! docs/architecture.md ("Progress recording").
//!
//! `progress/<UTC timestamp>-<short sha>[-dirty]/` gets `meta.json` and a
//! compact `scoreboard.json` (plus `grid.png` with `--grid`), and one line is
//! appended to `progress/index.jsonl`. The directory is gitignored.
//!
//! Snapshots are meant to be taken often and kept forever, so they store
//! only small data: one status character per manifest test instead of a
//! per-test object, and failure reasons counted rather than repeated. Images
//! are derived from that data on demand by `conformance grid`.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::ctx::{Ctx, git};
use crate::grid;
use crate::manifest::{Manifest, TIER_NAMES};
use crate::run::{RunReport, Status};
use crate::sha256;

/// Scoreboard format version. Version 1 (implicit, no `schema` key) held a
/// pretty-printed object per test and is not read any more.
pub const SCHEMA: u32 = 2;

/// A failure reason's test ids are listed only when at most this many tests
/// share it: a handful of ids is useful when triaging, but the thousands
/// behind "X is not implemented yet" would be the bulk of the file.
const FEW_IDS: usize = 10;

/// The contents of `scoreboard.json`, written minified.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Scoreboard {
    pub schema: u32,
    pub timestamp: String,
    pub sha: String,
    pub dirty: bool,
    /// SHA-256 of `conformance/manifest.json` as run. `None` only when it
    /// is unknown, and then `embedded` is always present.
    pub manifest_sha256: Option<String>,
    /// One character per manifest test, in manifest order; see
    /// [`status_char`].
    pub status: String,
    pub tiers: BTreeMap<String, TierSummary>,
    pub wall_seconds: f64,
    /// Summed wall time of every neoscad process, in milliseconds.
    pub process_ms_total: f64,
    /// Failure reason to the number of tests that failed with it.
    pub failures: BTreeMap<String, usize>,
    /// Ids behind each reason shared by at most [`FEW_IDS`] tests.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub failure_ids: BTreeMap<String, Vec<String>>,
    /// The test list, present only when the manifest as run cannot be
    /// recovered from git later (it differed from HEAD's).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedded: Option<EmbeddedTests>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TierSummary {
    pub name: String,
    pub pass: usize,
    pub fail: usize,
    pub skip: usize,
    pub pending: usize,
    pub total: usize,
    /// Summed process time of this tier's cases, in milliseconds.
    pub ms: f64,
}

/// A snapshot's own copy of the manifest order: ids, and each test's tier
/// as one digit, both parallel to `Scoreboard::status`. The grid needs the
/// tiers; the ids make the status string mean something to a reader.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddedTests {
    pub ids: Vec<String>,
    pub tiers: String,
}

pub fn status_char(s: Status) -> char {
    match s {
        Status::Pass => 'P',
        Status::Fail => 'F',
        Status::Skip => 'S',
        Status::Pending => '-',
    }
}

pub fn parse_status(c: char) -> Option<Status> {
    match c {
        'P' => Some(Status::Pass),
        'F' => Some(Status::Fail),
        'S' => Some(Status::Skip),
        '-' => Some(Status::Pending),
        _ => None,
    }
}

impl Scoreboard {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let v: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        if v.get("schema").and_then(serde_json::Value::as_u64) != Some(u64::from(SCHEMA)) {
            return Err(format!(
                "{}: not a schema-{SCHEMA} scoreboard",
                path.display()
            ));
        }
        serde_json::from_value(v).map_err(|e| format!("{}: {e}", path.display()))
    }

    pub fn statuses(&self) -> Result<Vec<Status>, String> {
        self.status
            .chars()
            .map(|c| parse_status(c).ok_or_else(|| format!("unknown status character {c:?}")))
            .collect()
    }
}

/// `git show <rev>:conformance/manifest.json` as raw bytes (the hash is
/// over bytes, so the lossy, trimmed text of `ctx::git` would not do).
pub fn manifest_at(ctx: &Ctx, rev: &str) -> Option<Vec<u8>> {
    let out = Command::new("git")
        .arg("-C")
        .arg(&ctx.repo)
        .args(["show", &format!("{rev}:conformance/manifest.json")])
        .output()
        .ok()?;
    out.status.success().then_some(out.stdout)
}

pub fn record(
    ctx: &Ctx,
    manifest: &Manifest,
    report: &RunReport,
    with_grid: bool,
) -> Result<PathBuf, String> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();
    let (compact, iso) = utc_timestamps(now);

    let sha = git(&ctx.repo, &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let short =
        git(&ctx.repo, &["rev-parse", "--short=7", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let branch = git(&ctx.repo, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap_or_default();
    let subject = git(&ctx.repo, &["log", "-1", "--format=%s"]).unwrap_or_default();
    let commit_time = git(&ctx.repo, &["log", "-1", "--format=%cI"]).unwrap_or_default();
    // Untracked files count: a new, not-yet-added source file changes what
    // was measured just as much as an edit does.
    let dirty = git(&ctx.repo, &["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    let neoscad_version = Command::new(&report.binary)
        .arg("--version")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();

    let base = format!("{compact}-{short}{}", if dirty { "-dirty" } else { "" });
    let progress = ctx.progress_dir();
    let mut dir = progress.join(&base);
    let mut n = 2;
    while dir.exists() {
        dir = progress.join(format!("{base}-{n}"));
        n += 1;
    }
    fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let dir_name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let meta = json!({
        "sha": sha,
        "short_sha": short,
        "branch": branch,
        "subject": subject,
        "commit_time": commit_time,
        "timestamp": iso,
        "dirty": dirty,
        "reference_commit": ctx.reference_commit(),
        "manifest_reference_commit": manifest.reference.commit,
        "neoscad_version": neoscad_version,
        "binary": report.binary.to_string_lossy(),
    });
    let text = serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())? + "\n";
    write_file(&dir.join("meta.json"), &text)?;

    // Hash the manifest file as it is now. The run parsed it moments ago, so
    // this is the manifest as run unless it was edited mid-run; comparing the
    // parsed test lists catches that, and then the hash is withheld rather
    // than let a later `conformance grid` map statuses onto the wrong tests.
    let bytes = fs::read(ctx.manifest_path())
        .map_err(|e| format!("{}: {e}", ctx.manifest_path().display()))?;
    let same_as_run =
        serde_json::from_slice::<Manifest>(&bytes).is_ok_and(|m| same_tests(&m, manifest));
    let hash = same_as_run.then(|| sha256::hex(&bytes));
    // `conformance grid` finds the manifest again with `git show <sha>:...`.
    // When the run used a manifest HEAD does not have (an uncommitted
    // regeneration), that lookup would fail forever, so the test list is
    // embedded instead. This is the only case that makes a snapshot large.
    let recoverable = hash.is_some() && manifest_at(ctx, "HEAD").map(|b| sha256::hex(&b)) == hash;

    let scoreboard = build_scoreboard(manifest, report, &iso, &sha, dirty, hash, !recoverable);
    let text = serde_json::to_string(&scoreboard).map_err(|e| e.to_string())? + "\n";
    write_file(&dir.join("scoreboard.json"), &text)?;

    if with_grid {
        let cells: Vec<(u8, Status)> = manifest
            .tests
            .iter()
            .map(|c| c.tier)
            .zip(scoreboard.statuses()?)
            .collect();
        grid::write_png(
            &dir.join("grid.png"),
            &grid::title(&iso, &short, dirty),
            &subject,
            &cells,
        )?;
    }

    // The index keeps the shape of its earliest lines (per-tier counts only,
    // no timings) so the timeline stays readable by one parser.
    let tiers: BTreeMap<&String, serde_json::Value> = scoreboard
        .tiers
        .iter()
        .map(|(t, c)| (t, json!({"name": c.name, "pass": c.pass, "fail": c.fail, "skip": c.skip, "pending": c.pending, "total": c.total})))
        .collect();
    let line = json!({
        "dir": dir_name,
        "timestamp": iso,
        "sha": sha,
        "dirty": dirty,
        "subject": subject,
        "tiers": tiers,
    });
    let mut index = OpenOptions::new()
        .create(true)
        .append(true)
        .open(progress.join("index.jsonl"))
        .map_err(|e| e.to_string())?;
    writeln!(index, "{line}").map_err(|e| e.to_string())?;
    Ok(dir)
}

fn same_tests(a: &Manifest, b: &Manifest) -> bool {
    a.tests.len() == b.tests.len()
        && a.tests
            .iter()
            .zip(&b.tests)
            .all(|(x, y)| x.id == y.id && x.tier == y.tier)
}

/// The compact scoreboard for a run, with statuses in manifest order. A test
/// the run did not report (impossible for a full run, which `--record`
/// requires) is recorded as pending.
pub fn build_scoreboard(
    manifest: &Manifest,
    report: &RunReport,
    iso: &str,
    sha: &str,
    dirty: bool,
    manifest_sha256: Option<String>,
    embed: bool,
) -> Scoreboard {
    let by_id: BTreeMap<&str, &crate::run::Outcome> =
        report.outcomes.iter().map(|o| (o.id.as_str(), o)).collect();
    let status: String = manifest
        .tests
        .iter()
        .map(|c| {
            status_char(
                by_id
                    .get(c.id.as_str())
                    .map_or(Status::Pending, |o| o.status),
            )
        })
        .collect();

    let mut tier_ms: BTreeMap<u8, f64> = BTreeMap::new();
    for o in &report.outcomes {
        *tier_ms.entry(o.tier).or_default() += o.ms.unwrap_or(0.0);
    }
    let tiers = report
        .per_tier
        .iter()
        .map(|(t, c)| {
            let name = TIER_NAMES
                .get(usize::from(*t))
                .copied()
                .unwrap_or("?")
                .to_string();
            let ms = round1(tier_ms.get(t).copied().unwrap_or(0.0));
            (
                t.to_string(),
                TierSummary {
                    name,
                    pass: c.pass,
                    fail: c.fail,
                    skip: c.skip,
                    pending: c.pending,
                    total: c.total,
                    ms,
                },
            )
        })
        .collect();

    // Walk in manifest order so each reason's id list is in manifest order.
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for c in &manifest.tests {
        if let Some(o) = by_id
            .get(c.id.as_str())
            .filter(|o| o.status == Status::Fail)
        {
            let reason = o.reason.clone().unwrap_or_else(|| "?".into());
            groups.entry(reason).or_default().push(o.id.clone());
        }
    }
    let failures = groups
        .iter()
        .map(|(r, ids)| (r.clone(), ids.len()))
        .collect();
    let failure_ids = groups
        .into_iter()
        .filter(|(_, ids)| ids.len() <= FEW_IDS)
        .collect();

    let embedded = embed.then(|| EmbeddedTests {
        ids: manifest.tests.iter().map(|c| c.id.clone()).collect(),
        tiers: manifest.tests.iter().map(|c| tier_char(c.tier)).collect(),
    });

    Scoreboard {
        schema: SCHEMA,
        timestamp: iso.to_string(),
        sha: sha.to_string(),
        dirty,
        manifest_sha256,
        status,
        tiers,
        wall_seconds: (report.wall.as_secs_f64() * 1000.0).round() / 1000.0,
        process_ms_total: report
            .outcomes
            .iter()
            .filter_map(|o| o.ms)
            .sum::<f64>()
            .round(),
        failures,
        failure_ids,
        embedded,
    }
}

/// Tiers are single digits (0-5), so one character each suffices.
fn tier_char(t: u8) -> char {
    char::from_digit(u32::from(t), 10).unwrap_or('?')
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

fn write_file(path: &Path, text: &str) -> Result<(), String> {
    fs::write(path, text).map_err(|e| format!("{}: {e}", path.display()))
}

/// (`20260925T184210Z`, `2026-09-25T18:42:10Z`) for a Unix time.
fn utc_timestamps(secs: u64) -> (String, String) {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let (h, mi, s) = (rem / 3600, rem % 3600 / 60, rem % 60);
    let (y, mo, d) = civil_from_days(days);
    (
        format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z"),
        format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}Z"),
    )
}

/// Days since 1970-01-01 to a proleptic Gregorian date (H. Hinnant's
/// `civil_from_days`), to avoid a date-time dependency for one format.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = yoe + era * 400 + i64::from(m <= 2);
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps() {
        // 2026-09-25T18:42:10Z
        assert_eq!(utc_timestamps(1_790_361_730).0, "20260925T184210Z");
        assert_eq!(utc_timestamps(0).1, "1970-01-01T00:00:00Z");
        assert_eq!(utc_timestamps(951_782_400).1, "2000-02-29T00:00:00Z");
    }
}
