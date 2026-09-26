//! The progress grid: one coloured cell per manifest test, grouped by tier,
//! on a fixed 1920x1080 canvas so snapshots can be stitched into a video.
//!
//! Cell positions depend only on the manifest (its order and per-tier
//! counts), so a test stays in the same place from frame to frame and only
//! its colour changes. Text uses a built-in 5x7 bitmap font to avoid
//! shipping or locating a font file.
//!
//! Snapshots do not store the image: `conformance grid` rebuilds it from a
//! snapshot's compact `scoreboard.json` and `meta.json` plus the manifest
//! the run used (see `cells_for`).

use std::fs::{self, File};
use std::io::BufWriter;
use std::path::{Path, PathBuf};

use crate::ctx::Ctx;
use crate::manifest::{Manifest, TIER_NAMES};
use crate::record::{self, Scoreboard};
use crate::run::Status;
use crate::sha256;

pub const WIDTH: usize = 1920;
pub const HEIGHT: usize = 1080;
const MARGIN: usize = 24;

type Rgb = [u8; 3];
const BG: Rgb = [22, 22, 26];
const TEXT: Rgb = [235, 235, 235];
const DIM: Rgb = [150, 150, 160];

pub fn colour(s: Status) -> Rgb {
    match s {
        Status::Pass => [46, 204, 113],
        Status::Fail => [231, 76, 60],
        Status::Skip => [62, 62, 70],
        Status::Pending => [72, 96, 150],
    }
}

struct Canvas {
    px: Vec<u8>,
}

impl Canvas {
    fn new() -> Self {
        let mut px = vec![0; WIDTH * HEIGHT * 3];
        for p in px.as_chunks_mut::<3>().0 {
            *p = BG;
        }
        Self { px }
    }

    fn rect(&mut self, x: usize, y: usize, w: usize, h: usize, c: Rgb) {
        for yy in y..(y + h).min(HEIGHT) {
            for xx in x..(x + w).min(WIDTH) {
                let i = (yy * WIDTH + xx) * 3;
                self.px[i..i + 3].copy_from_slice(&c);
            }
        }
    }

    /// Draw text at `scale` (glyphs are 5x7, advance 6). Returns the x after
    /// the last glyph. Text past the right margin is cut off.
    fn text(&mut self, x: usize, y: usize, s: &str, scale: usize, c: Rgb) -> usize {
        let mut cx = x;
        for ch in s.chars() {
            if cx + 5 * scale > WIDTH - MARGIN {
                break;
            }
            let g = glyph(ch);
            for (row, bits) in g.iter().enumerate() {
                for col in 0..5 {
                    if bits & (0x10 >> col) != 0 {
                        self.rect(cx + col * scale, y + row * scale, scale, scale, c);
                    }
                }
            }
            cx += 6 * scale;
        }
        cx
    }
}

/// The header line. Both `--record --grid` and `conformance grid` build it
/// here, so a regenerated image matches one written at record time.
pub fn title(iso_timestamp: &str, short_sha: &str, dirty: bool) -> String {
    format!(
        "NEOSCAD CONFORMANCE  {iso_timestamp}  {short_sha}{}",
        if dirty { "-DIRTY" } else { "" }
    )
}

/// Render `cells` (tier, status) in manifest order to a PNG.
pub fn write_png(
    path: &Path,
    title: &str,
    subject: &str,
    cells: &[(u8, Status)],
) -> Result<(), String> {
    let img = render(title, subject, cells);
    let file = File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(BufWriter::new(file), WIDTH as u32, HEIGHT as u32);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().map_err(|e| e.to_string())?;
    w.write_image_data(&img).map_err(|e| e.to_string())?;
    w.finish().map_err(|e| e.to_string())
}

fn render(title: &str, subject: &str, cells: &[(u8, Status)]) -> Vec<u8> {
    let mut cv = Canvas::new();
    let mut totals = [0usize; 4];
    let mut tiers: Vec<(u8, Vec<Status>)> = Vec::new();
    for &(t, s) in cells {
        totals[s as usize] += 1;
        match tiers.last_mut() {
            Some((lt, v)) if *lt == t => v.push(s),
            _ => match tiers.iter_mut().find(|(lt, _)| *lt == t) {
                Some((_, v)) => v.push(s),
                None => tiers.push((t, vec![s])),
            },
        }
    }
    tiers.sort_by_key(|(t, _)| *t);

    // Header: title, commit subject, legend with totals.
    cv.text(MARGIN, 18, title, 3, TEXT);
    cv.text(MARGIN, 52, subject, 2, DIM);
    let mut x = MARGIN;
    let legend_y = 78;
    for (label, s) in [
        ("PASS", Status::Pass),
        ("FAIL", Status::Fail),
        ("SKIP", Status::Skip),
        ("PENDING", Status::Pending),
    ] {
        cv.rect(x, legend_y, 14, 14, colour(s));
        x = cv.text(
            x + 22,
            legend_y,
            &format!("{label} {}", totals[s as usize]),
            2,
            TEXT,
        ) + 30;
    }
    cv.text(x + 10, legend_y, &format!("TOTAL {}", cells.len()), 2, DIM);
    let header = 110;

    // Largest cell pitch that fits every tier on the canvas.
    const LABEL: usize = 22;
    const SECTION_GAP: usize = 10;
    let avail_w = WIDTH - 2 * MARGIN;
    let pitch = (3..=48)
        .rev()
        .find(|&p| {
            let cols = avail_w / p;
            let h: usize = tiers
                .iter()
                .map(|(_, v)| LABEL + v.len().div_ceil(cols) * p + SECTION_GAP)
                .sum();
            header + h <= HEIGHT - MARGIN / 2
        })
        .unwrap_or(3);
    let cols = avail_w / pitch;
    let gap = (pitch / 6).max(1);

    let mut y = header;
    for (t, statuses) in &tiers {
        let mut c = [0usize; 4];
        for s in statuses {
            c[*s as usize] += 1;
        }
        let name = TIER_NAMES
            .get(usize::from(*t))
            .copied()
            .unwrap_or("?")
            .to_uppercase();
        let label = format!(
            "TIER {t} {name}   PASS {}  FAIL {}  SKIP {}  PENDING {}   ({})",
            c[0],
            c[1],
            c[2],
            c[3],
            statuses.len()
        );
        cv.text(MARGIN, y, &label, 2, TEXT);
        y += LABEL;
        for (i, s) in statuses.iter().enumerate() {
            let (row, col) = (i / cols, i % cols);
            cv.rect(
                MARGIN + col * pitch,
                y + row * pitch,
                pitch - gap,
                pitch - gap,
                colour(*s),
            );
        }
        y += statuses.len().div_ceil(cols) * pitch + SECTION_GAP;
    }
    cv.px
}

/// The (tier, status) cells of a snapshot, in manifest order.
///
/// The status string alone has no tiers, so the manifest the run used is
/// needed, identified by its SHA-256. It is looked for, in order:
/// 1. in the scoreboard itself, when record embedded the test list because
///    the manifest differed from HEAD's;
/// 2. at the recorded commit (`git show <sha>:conformance/manifest.json`);
/// 3. in the working tree. This rescues a snapshot whose commit git no
///    longer has (rebased away), or a dirty one whose manifest matched only
///    the working tree. A hash match is required, so this never silently
///    maps statuses onto a different test list.
pub fn cells_for(ctx: &Ctx, sb: &Scoreboard) -> Result<Vec<(u8, Status)>, String> {
    let statuses = sb.statuses()?;
    let tiers: Vec<u8> = if let Some(e) = &sb.embedded {
        e.tiers
            .chars()
            .map(|c| {
                c.to_digit(10)
                    .and_then(|d| u8::try_from(d).ok())
                    .ok_or_else(|| format!("bad tier {c:?}"))
            })
            .collect::<Result<_, _>>()?
    } else {
        let want = sb
            .manifest_sha256
            .as_deref()
            .ok_or("scoreboard has neither manifest_sha256 nor embedded tests")?;
        let bytes = [
            record::manifest_at(ctx, &sb.sha),
            fs::read(ctx.manifest_path()).ok(),
        ]
        .into_iter()
        .flatten()
        .find(|b| sha256::hex(b) == want)
        .ok_or_else(|| {
            format!(
                "no manifest with sha256 {want} at commit {} or in the working tree",
                sb.sha
            )
        })?;
        let m: Manifest =
            serde_json::from_slice(&bytes).map_err(|e| format!("manifest {want}: {e}"))?;
        m.tests.iter().map(|c| c.tier).collect()
    };
    if tiers.len() != statuses.len() {
        return Err(format!(
            "{} statuses but {} tests in the manifest",
            statuses.len(),
            tiers.len()
        ));
    }
    Ok(tiers.into_iter().zip(statuses).collect())
}

/// Render one snapshot directory's grid to `out`.
pub fn render_snapshot(ctx: &Ctx, dir: &Path, out: &Path) -> Result<(), String> {
    let sb = Scoreboard::load(&dir.join("scoreboard.json"))?;
    let meta_path = dir.join("meta.json");
    let meta: serde_json::Value = fs::read_to_string(&meta_path)
        .map_err(|e| format!("{}: {e}", meta_path.display()))
        .and_then(|t| {
            serde_json::from_str(&t).map_err(|e| format!("{}: {e}", meta_path.display()))
        })?;
    let short = meta["short_sha"]
        .as_str()
        .map_or_else(|| sb.sha.chars().take(7).collect(), str::to_string);
    let subject = meta["subject"].as_str().unwrap_or_default();
    let cells = cells_for(ctx, &sb)?;
    write_png(
        out,
        &title(&sb.timestamp, &short, sb.dirty),
        subject,
        &cells,
    )
}

/// `conformance grid`: write `grid.png` for the given snapshots, or for all
/// in `progress/index.jsonl`. Returns the exit code: 1 if any failed.
pub fn command(
    ctx: &Ctx,
    dirs: &[PathBuf],
    all: bool,
    out: Option<&Path>,
    force: bool,
) -> Result<u8, String> {
    let progress = ctx.progress_dir();
    let mut targets: Vec<PathBuf> = dirs
        .iter()
        .map(|d| {
            if d.is_dir() {
                d.clone()
            } else {
                progress.join(d)
            }
        })
        .collect();
    if all {
        let index = progress.join("index.jsonl");
        let text = fs::read_to_string(&index).map_err(|e| format!("{}: {e}", index.display()))?;
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let v: serde_json::Value =
                serde_json::from_str(line).map_err(|e| format!("{}: {e}", index.display()))?;
            if let Some(d) = v["dir"].as_str() {
                targets.push(progress.join(d));
            }
        }
    }
    if targets.is_empty() {
        return Err("name snapshot directories, or pass --all".into());
    }
    if out.is_some() && targets.len() != 1 {
        return Err("--out needs exactly one snapshot".into());
    }

    let mut failed = false;
    for dir in &targets {
        let dest = out.map_or_else(|| dir.join("grid.png"), Path::to_path_buf);
        // An explicit --out is a request for that file, so it is always
        // written; the default target is skipped when present, so that
        // `--all` only fills gaps and never repaints an existing image.
        if out.is_none() && dest.exists() && !force {
            println!("skip {} (exists; --force to redo)", dest.display());
            continue;
        }
        match render_snapshot(ctx, dir, &dest) {
            Ok(()) => println!("wrote {}", dest.display()),
            Err(e) => {
                eprintln!("{}: {e}", dir.display());
                failed = true;
            }
        }
    }
    Ok(u8::from(failed))
}

/// 5x7 glyphs, one byte per row, bit 4 = leftmost column. Lower case is
/// drawn as upper case; unknown characters as '?'.
fn glyph(c: char) -> [u8; 7] {
    match c.to_ascii_uppercase() {
        'A' => [0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x0A, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x11, 0x0A, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        ' ' => [0; 7],
        '.' => [0, 0, 0, 0, 0, 0x0C, 0x0C],
        ',' => [0, 0, 0, 0, 0x0C, 0x04, 0x08],
        ':' => [0, 0x0C, 0x0C, 0, 0x0C, 0x0C, 0],
        ';' => [0, 0x0C, 0x0C, 0, 0x0C, 0x04, 0x08],
        '-' => [0, 0, 0, 0x1F, 0, 0, 0],
        '_' => [0, 0, 0, 0, 0, 0, 0x1F],
        '/' => [0, 0x01, 0x02, 0x04, 0x08, 0x10, 0],
        '\\' => [0, 0x10, 0x08, 0x04, 0x02, 0x01, 0],
        '(' => [0x02, 0x04, 0x08, 0x08, 0x08, 0x04, 0x02],
        ')' => [0x08, 0x04, 0x02, 0x02, 0x02, 0x04, 0x08],
        '[' => [0x0E, 0x08, 0x08, 0x08, 0x08, 0x08, 0x0E],
        ']' => [0x0E, 0x02, 0x02, 0x02, 0x02, 0x02, 0x0E],
        '<' => [0x02, 0x04, 0x08, 0x10, 0x08, 0x04, 0x02],
        '>' => [0x08, 0x04, 0x02, 0x01, 0x02, 0x04, 0x08],
        '%' => [0x18, 0x19, 0x02, 0x04, 0x08, 0x13, 0x03],
        '+' => [0, 0x04, 0x04, 0x1F, 0x04, 0x04, 0],
        '=' => [0, 0, 0x1F, 0, 0x1F, 0, 0],
        '#' => [0x0A, 0x0A, 0x1F, 0x0A, 0x1F, 0x0A, 0x0A],
        '*' => [0, 0x04, 0x15, 0x0E, 0x15, 0x04, 0],
        '&' => [0x0C, 0x12, 0x14, 0x08, 0x15, 0x12, 0x0D],
        '@' => [0x0E, 0x11, 0x17, 0x15, 0x17, 0x10, 0x0F],
        '!' => [0x04, 0x04, 0x04, 0x04, 0x04, 0, 0x04],
        '|' => [0x04; 7],
        '\'' | '`' => [0x0C, 0x04, 0x08, 0, 0, 0, 0],
        '"' => [0x0A, 0x0A, 0, 0, 0, 0, 0],
        _ => [0x0E, 0x11, 0x01, 0x02, 0x04, 0, 0x04],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::time::Duration;

    use crate::run::{Counts, Outcome, RunReport};

    /// A small manifest with tiers interleaved out of order, plus a run over
    /// it with every status and repeated and unique failure reasons.
    fn fixture() -> (String, Manifest, RunReport) {
        let tiers = [0u8, 0, 1, 2, 1, 3, 3, 4, 5, 2, 0, 4];
        let case = |i: usize, t: u8| {
            format!(
                r#"{{"id":"t{i}","tier":{t},"runner":"text","group":"g","suffix":"x","args":[],"configs":[],"cmake_line":1}}"#
            )
        };
        let tests: Vec<String> = tiers.iter().enumerate().map(|(i, t)| case(i, *t)).collect();
        let text = format!(
            r#"{{"schema":1,"generated_by":"test","reference":{{"path":"r","commit":"c"}},"tier_names":[],"counts":{{}},"skip_reasons":{{}},"generated_files":[],"diagnostics":[],"tests":[{}]}}"#,
            tests.join(",")
        );
        let manifest: Manifest = serde_json::from_str(&text).unwrap();
        let all = [Status::Pass, Status::Fail, Status::Skip, Status::Pending];
        let mut per_tier: BTreeMap<u8, Counts> = BTreeMap::new();
        // Outcomes deliberately not in manifest order, like a parallel run's.
        let outcomes: Vec<Outcome> = manifest
            .tests
            .iter()
            .enumerate()
            .rev()
            .map(|(i, c)| {
                let status = all[i % 4];
                let e = per_tier.entry(c.tier).or_default();
                e.total += 1;
                let reason = (status == Status::Fail)
                    .then(|| if i == 1 { "rare" } else { "common" }.to_string());
                Outcome {
                    id: c.id.clone(),
                    tier: c.tier,
                    status,
                    reason,
                    ms: Some(1.25),
                    excerpt: Vec::new(),
                }
            })
            .collect();
        let report = RunReport {
            outcomes,
            per_tier,
            wall: Duration::from_millis(1500),
            binary: PathBuf::new(),
        };
        (text, manifest, report)
    }

    /// What `--record` used to feed the renderer: manifest order, status
    /// looked up by id.
    fn original_cells(m: &Manifest, r: &RunReport) -> Vec<(u8, Status)> {
        m.tests
            .iter()
            .map(|c| {
                (
                    c.tier,
                    r.outcomes.iter().find(|o| o.id == c.id).unwrap().status,
                )
            })
            .collect()
    }

    fn roundtrip(sb: &Scoreboard) -> Scoreboard {
        let text = serde_json::to_string(sb).unwrap();
        assert!(!text.contains('\n'), "scoreboard must be minified");
        serde_json::from_str(&text).unwrap()
    }

    #[test]
    fn compact_scoreboard_regenerates_the_same_cells() {
        let (text, manifest, report) = fixture();
        let want = original_cells(&manifest, &report);

        // Embedded test list: no manifest lookup at all.
        let sb = roundtrip(&record::build_scoreboard(
            &manifest,
            &report,
            "T",
            "nosuchsha",
            true,
            None,
            true,
        ));
        let nowhere = Ctx {
            repo: PathBuf::from("/nonexistent"),
            ref_root: PathBuf::from("/nonexistent"),
        };
        assert_eq!(cells_for(&nowhere, &sb).unwrap(), want);
        assert_eq!(
            render("t", "s", &cells_for(&nowhere, &sb).unwrap()),
            render("t", "s", &want)
        );

        // Hash only: found in the working tree (the "repo" is not a git
        // repository, so `git show` fails and the fallback is exercised).
        let repo = std::env::temp_dir().join(format!("neoscad-grid-test-{}", std::process::id()));
        fs::create_dir_all(repo.join("conformance")).unwrap();
        fs::write(repo.join("conformance/manifest.json"), &text).unwrap();
        let ctx = Ctx {
            repo: repo.clone(),
            ref_root: repo.clone(),
        };
        let hash = sha256::hex(text.as_bytes());
        let sb = roundtrip(&record::build_scoreboard(
            &manifest,
            &report,
            "T",
            "nosuchsha",
            true,
            Some(hash),
            false,
        ));
        assert!(sb.embedded.is_none());
        assert_eq!(cells_for(&ctx, &sb).unwrap(), want);

        // A manifest with a different hash must be refused, not used.
        fs::write(
            repo.join("conformance/manifest.json"),
            text.replace("\"test\"", "\"other\""),
        )
        .unwrap();
        assert!(cells_for(&ctx, &sb).is_err());
        fs::remove_dir_all(&repo).unwrap();

        // Failure reasons: counted, ids listed for the rare ones.
        assert_eq!(
            sb.failures.values().sum::<usize>(),
            want.iter().filter(|c| c.1 == Status::Fail).count()
        );
        assert_eq!(sb.failure_ids["rare"], vec!["t1".to_string()]);
        assert_eq!(sb.status.len(), manifest.tests.len());
    }

    #[test]
    fn a_large_manifest_fits_the_canvas() {
        // Roughly the real manifest's shape: ~3,300 cells over six tiers.
        let mut cells = Vec::new();
        for (t, n) in [(0u8, 36), (1, 140), (2, 190), (3, 900), (4, 1900), (5, 60)] {
            cells.extend(std::iter::repeat_n((t, Status::Pending), n));
        }
        let px = render("T", "S", &cells);
        assert_eq!(px.len(), WIDTH * HEIGHT * 3);
        // The last pending cell must have been drawn somewhere.
        assert!(px.as_chunks::<3>().0.contains(&colour(Status::Pending)));
    }
}
