//! `conformance run --record`: write a progress snapshot, as specified in
//! docs/architecture.md ("Progress recording").
//!
//! `progress/<UTC timestamp>-<short sha>[-dirty]/` gets `meta.json`,
//! `scoreboard.json` and `grid.png`, and one line is appended to
//! `progress/index.jsonl`. The directory is gitignored.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

use crate::ctx::{Ctx, git};
use crate::grid;
use crate::manifest::{Manifest, TIER_NAMES};
use crate::run::{RunReport, Status};

pub fn record(ctx: &Ctx, manifest: &Manifest, report: &RunReport) -> Result<PathBuf, String> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|e| e.to_string())?.as_secs();
    let (compact, iso) = utc_timestamps(now);

    let sha = git(&ctx.repo, &["rev-parse", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let short = git(&ctx.repo, &["rev-parse", "--short=7", "HEAD"]).unwrap_or_else(|| "unknown".into());
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
    let dir_name = dir.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();

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
    write_json(&dir.join("meta.json"), &meta)?;

    let tiers: BTreeMap<String, serde_json::Value> = report
        .per_tier
        .iter()
        .map(|(t, c)| {
            let name = TIER_NAMES.get(usize::from(*t)).copied().unwrap_or("?");
            (t.to_string(), json!({"name": name, "pass": c.pass, "fail": c.fail, "skip": c.skip, "pending": c.pending, "total": c.total}))
        })
        .collect();
    let process_ms: f64 = report.outcomes.iter().filter_map(|o| o.ms).sum();
    let mut slowest: Vec<_> = report.outcomes.iter().filter(|o| o.ms.is_some()).collect();
    slowest.sort_by(|a, b| b.ms.partial_cmp(&a.ms).unwrap_or(std::cmp::Ordering::Equal));
    let scoreboard = json!({
        "timestamp": iso,
        "sha": sha,
        "dirty": dirty,
        "tiers": tiers,
        "timing": {
            "wall_seconds": (report.wall.as_secs_f64() * 1000.0).round() / 1000.0,
            "process_ms_total": process_ms.round(),
            "slowest": slowest.iter().take(10).map(|o| json!({"id": o.id, "ms": o.ms})).collect::<Vec<_>>(),
        },
        "tests": report.outcomes,
    });
    write_json(&dir.join("scoreboard.json"), &scoreboard)?;

    // Grid in manifest order, so a test keeps its cell from frame to frame.
    let status: BTreeMap<&str, Status> = report.outcomes.iter().map(|o| (o.id.as_str(), o.status)).collect();
    let cells: Vec<(u8, Status)> = manifest
        .tests
        .iter()
        .map(|c| (c.tier, status.get(c.id.as_str()).copied().unwrap_or(Status::Pending)))
        .collect();
    let title = format!("NEOSCAD CONFORMANCE  {iso}  {short}{}", if dirty { "-DIRTY" } else { "" });
    grid::write_png(&dir.join("grid.png"), &title, &subject, &cells)?;

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

fn write_json(path: &std::path::Path, v: &serde_json::Value) -> Result<(), String> {
    let text = serde_json::to_string_pretty(v).map_err(|e| e.to_string())? + "\n";
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
