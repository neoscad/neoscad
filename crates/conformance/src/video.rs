//! `conformance video`: the project's progress as an H.264 video, drawn
//! entirely from what `progress/` records.
//!
//! One scene per snapshot in `progress/index.jsonl`, in order: the test grid
//! (`grid.rs`, regenerated from the compact scoreboard plus the manifest at
//! that commit), a caption with the time, short sha and commit subject, and
//! a side panel with per-tier pass counts and a line chart of total passes
//! over time. Moving from one snapshot to the next, cell colours, counters
//! and the chart's head are interpolated, so gains read as motion rather
//! than a cut. Benchmark runs (`progress/bench`) appear as interludes
//! after the snapshot they belong to, and so does the agent-loop eval
//! (`progress/agent-eval`) when `--agent-eval` asks for it; a title card
//! opens and an end card with the final totals closes.
//!
//! The agent-eval interlude is off by default because its table (pass
//! counts, tool calls, cost and time per condition) is a run result, and
//! the project does not publish agent-eval results: a video made with the
//! default options is safe to post.
//!
//! Frames are 1920x1080 PNGs written in parallel, then encoded by ffmpeg.
//! Every frame is a pure function of the recorded data and the frame's
//! place in the timeline, so the frames are byte-identical at any thread
//! count and the video is identical for the same inputs and ffmpeg.

use std::fs;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::process::Command;

use rayon::prelude::*;
use serde_json::Value;

use crate::bench_chart;
use crate::ctx::{Ctx, git};
use crate::grid::{self, Canvas, DIM, HEIGHT, MARGIN, Rgb, TEXT, WIDTH};
use crate::manifest::TIER_NAMES;
use crate::record::Scoreboard;
use crate::run::Status;

const TIERS: usize = TIER_NAMES.len();
const GREEN: Rgb = [46, 204, 113];
const RED: Rgb = [231, 76, 60];
const AMBER: Rgb = [241, 196, 15];
const TRACK: Rgb = [48, 48, 56];
const FILL: Rgb = [24, 64, 44];

/// Seconds a snapshot-to-snapshot transition takes.
const TRANSITION_S: f64 = 0.75;
/// Seconds a cross-fade between unrelated scenes takes.
const FADE_S: f64 = 0.5;

/// Layout of a snapshot scene: grid on the left, panel on the right.
const CONTENT_Y: usize = 134;
const PANEL_X: usize = 1430;
const PANEL_W: usize = WIDTH - MARGIN - PANEL_X;
const GRID_W: usize = PANEL_X - 40 - MARGIN;

#[derive(Debug, Clone)]
pub struct VideoOptions {
    /// Output file (default: `<progress>/video/progress.mp4`).
    pub out: Option<PathBuf>,
    pub fps: u32,
    /// Seconds each snapshot is held once its transition ends.
    pub hold: f64,
    /// Keep the PNG frames here instead of a temporary directory.
    pub frames_dir: Option<PathBuf>,
    /// The recorded data (default: the repository's `progress/`).
    pub progress: Option<PathBuf>,
    pub ffmpeg: PathBuf,
    /// Include the agent-eval interlude. Off by default: it shows eval
    /// results, which are not published (see the module comment).
    pub agent_eval: bool,
    /// A `git filter-repo` commit map (`old new` per line). Default: the
    /// repository's `.git/filter-repo/commit-map` when there is one.
    pub commit_map: Option<PathBuf>,
}

/// Recorded commit ids translated to rewritten history.
///
/// `progress/` records the commit each run was made at. When the history
/// is rewritten (the repository went through `git filter-repo` before it
/// was published), those commits no longer exist: the manifest a snapshot
/// needs cannot be read at its commit, so the video fails, and the
/// captions would show ids nobody can look up. filter-repo leaves a map of
/// old to new ids; with it, every recorded id is read as its new commit.
/// A commit filter-repo pruned (mapped to all zeros) keeps its old id.
#[derive(Debug, Default)]
struct CommitMap(std::collections::HashMap<String, String>);

impl CommitMap {
    fn parse(text: &str) -> CommitMap {
        let pairs = text.lines().filter_map(|line| {
            let mut words = line.split_whitespace();
            let (old, new) = (words.next()?, words.next()?);
            let hex = |s: &str| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit());
            (hex(old) && hex(new) && new.bytes().any(|b| b != b'0'))
                .then(|| (old.to_string(), new.to_string()))
        });
        CommitMap(pairs.collect())
    }

    fn load(ctx: &Ctx, path: Option<&Path>) -> Result<CommitMap, String> {
        let path = match path {
            Some(p) => p.to_path_buf(),
            None => {
                let Some(dir) = git(&ctx.repo, &["rev-parse", "--git-common-dir"]) else {
                    return Ok(CommitMap::default());
                };
                let p = ctx.repo.join(dir).join("filter-repo/commit-map");
                if !p.is_file() {
                    return Ok(CommitMap::default());
                }
                p
            }
        };
        let text = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(CommitMap::parse(&text))
    }

    /// The rewritten id of a full recorded id, if it was rewritten.
    fn get(&self, sha: &str) -> Option<&str> {
        self.0.get(sha).map(String::as_str)
    }

    /// The rewritten commit's subject. Subjects can name other commits,
    /// and filter-repo rewrote those ids in the messages too, so the
    /// recorded subject may cite an id that no longer exists.
    fn subject(ctx: &Ctx, new: &str) -> Option<String> {
        git(&ctx.repo, &["log", "-1", "--format=%s", new])
    }

    /// Point a bench or eval record's `sha`, `short_sha` and `subject` at
    /// the rewritten commit.
    fn rewrite_doc(&self, ctx: &Ctx, doc: &mut Value) {
        if let Some(new) = doc["sha"].as_str().and_then(|s| self.get(s)) {
            let new = new.to_string();
            if doc.get("subject").is_some()
                && let Some(subject) = Self::subject(ctx, &new)
            {
                doc["subject"] = Value::String(subject);
            }
            doc["short_sha"] = Value::String(new[..7].to_string());
            doc["sha"] = Value::String(new);
        }
    }
}

/// A progress snapshot, reduced to what its frames need.
#[derive(Debug, Clone)]
struct Snap {
    /// Unix seconds.
    time: i64,
    /// "2026-09-26 05:09 UTC".
    when: String,
    /// Short sha, with `-DIRTY` for a run on uncommitted changes.
    label: String,
    sha: String,
    subject: String,
    cells: Vec<(u8, Status)>,
}

impl Snap {
    /// Per tier: passes, and tests that are not skipped. Skipped tests are
    /// ones the suite never runs here (experimental, missing inputs), so
    /// "40 / 40" reads as "all of what can pass does".
    fn tier_counts(&self) -> [(usize, usize); TIERS] {
        let mut c = [(0, 0); TIERS];
        for &(t, s) in &self.cells {
            if let Some(e) = c.get_mut(usize::from(t)) {
                e.0 += usize::from(s == Status::Pass);
                e.1 += usize::from(s != Status::Skip);
            }
        }
        c
    }

    fn passes(&self) -> usize {
        self.cells.iter().filter(|c| c.1 == Status::Pass).count()
    }
}

/// A benchmark result, shown after snapshot `at`.
#[derive(Debug, Clone)]
struct Bench {
    doc: Value,
    at: usize,
}

/// Everything the video is drawn from.
#[derive(Debug, Clone)]
struct Data {
    snaps: Vec<Snap>,
    benches: Vec<Bench>,
    /// The agent-loop eval record and the snapshot it follows.
    eval: Option<(Value, usize)>,
    /// Commits up to the last snapshot's, when git can say.
    commits: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Scene {
    Title,
    /// Snapshot `i`, `f` of the way through the transition from `i - 1`
    /// (1 = settled).
    Snap(usize, f64),
    Bench(usize),
    Eval,
    End,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Frame {
    Still(Scene),
    /// A cross-fade `f` of the way from one scene to the other.
    Fade(Scene, Scene, f64),
}

pub fn command(ctx: &Ctx, opts: &VideoOptions) -> Result<u8, String> {
    if opts.fps == 0 || opts.fps > 120 {
        return Err("--fps must be 1 to 120".into());
    }
    if !(opts.hold.is_finite() && opts.hold > 0.0) {
        return Err("--hold must be positive".into());
    }
    let progress = opts.progress.clone().unwrap_or_else(|| ctx.progress_dir());
    let map = CommitMap::load(ctx, opts.commit_map.as_deref())?;
    let data = load(ctx, &progress, opts.agent_eval, &map)?;
    let plan = plan(&data, opts.fps, opts.hold);
    let frames: usize = plan.iter().map(|p| p.1).sum();

    let out = opts
        .out
        .clone()
        .unwrap_or_else(|| progress.join("video/progress.mp4"));
    if let Some(parent) = out.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    let (dir, temporary) = match &opts.frames_dir {
        Some(d) => (d.clone(), false),
        None => (
            std::env::temp_dir().join(format!("neoscad-video-{}", std::process::id())),
            true,
        ),
    };
    prepare_frames_dir(&dir)?;
    println!(
        "{} snapshots, {} benchmark and {} agent-eval interludes: {frames} frames at {} fps",
        data.snaps.len(),
        data.benches.len(),
        usize::from(data.eval.is_some()),
        opts.fps
    );
    write_frames(&data, &plan, &dir)?;
    let encoded = encode(&opts.ffmpeg, &dir, opts.fps, &out);
    if temporary {
        // Only our own frame files are in a directory we created.
        let _ = fs::remove_dir_all(&dir);
    }
    encoded?;
    let size = fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    println!(
        "wrote {} ({:.1} s, {:.1} MB){}",
        out.display(),
        frames as f64 / f64::from(opts.fps),
        size as f64 / 1e6,
        if temporary {
            String::new()
        } else {
            format!("; frames in {}", dir.display())
        }
    );
    Ok(0)
}

/// Create the frames directory and delete any `frame_*.png` from an
/// earlier, possibly longer, render: ffmpeg reads the numbered sequence
/// until it breaks, so a stale tail would be appended to the video.
fn prepare_frames_dir(dir: &Path) -> Result<(), String> {
    fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let entries = fs::read_dir(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    for e in entries.flatten() {
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("frame_") && name.ends_with(".png") {
            fs::remove_file(e.path()).map_err(|err| format!("{name}: {err}"))?;
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Loading

fn read_json(path: &Path) -> Result<Value, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// `*.json` files directly in `dir`, sorted by name (which is by time).
fn json_files(dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_file() && p.extension().is_some_and(|x| x == "json"))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    files
}

fn load(ctx: &Ctx, progress: &Path, agent_eval: bool, map: &CommitMap) -> Result<Data, String> {
    let index = progress.join("index.jsonl");
    let text = fs::read_to_string(&index).map_err(|e| format!("{}: {e}", index.display()))?;
    let mut snaps = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let v: Value =
            serde_json::from_str(line).map_err(|e| format!("{}: {e}", index.display()))?;
        let Some(name) = v["dir"].as_str() else {
            continue;
        };
        let dir = progress.join(name);
        let mut sb = Scoreboard::load(&dir.join("scoreboard.json"))?;
        let meta = read_json(&dir.join("meta.json"))?;
        let rewritten = map.get(&sb.sha).map(str::to_string);
        if let Some(new) = &rewritten {
            sb.sha = new.clone();
        }
        let cells = grid::cells_for(ctx, &sb).map_err(|e| format!("{}: {e}", dir.display()))?;
        let recorded_subject = meta["subject"].as_str().unwrap_or_default().to_string();
        let (short, subject) = match &rewritten {
            Some(new) => (
                new[..7].to_string(),
                CommitMap::subject(ctx, new).unwrap_or(recorded_subject),
            ),
            None => (
                meta["short_sha"]
                    .as_str()
                    .map_or_else(|| sb.sha.chars().take(7).collect(), str::to_string),
                recorded_subject,
            ),
        };
        let time = parse_time(&sb.timestamp)
            .ok_or_else(|| format!("{}: bad timestamp {:?}", dir.display(), sb.timestamp))?;
        snaps.push(Snap {
            time,
            when: fmt_when(time),
            label: format!("{}{}", short, if sb.dirty { "-DIRTY" } else { "" }),
            sha: sb.sha.clone(),
            subject,
            cells,
        });
    }
    if snaps.is_empty() {
        return Err(format!("no snapshots in {}", index.display()));
    }

    let mut benches: Vec<Bench> = Vec::new();
    for path in json_files(&progress.join("bench")) {
        let mut doc = read_json(&path)?;
        map.rewrite_doc(ctx, &mut doc);
        // A run that timed no models (an edit-loop-only run, say) draws an
        // empty chart and has no geometric mean: nothing to show.
        let has_geomean = doc["geomean_speedup"]
            .as_object()
            .is_some_and(|g| g.values().any(|r| r["value"].is_number()));
        if !has_geomean {
            continue;
        }
        let time = doc["timestamp"].as_str().and_then(parse_time).unwrap_or(0);
        let at = nearest(&snaps, doc["sha"].as_str().unwrap_or(""), time);
        benches.push(Bench { doc, at });
    }
    // A quick run is superseded by a full one at the same snapshot: showing
    // both repeats the same models with fewer references.
    let full_at: Vec<usize> = benches
        .iter()
        .filter(|b| b.doc["quick"].as_bool() != Some(true))
        .map(|b| b.at)
        .collect();
    benches.retain(|b| b.doc["quick"].as_bool() != Some(true) || !full_at.contains(&b.at));

    // The latest eval that ran both conditions; a single-condition rerun
    // has nothing to compare. Not even read unless asked for, so a default
    // video cannot pick up a result.
    let eval_files = if agent_eval {
        json_files(&progress.join("agent-eval"))
    } else {
        Vec::new()
    };
    let eval = eval_files
        .into_iter()
        .rev()
        .find_map(|path| {
            let mut doc = read_json(&path).ok()?;
            map.rewrite_doc(ctx, &mut doc);
            let runs = doc["runs"].as_array()?;
            let has = |c: &str| runs.iter().any(|r| r["condition"] == c);
            (has("A") && has("B")).then_some(doc)
        })
        .map(|doc| {
            let time = doc["timestamp"].as_str().and_then(parse_time).unwrap_or(0);
            let at = nearest(&snaps, doc["sha"].as_str().unwrap_or(""), time);
            (doc, at)
        });

    let last = &snaps[snaps.len() - 1].sha;
    let commits = git(&ctx.repo, &["rev-list", "--count", last]).and_then(|s| s.parse().ok());
    Ok(Data {
        snaps,
        benches,
        eval,
        commits,
    })
}

/// The snapshot a result belongs with: the first at the same commit, else
/// the nearest in time (the earlier on a tie).
fn nearest(snaps: &[Snap], sha: &str, time: i64) -> usize {
    if !sha.is_empty()
        && let Some(i) = snaps.iter().position(|s| s.sha == sha)
    {
        return i;
    }
    (0..snaps.len())
        .min_by_key(|&i| (snaps[i].time - time).abs())
        .unwrap_or(0)
}

/// Unix seconds from "2026-09-26T05:09:12Z" or the compact
/// "20260926T050912Z": the digits are the same either way.
fn parse_time(s: &str) -> Option<i64> {
    let d: Vec<i64> = s
        .chars()
        .filter_map(|c| c.to_digit(10).map(i64::from))
        .collect();
    if d.len() != 14 {
        return None;
    }
    let num = |r: std::ops::Range<usize>| d[r].iter().fold(0, |a, x| a * 10 + x);
    let (y, mo, day) = (num(0..4), num(4..6), num(6..8));
    let (h, mi, sec) = (num(8..10), num(10..12), num(12..14));
    if !(1..=12).contains(&mo) || !(1..=31).contains(&day) {
        return None;
    }
    Some(days_from_civil(y, mo, day) * 86400 + h * 3600 + mi * 60 + sec)
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Hinnant's
/// algorithm; `record::civil_from_days` is its inverse).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn fmt_date(t: i64) -> String {
    let (_, iso) = crate::record::utc_timestamps(u64::try_from(t).unwrap_or(0));
    iso[..10].to_string()
}

fn fmt_hm(t: i64) -> String {
    let s = t.rem_euclid(86400);
    format!("{:02}:{:02}", s / 3600, s / 60 % 60)
}

fn fmt_when(t: i64) -> String {
    format!("{} {} UTC", fmt_date(t), fmt_hm(t))
}

/// "2026-09-26 04:14 – 22:24 UTC", with the second date when they differ.
fn fmt_span(a: i64, b: i64) -> String {
    if fmt_date(a) == fmt_date(b) {
        format!("{} {} \u{2013} {} UTC", fmt_date(a), fmt_hm(a), fmt_hm(b))
    } else {
        format!(
            "{} {} \u{2013} {} {} UTC",
            fmt_date(a),
            fmt_hm(a),
            fmt_date(b),
            fmt_hm(b)
        )
    }
}

/// 1719 -> "1,719".
fn thousands(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

// ---------------------------------------------------------------------------
// Timeline

/// The frames in order, each with how many times it repeats. Consecutive
/// identical frames are merged so a held scene is drawn and encoded once.
fn plan(data: &Data, fps: u32, hold: f64) -> Vec<(Frame, usize)> {
    let n = |s: f64| ((s * f64::from(fps)).round() as usize).max(1);
    let mut p: Vec<(Frame, usize)> = Vec::new();
    let push = |p: &mut Vec<(Frame, usize)>, f: Frame, count: usize| match p.last_mut() {
        Some((last, c)) if *last == f => *c += count,
        _ => p.push((f, count)),
    };
    let fade = |p: &mut Vec<(Frame, usize)>, a: Scene, b: Scene| {
        let k = n(FADE_S);
        for j in 1..=k {
            push(p, Frame::Fade(a, b, j as f64 / (k + 1) as f64), 1);
        }
    };

    push(&mut p, Frame::Still(Scene::Title), n(hold * 1.5));
    fade(&mut p, Scene::Title, Scene::Snap(0, 1.0));
    for i in 0..data.snaps.len() {
        if i > 0 {
            let k = n(TRANSITION_S);
            for j in 1..=k {
                push(&mut p, Frame::Still(Scene::Snap(i, j as f64 / k as f64)), 1);
            }
        }
        let settled = Scene::Snap(i, 1.0);
        push(&mut p, Frame::Still(settled), n(hold));

        let mut interludes: Vec<Scene> = (0..data.benches.len())
            .filter(|&k| data.benches[k].at == i)
            .map(Scene::Bench)
            .collect();
        if data.eval.as_ref().is_some_and(|e| e.1 == i) {
            interludes.push(Scene::Eval);
        }
        let mut shown = settled;
        for s in interludes {
            fade(&mut p, shown, s);
            push(&mut p, Frame::Still(s), n(hold * 2.0));
            shown = s;
        }
        if shown != settled {
            fade(&mut p, shown, settled);
            push(&mut p, Frame::Still(settled), n(FADE_S));
        }
    }
    fade(&mut p, Scene::Snap(data.snaps.len() - 1, 1.0), Scene::End);
    push(&mut p, Frame::Still(Scene::End), n(hold * 2.5));
    p
}

/// Render every frame to `dir/frame_NNNNN.png`, in parallel. A repeated
/// frame is written once and hard-linked for its repeats (copied where
/// links are unsupported), which keeps a long hold from costing disk.
fn write_frames(data: &Data, plan: &[(Frame, usize)], dir: &Path) -> Result<(), String> {
    let mut starts = Vec::with_capacity(plan.len());
    let mut next = 0usize;
    for (_, count) in plan {
        starts.push(next);
        next += count;
    }
    plan.par_iter()
        .zip(starts)
        .try_for_each(|(&(frame, count), start)| {
            let png = encode_png(&render(data, frame))?;
            let first = dir.join(frame_name(start));
            fs::write(&first, &png).map_err(|e| format!("{}: {e}", first.display()))?;
            for k in 1..count {
                let path = dir.join(frame_name(start + k));
                if fs::hard_link(&first, &path).is_err() {
                    fs::write(&path, &png).map_err(|e| format!("{}: {e}", path.display()))?;
                }
            }
            Ok(())
        })
}

fn frame_name(i: usize) -> String {
    format!("frame_{i:05}.png")
}

fn encode_png(px: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(BufWriter::new(&mut out), WIDTH as u32, HEIGHT as u32);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().map_err(|e| e.to_string())?;
        w.write_image_data(px).map_err(|e| e.to_string())?;
        w.finish().map_err(|e| e.to_string())?;
    }
    Ok(out)
}

/// H.264 in yuv420p (what players and browsers expect), CRF 20. The thread
/// count is fixed because x264's output depends on it: left to default it
/// follows the machine's core count, and the same data would give a
/// different file on another machine. `bitexact` and dropping metadata keep
/// the container free of anything that varies between runs.
fn encode(ffmpeg: &Path, dir: &Path, fps: u32, out: &Path) -> Result<(), String> {
    let status = Command::new(ffmpeg)
        .args(["-y", "-loglevel", "error", "-framerate"])
        .arg(fps.to_string())
        .arg("-i")
        .arg(dir.join("frame_%05d.png"))
        .args([
            "-c:v",
            "libx264",
            "-preset",
            "medium",
            "-crf",
            "20",
            "-pix_fmt",
            "yuv420p",
            "-threads",
            "4",
            "-map_metadata",
            "-1",
            "-fflags",
            "+bitexact",
            "-flags:v",
            "+bitexact",
            "-movflags",
            "+faststart",
        ])
        .arg(out)
        .status()
        .map_err(|e| format!("{}: {e} (pass --ffmpeg PATH)", ffmpeg.display()))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("ffmpeg failed ({status})"))
    }
}

// ---------------------------------------------------------------------------
// Drawing

/// Smoothstep: transitions start and end at rest.
fn ease(f: f64) -> f64 {
    let f = f.clamp(0.0, 1.0);
    f * f * (3.0 - 2.0 * f)
}

fn lerp(a: f64, b: f64, f: f64) -> f64 {
    a + (b - a) * f
}

fn lerp_rgb(a: Rgb, b: Rgb, f: f64) -> Rgb {
    [0, 1, 2].map(|i| lerp(f64::from(a[i]), f64::from(b[i]), f).round() as u8)
}

fn render(data: &Data, frame: Frame) -> Vec<u8> {
    match frame {
        Frame::Still(s) => scene(data, s),
        Frame::Fade(a, b, f) => {
            let (a, b, f) = (scene(data, a), scene(data, b), ease(f));
            a.iter()
                .zip(&b)
                .map(|(&x, &y)| lerp(f64::from(x), f64::from(y), f).round() as u8)
                .collect()
        }
    }
}

fn scene(data: &Data, s: Scene) -> Vec<u8> {
    match s {
        Scene::Title => title_card(data),
        Scene::Snap(i, f) => snapshot_frame(data, i, f),
        Scene::Bench(k) => bench_chart::render(&data.benches[k].doc),
        Scene::Eval => eval_frame(data),
        Scene::End => end_card(data),
    }
}

/// Width in pixels of `s` drawn at `scale`.
fn text_w(s: &str, scale: usize) -> usize {
    (s.chars().count() * 6).saturating_sub(1) * scale
}

fn centred(cv: &mut Canvas, y: usize, s: &str, scale: usize, c: Rgb) {
    let x = WIDTH.saturating_sub(text_w(s, scale)) / 2;
    cv.text(x.max(MARGIN), y, s, scale, c);
}

fn right(cv: &mut Canvas, x_end: usize, y: usize, s: &str, scale: usize, c: Rgb) {
    cv.text(x_end.saturating_sub(text_w(s, scale)), y, s, scale, c);
}

/// A line `t` pixels thick, stamped as squares along the segment.
fn line(cv: &mut Canvas, a: (f64, f64), b: (f64, f64), t: usize, c: Rgb) {
    let steps = (b.0 - a.0).abs().max((b.1 - a.1).abs()).ceil().max(1.0) as usize;
    for k in 0..=steps {
        let f = k as f64 / steps as f64;
        let x = lerp(a.0, b.0, f).round() as usize;
        let y = lerp(a.1, b.1, f).round() as usize;
        cv.rect(x.saturating_sub(t / 2), y.saturating_sub(t / 2), t, t, c);
    }
}

fn title_card(data: &Data) -> Vec<u8> {
    let mut cv = Canvas::new();
    let (first, last) = (&data.snaps[0], &data.snaps[data.snaps.len() - 1]);
    centred(
        &mut cv,
        360,
        "NEOSCAD \u{2014} REBUILDING OPENSCAD",
        8,
        TEXT,
    );
    centred(&mut cv, 470, &fmt_span(first.time, last.time), 4, GREEN);
    centred(
        &mut cv,
        540,
        &format!(
            "{} PROGRESS SNAPSHOTS AGAINST OPENSCAD'S OWN REGRESSION SUITE",
            data.snaps.len()
        ),
        3,
        DIM,
    );
    // The legend is 406 px wide: four swatches and labels, 19 characters.
    grid::legend(&mut cv, (WIDTH - 406) / 2, 640, None);
    centred(
        &mut cv,
        680,
        &format!(
            "EACH SQUARE IS ONE OF THE SUITE'S {} TESTS",
            thousands(last.cells.len())
        ),
        2,
        DIM,
    );
    cv.px
}

fn snapshot_frame(data: &Data, i: usize, f: f64) -> Vec<u8> {
    let mut cv = Canvas::new();
    let n = data.snaps.len();
    let cur = &data.snaps[i];
    let prev = if i > 0 { &data.snaps[i - 1] } else { cur };
    let e = ease(f);

    // Caption: the commit this scene measured.
    cv.text(
        MARGIN,
        20,
        &format!("{}   {}", cur.when, cur.label),
        4,
        TEXT,
    );
    right(
        &mut cv,
        WIDTH - MARGIN,
        30,
        &format!("SNAPSHOT {} OF {n}", i + 1),
        2,
        DIM,
    );
    cv.text(MARGIN, 64, &cur.subject, 3, DIM);
    grid::legend(&mut cv, MARGIN, 104, None);

    // Grid: colours blend cell by cell when both snapshots used the same
    // test list. When the manifest changed the cells do not correspond, so
    // the grid switches at the midpoint instead of blending unrelated tests.
    let same = prev.cells.len() == cur.cells.len()
        && prev.cells.iter().zip(&cur.cells).all(|(a, b)| a.0 == b.0);
    let cells: Vec<(u8, Rgb, Status)> = if same {
        prev.cells
            .iter()
            .zip(&cur.cells)
            .map(|(a, b)| (b.0, lerp_rgb(grid::colour(a.1), grid::colour(b.1), e), b.1))
            .collect()
    } else {
        let src = if e < 0.5 { prev } else { cur };
        src.cells
            .iter()
            .map(|&(t, s)| (t, grid::colour(s), s))
            .collect()
    };
    grid::draw_tiers(
        &mut cv,
        (MARGIN, CONTENT_Y, GRID_W, HEIGHT - 12 - CONTENT_Y),
        &grid::by_tier(cells.into_iter()),
        false,
    );

    panel(&mut cv, data, i, e);
    cv.px
}

/// The right-hand panel: total passes, per-tier counts and bars, and the
/// chart of total passes over time, all interpolated by `e`.
fn panel(cv: &mut Canvas, data: &Data, i: usize, e: f64) {
    let x = PANEL_X;
    let x_end = PANEL_X + PANEL_W;
    let cur = data.snaps[i].tier_counts();
    let prev = if i > 0 {
        data.snaps[i - 1].tier_counts()
    } else {
        cur
    };
    let tiers: Vec<(usize, usize)> = (0..TIERS)
        .map(|t| {
            let p = lerp(prev[t].0 as f64, cur[t].0 as f64, e).round() as usize;
            let r = lerp(prev[t].1 as f64, cur[t].1 as f64, e).round() as usize;
            (p, r)
        })
        .collect();
    let total: usize = tiers.iter().map(|t| t.0).sum();
    let runnable: usize = tiers.iter().map(|t| t.1).sum();

    cv.text(x, CONTENT_Y, "CONFORMANCE PASSES", 2, DIM);
    cv.text(x, CONTENT_Y + 26, &thousands(total), 8, GREEN);
    cv.text(
        x,
        CONTENT_Y + 92,
        &format!("OF {} RUNNABLE TESTS", thousands(runnable)),
        2,
        DIM,
    );
    let mut y = CONTENT_Y + 130;
    for (t, &(p, r)) in tiers.iter().enumerate() {
        cv.text(
            x,
            y,
            &format!("TIER {t} {}", TIER_NAMES[t].to_uppercase()),
            2,
            TEXT,
        );
        right(cv, x_end, y, &format!("{p} / {r}"), 2, TEXT);
        cv.rect(x, y + 20, PANEL_W, 8, TRACK);
        if let Some(w) = (PANEL_W * p.min(r)).checked_div(r) {
            cv.rect(x, y + 20, w, 8, GREEN);
        }
        y += 44;
    }

    chart(cv, data, i, e, y + 20);
}

/// Total passes against time, snapshot by snapshot, with the newest point
/// sliding from the previous one as `e` goes from 0 to 1.
fn chart(cv: &mut Canvas, data: &Data, i: usize, e: f64, top: usize) {
    cv.text(PANEL_X, top, "TOTAL PASSES OVER TIME", 2, DIM);
    let (px0, px1) = (PANEL_X + 60, PANEL_X + PANEL_W - 8);
    let (py0, py1) = (top + 36, HEIGHT - 70);

    let max = data.snaps.iter().map(Snap::passes).max().unwrap_or(0);
    let step = [100usize, 200, 250, 500, 1000, 2000, 5000, 10_000, 20_000]
        .into_iter()
        .find(|s| max.div_ceil(*s) <= 4)
        .unwrap_or(50_000);
    let ymax = max.div_ceil(step).max(1) * step;
    let ys = |v: f64| py1 as f64 - v / ymax as f64 * (py1 - py0) as f64;
    let (t0, t1) = (data.snaps[0].time, data.snaps[data.snaps.len() - 1].time);
    let span = (t1 - t0).max(1);
    let xs = |t: i64| px0 as f64 + (t - t0) as f64 / span as f64 * (px1 - px0) as f64;

    // Horizontal gridlines with their values, then hour ticks.
    for k in 0..=ymax / step {
        let v = k * step;
        let y = ys(v as f64).round() as usize;
        cv.rect(px0, y, px1 - px0, 1, TRACK);
        right(cv, px0 - 10, y.saturating_sub(7), &thousands(v), 2, DIM);
    }
    let hours = [1i64, 2, 3, 4, 6, 12, 24, 48, 168]
        .into_iter()
        .find(|h| span / (h * 3600) <= 4)
        .unwrap_or(720)
        * 3600;
    let mut tick = (t0 + hours - 1).div_euclid(hours) * hours;
    while tick <= t1 {
        let x = xs(tick).round() as usize;
        cv.rect(x, py0, 1, py1 - py0, TRACK);
        let label = fmt_hm(tick);
        cv.text(
            x.saturating_sub(text_w(&label, 2) / 2),
            py1 + 12,
            &label,
            2,
            DIM,
        );
        tick += hours;
    }

    let mut pts: Vec<(f64, f64)> = data.snaps[..=i]
        .iter()
        .map(|s| (xs(s.time), ys(s.passes() as f64)))
        .collect();
    if i > 0 {
        let (a, b) = (pts[i - 1], pts[i]);
        pts[i] = (lerp(a.0, b.0, e), lerp(a.1, b.1, e));
    }
    // Area under the line, then the line, then a dot per snapshot.
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (xa, xb) = (a.0.round() as usize, b.0.round() as usize);
        for x in xa..=xb {
            let f = if xb > xa {
                (x - xa) as f64 / (xb - xa) as f64
            } else {
                1.0
            };
            let y = lerp(a.1, b.1, f).round() as usize;
            cv.rect(x, y, 1, py1.saturating_sub(y), FILL);
        }
    }
    for w in pts.windows(2) {
        line(cv, w[0], w[1], 3, GREEN);
    }
    for &(x, y) in &pts {
        let (x, y) = (x.round() as usize, y.round() as usize);
        cv.rect(x.saturating_sub(3), y.saturating_sub(3), 7, 7, TEXT);
    }
}

fn eval_frame(data: &Data) -> Vec<u8> {
    let mut cv = Canvas::new();
    let Some((doc, _)) = &data.eval else {
        return cv.px;
    };
    centred(&mut cv, 50, "AGENT-LOOP EVAL: NEOSCAD MCP VS BASH", 5, TEXT);
    centred(&mut cv, 120, "PILOT, N=1", 4, AMBER);
    let when = doc["timestamp"]
        .as_str()
        .and_then(parse_time)
        .map(fmt_when)
        .unwrap_or_default();
    let sha: String = doc["sha"].as_str().unwrap_or("").chars().take(7).collect();
    centred(
        &mut cv,
        180,
        &format!(
            "MODEL {}   {when}   {sha}{}",
            doc["model"].as_str().unwrap_or("?"),
            if doc["dirty"].as_bool() == Some(true) {
                "-DIRTY"
            } else {
                ""
            }
        ),
        2,
        DIM,
    );
    centred(
        &mut cv,
        206,
        "A: NEOSCAD'S MCP SERVER     B: BASH AND THE OPENSCAD COMMAND LINE",
        2,
        DIM,
    );

    let runs: &[Value] = doc["runs"].as_array().map_or(&[], Vec::as_slice);
    let mut tasks: Vec<&str> = Vec::new();
    for r in runs {
        if let Some(t) = r["task"].as_str()
            && !tasks.contains(&t)
        {
            tasks.push(t);
        }
    }
    let find = |task: &str, cond: &str| {
        runs.iter()
            .find(|r| r["task"] == task && r["condition"] == cond)
    };

    // Columns: task, then per condition pass, tests, tool calls, cost, time.
    const TASK_X: usize = 90;
    const GROUPS: [(&str, &str, usize); 2] = [("A", "MCP (A)", 520), ("B", "BASH (B)", 1200)];
    const COLS: [(&str, usize); 5] = [
        ("PASS", 0),
        ("TESTS", 110),
        ("CALLS", 250),
        ("COST", 380),
        ("TIME", 530),
    ];
    let mut y = 280;
    for (_, name, gx) in GROUPS {
        cv.text(gx, y, name, 3, TEXT);
    }
    y += 40;
    cv.text(TASK_X, y, "TASK", 2, DIM);
    for (_, _, gx) in GROUPS {
        for (h, cx) in COLS {
            cv.text(gx + cx, y, h, 2, DIM);
        }
    }
    y += 30;
    cv.rect(TASK_X, y, WIDTH - 2 * TASK_X, 2, TRACK);
    y += 20;

    #[derive(Default)]
    struct Sum {
        pass: usize,
        runs: usize,
        tests: (u64, u64),
        calls: u64,
        cost: f64,
        wall: f64,
    }
    let mut sums = [Sum::default(), Sum::default()];
    for task in &tasks {
        cv.text(TASK_X, y, &task.replace('_', " ").to_uppercase(), 3, TEXT);
        for (k, (cond, _, gx)) in GROUPS.into_iter().enumerate() {
            let Some(r) = find(task, cond) else {
                cv.text(gx, y, "-", 3, DIM);
                continue;
            };
            let pass = r["pass"].as_bool() == Some(true);
            let (passed, total) = (
                r["grade"]["passed"].as_u64().unwrap_or(0),
                r["grade"]["total"].as_u64().unwrap_or(0),
            );
            let calls = r["tool_calls"].as_u64().unwrap_or(0);
            let cost = r["cost_usd"].as_f64().unwrap_or(0.0);
            let wall = r["wall_s"].as_f64().unwrap_or(0.0);
            let s = &mut sums[k];
            s.pass += usize::from(pass);
            s.runs += 1;
            s.tests.0 += passed;
            s.tests.1 += total;
            s.calls += calls;
            s.cost += cost;
            s.wall += wall;
            cv.text(
                gx,
                y,
                if pass { "YES" } else { "NO" },
                3,
                if pass { GREEN } else { RED },
            );
            cv.text(gx + COLS[1].1, y, &format!("{passed}/{total}"), 3, TEXT);
            cv.text(gx + COLS[2].1, y, &calls.to_string(), 3, TEXT);
            cv.text(gx + COLS[3].1, y, &format!("${cost:.2}"), 3, TEXT);
            cv.text(gx + COLS[4].1, y, &format!("{wall:.0}S"), 3, TEXT);
        }
        y += 56;
    }
    cv.rect(TASK_X, y, WIDTH - 2 * TASK_X, 2, TRACK);
    y += 20;
    cv.text(TASK_X, y, "TOTAL", 3, TEXT);
    for (k, (_, _, gx)) in GROUPS.into_iter().enumerate() {
        let s = &sums[k];
        let all = s.pass == s.runs && s.runs > 0;
        cv.text(
            gx,
            y,
            &format!("{}/{}", s.pass, s.runs),
            3,
            if all { GREEN } else { AMBER },
        );
        cv.text(
            gx + COLS[1].1,
            y,
            &format!("{}/{}", s.tests.0, s.tests.1),
            3,
            TEXT,
        );
        cv.text(gx + COLS[2].1, y, &s.calls.to_string(), 3, TEXT);
        cv.text(gx + COLS[3].1, y, &format!("${:.2}", s.cost), 3, TEXT);
        cv.text(gx + COLS[4].1, y, &format!("{:.0}S", s.wall), 3, TEXT);
    }

    centred(
        &mut cv,
        HEIGHT - 150,
        "GRADED BY HIDDEN NEOSCAD TEST FILES THE AGENT NEVER SAW",
        2,
        DIM,
    );
    centred(
        &mut cv,
        HEIGHT - 120,
        "ONE RUN PER TASK AND CONDITION: AN ANECDOTE, NOT A MEASUREMENT",
        2,
        DIM,
    );
    if let Some(when) = doc["regraded"].as_str().and_then(parse_time) {
        centred(
            &mut cv,
            HEIGHT - 90,
            &format!("REGRADED {}", fmt_when(when)),
            2,
            DIM,
        );
    }
    cv.px
}

fn end_card(data: &Data) -> Vec<u8> {
    let mut cv = Canvas::new();
    let (first, last) = (&data.snaps[0], &data.snaps[data.snaps.len() - 1]);
    centred(
        &mut cv,
        110,
        "NEOSCAD \u{2014} REBUILDING OPENSCAD",
        6,
        TEXT,
    );
    centred(&mut cv, 190, &fmt_span(first.time, last.time), 3, DIM);

    let counts = last.tier_counts();
    let runnable: usize = counts.iter().map(|c| c.1).sum();
    centred(&mut cv, 270, &thousands(last.passes()), 14, GREEN);
    centred(
        &mut cv,
        390,
        &format!(
            "CONFORMANCE PASSES OF {} RUNNABLE TESTS",
            thousands(runnable)
        ),
        3,
        TEXT,
    );
    let tiers: Vec<String> = counts
        .iter()
        .enumerate()
        .map(|(t, c)| format!("{} {}", TIER_NAMES[t].to_uppercase(), c.0))
        .collect();
    centred(&mut cv, 436, &tiers.join("   "), 2, DIM);

    // Geometric means from the newest full benchmark (else the newest).
    let bench = data
        .benches
        .iter()
        .rev()
        .find(|b| b.doc["quick"].as_bool() != Some(true))
        .or_else(|| data.benches.last());
    let mut y = 520;
    if let Some(b) = bench {
        let when = b.doc["timestamp"]
            .as_str()
            .and_then(parse_time)
            .map(fmt_when)
            .unwrap_or_default();
        centred(
            &mut cv,
            y,
            &format!("BENCHMARK SPEEDUP, GEOMETRIC MEAN ({when})"),
            2,
            DIM,
        );
        y += 40;
        for (id, name, colour) in bench_chart::REFS.iter().filter(|r| r.0 != "neoscad") {
            let g = &b.doc["geomean_speedup"][*id];
            let Some(v) = g["value"].as_f64() else {
                continue;
            };
            let text = format!("{v:.2}X VS {name}  ({} MODELS)", g["models"]);
            let w = text_w(&text, 3) + 26;
            let x = (WIDTH - w) / 2;
            cv.rect(x, y + 3, 16, 16, *colour);
            cv.text(x + 26, y, &text, 3, TEXT);
            y += 40;
        }
    }

    let mut facts = vec![format!("{} SNAPSHOTS", data.snaps.len())];
    if let Some(c) = data.commits {
        facts.insert(0, format!("{} COMMITS", thousands(c)));
    }
    centred(&mut cv, y.max(700) + 30, &facts.join("   "), 3, TEXT);
    centred(
        &mut cv,
        HEIGHT - 80,
        &format!("LAST SNAPSHOT {}  {}", last.label, last.subject),
        2,
        DIM,
    );
    cv.px
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn snap(time: i64, sha: &str, statuses: &str) -> Snap {
        let tiers = [0u8, 0, 1, 1, 3, 4];
        Snap {
            time,
            when: fmt_when(time),
            label: sha.to_uppercase(),
            sha: sha.into(),
            subject: format!("commit {sha}"),
            cells: tiers
                .iter()
                .zip(statuses.chars())
                .map(|(&t, c)| (t, crate::record::parse_status(c).unwrap()))
                .collect(),
        }
    }

    fn data() -> Data {
        let t = parse_time("2026-09-26T04:14:15Z").unwrap();
        let snaps = vec![
            snap(t, "aaaaaaa", "FF-S--"),
            snap(t + 3600, "bbbbbbb", "PPF-S-"),
            snap(t + 9000, "ccccccc", "PPPPSP"),
        ];
        let bench = json!({
            "timestamp": "2026-09-26T05:20:00Z", "short_sha": "bbbbbbb", "sha": "zzz",
            "subject": "bench", "quick": false,
            "binaries": {"neoscad": {}, "nightly-manifold": {}},
            "models": {"a": {"results": {
                "neoscad": {"rc": 0, "best_s": 0.01},
                "nightly-manifold": {"rc": 0, "best_s": 0.1}}}},
            "extra": {},
            "geomean_speedup": {"nightly-manifold": {"value": 10.0, "models": 1,
                "excluded_timeouts_or_failures": []}},
        });
        let eval = json!({
            "timestamp": "20260926T061000Z", "sha": "ccccccc", "model": "sonnet",
            "runs": [
                {"task": "box", "condition": "A", "pass": true, "tool_calls": 3,
                 "cost_usd": 0.1, "wall_s": 40.0, "grade": {"passed": 6, "total": 6}},
                {"task": "box", "condition": "B", "pass": false, "tool_calls": 9,
                 "cost_usd": 0.3, "wall_s": 90.0, "grade": {"passed": 4, "total": 6}},
            ],
        });
        Data {
            benches: vec![Bench {
                at: nearest(&snaps, "zzz", parse_time("2026-09-26T05:20:00Z").unwrap()),
                doc: bench,
            }],
            eval: Some((eval, 2)),
            snaps,
            commits: Some(42),
        }
    }

    #[test]
    fn commit_maps_translate_recorded_ids() {
        let old = "1".repeat(40);
        let new = "2".repeat(40);
        let pruned = "3".repeat(40);
        let text = format!("old new\n{old} {new}\n{pruned} {}\n", "0".repeat(40));
        let map = CommitMap::parse(&text);
        assert_eq!(map.get(&old), Some(new.as_str()));
        // A pruned commit and an unknown one keep their recorded ids.
        assert_eq!(map.get(&pruned), None);
        assert_eq!(map.get("abc"), None);
        let mut doc = json!({ "sha": old, "short_sha": "1111111" });
        map.rewrite_doc(&Ctx::repo_only().unwrap(), &mut doc);
        assert_eq!(doc["sha"], json!(new));
        assert_eq!(doc["short_sha"], json!("2222222"));
    }

    #[test]
    fn timestamps_parse_in_both_forms() {
        let a = parse_time("2026-09-26T05:09:12Z").unwrap();
        assert_eq!(parse_time("20260926T050912Z"), Some(a));
        assert_eq!(fmt_when(a), "2026-09-26 05:09 UTC");
        assert_eq!(parse_time("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse_time("2026-09-26"), None);
        assert_eq!(fmt_span(a, a + 3600), "2026-09-26 05:09 \u{2013} 06:09 UTC");
        assert_eq!(thousands(1719), "1,719");
        assert_eq!(thousands(999), "999");
        assert_eq!(thousands(1_000_000), "1,000,000");
    }

    #[test]
    fn results_attach_to_their_commit_or_the_nearest_snapshot() {
        let d = data();
        // The bench's sha is no snapshot's; 05:20 is nearest 05:14.
        assert_eq!(d.benches[0].at, 1);
        assert_eq!(nearest(&d.snaps, "ccccccc", 0), 2);
    }

    #[test]
    fn plan_runs_title_snapshots_interludes_end() {
        let d = data();
        let p = plan(&d, 10, 1.0);
        assert_eq!(p[0], (Frame::Still(Scene::Title), 15));
        assert_eq!(p.last().unwrap(), &(Frame::Still(Scene::End), 25));
        let stills: Vec<Scene> = p
            .iter()
            .filter_map(|(f, c)| match f {
                Frame::Still(s) if *c > 1 => Some(*s),
                _ => None,
            })
            .collect();
        assert_eq!(
            stills,
            vec![
                Scene::Title,
                Scene::Snap(0, 1.0),
                Scene::Snap(1, 1.0),
                Scene::Bench(0),
                Scene::Snap(1, 1.0),
                Scene::Snap(2, 1.0),
                Scene::Eval,
                Scene::Snap(2, 1.0),
                Scene::End,
            ]
        );
        // No two neighbours are the same frame: holds were merged.
        assert!(p.windows(2).all(|w| w[0].0 != w[1].0));
    }

    #[test]
    fn frames_are_identical_at_any_thread_count() {
        let d = data();
        let p = plan(&d, 2, 0.5);
        let base = std::env::temp_dir().join(format!("neoscad-video-test-{}", std::process::id()));
        let mut outputs = Vec::new();
        for threads in [1, 4] {
            let dir = base.join(threads.to_string());
            prepare_frames_dir(&dir).unwrap();
            rayon::ThreadPoolBuilder::new()
                .num_threads(threads)
                .build()
                .unwrap()
                .install(|| write_frames(&d, &p, &dir))
                .unwrap();
            let mut names: Vec<_> = fs::read_dir(&dir)
                .unwrap()
                .map(|e| e.unwrap().file_name())
                .collect();
            names.sort();
            let files: Vec<Vec<u8>> = names
                .iter()
                .map(|n| fs::read(dir.join(n)).unwrap())
                .collect();
            outputs.push((names, files));
        }
        fs::remove_dir_all(&base).unwrap();
        let total: usize = p.iter().map(|x| x.1).sum();
        assert_eq!(outputs[0].0.len(), total);
        assert_eq!(outputs[0], outputs[1]);
    }

    #[test]
    fn a_transition_ends_on_the_settled_frame() {
        let d = data();
        assert_eq!(
            render(&d, Frame::Still(Scene::Snap(1, 1.0))),
            snapshot_frame(&d, 1, 1.0)
        );
        // Halfway through, a cell that went from fail to pass is neither.
        let mid = snapshot_frame(&d, 1, 0.5);
        let settled = snapshot_frame(&d, 1, 1.0);
        assert_ne!(mid, settled);
        assert_eq!(snapshot_frame(&d, 1, 0.0).len(), WIDTH * HEIGHT * 3);
    }
}
