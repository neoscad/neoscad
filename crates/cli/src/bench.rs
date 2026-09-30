//! `neoscad bench`: the community benchmark (docs/community-bench.md).
//!
//! Times this binary, and optionally an installed OpenSCAD, on the models
//! of this release's bench kit, the way `conformance bench` does (the
//! timing code is shared, in crates/bench-core), prints a table as it goes
//! and a summary at the end, and writes a result (schema 1,
//! `bench/result.schema.json`) that `--submit` files as an issue on
//! neoscad/benchmarks, where results are shown per released version on
//! neoscad.org.
//!
//! Only official release binaries may submit: before anything runs, the
//! executable's SHA-256 is compared with the release's published list, so
//! a self-built binary learns it cannot submit before it spends an hour
//! timing models, not after.
//!
//! Network access (the kit, the release's executable list) goes through
//! `curl`, which macOS, Windows 10 and later and practically every Linux
//! install ship: an HTTP client and TLS stack in the binary would cost
//! more than a megabyte for two downloads that most users make once.

use std::ffi::{OsStr, OsString};
use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use bench_core::kit::{self, LoadedKit};
use bench_core::official::{self, Check};
use bench_core::result::{BenchResult, KitInfo, Method, Neoscad, Pair, Reference, SCHEMA, Source};
use bench_core::timing::{self, Measurement, RcWord};
use bench_core::{machine, submit};
use clap::Parser;

const EXIT_ERROR: u8 = 1;
const VERSION: &str = env!("CARGO_PKG_VERSION");
/// The Rust target triple this binary was built for (build.rs).
const TARGET: &str = env!("NEOSCAD_TARGET");

#[derive(Parser, Debug)]
#[command(
    name = "neoscad bench",
    about = "Benchmark neoscad (and optionally OpenSCAD) on this release's bench kit, and submit the result",
    long_about = "Benchmark neoscad (and optionally OpenSCAD) on this release's bench kit, \
                  and submit the result to the community benchmarks shown on neoscad.org. \
                  Only official release binaries can submit. See docs/community-bench.md.",
    version
)]
pub(crate) struct Args {
    /// The bench kit: a .tar.gz or an unpacked directory. Default: this
    /// release's kit, downloaded from GitHub, checked against its .sha256
    /// and cached.
    #[arg(long, value_name = "FILE|DIR")]
    kit: Option<PathBuf>,

    /// An OpenSCAD to compare with (its version and backend are recorded,
    /// never its path). Default: look in the usual install locations and
    /// ask.
    #[arg(long, value_name = "PATH", conflicts_with = "no_openscad")]
    openscad: Option<PathBuf>,

    /// Time neoscad alone, without looking for OpenSCAD.
    #[arg(long = "no-openscad")]
    no_openscad: bool,

    /// A short run: the kit's quick models, one run each.
    #[arg(long)]
    quick: bool,

    /// Runs per model, keeping the best (default: the kit's, 3).
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..=20))]
    runs: Option<u32>,

    /// Write the result JSON to this file.
    #[arg(long, value_name = "FILE")]
    json: Option<PathBuf>,

    /// After the run, show the result and, if you agree, submit it to
    /// neoscad/benchmarks as a GitHub issue (official release binaries
    /// only).
    #[arg(long)]
    submit: bool,

    /// Who is running it: `user`, or `ci-baseline` for the release
    /// workflow's own runs.
    #[arg(long, hide = true, value_name = "SOURCE", default_value = "user")]
    source: String,

    /// Read the release's executable checksums from this file instead of
    /// GitHub (tests).
    #[arg(long = "release-sums", hide = true, value_name = "FILE")]
    release_sums: Option<PathBuf>,

    /// Run only these models (repeatable; debugging).
    #[arg(long = "model", hide = true, value_name = "ID")]
    models: Vec<String>,
}

/// Run `neoscad bench` with the arguments after `bench`.
pub fn main(args: Vec<OsString>) -> u8 {
    let argv = std::iter::once(OsString::from("neoscad bench")).chain(args);
    let a = match Args::try_parse_from(argv) {
        Ok(a) => a,
        Err(e) => {
            let _ = e.print();
            return if e.use_stderr() { EXIT_ERROR } else { 0 };
        }
    };
    match run(&a) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("neoscad bench: {e}");
            EXIT_ERROR
        }
    }
}

/// A directory removed when dropped, so an interrupted or failed run does
/// not leave hundreds of megabytes of STL behind.
struct WorkDir(PathBuf);

impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run(a: &Args) -> Result<u8, String> {
    let source = match a.source.as_str() {
        "user" => Source::User,
        "ci-baseline" => Source::CiBaseline,
        s => return Err(format!("unknown --source '{s}' (user or ci-baseline)")),
    };
    if a.submit && source == Source::CiBaseline {
        // Baselines reach the benchmarks repository only as the release
        // workflow's commits, which is how the repository tells them from
        // anyone's issue claiming to be one.
        return Err(
            "--submit is for users' results; the release workflow commits baselines".into(),
        );
    }
    let exe = std::env::current_exe().map_err(|e| format!("cannot find this executable: {e}"))?;
    let sha256 = bench_core::sha256_file(&exe)?;

    // Official or not, before anything is timed.
    let sums = match &a.release_sums {
        Some(p) => Some(std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?),
        None => fetch(&official::release_asset_url(VERSION, official::SUMS_ASSET))
            .ok()
            .map(|b| String::from_utf8_lossy(&b).into_owned()),
    };
    let check = official::check(sums.as_deref(), TARGET, &sha256);
    println!("neoscad {VERSION} ({TARGET}), sha256 {}…", &sha256[..16]);
    println!("  {}", check.explain(VERSION, TARGET));
    if a.submit && !check.official() {
        return Err(format!(
            "--submit takes results from official release binaries only, and this one is not \
             ({}). Released binaries are at https://github.com/{}/releases (or install one \
             through a package manager); without --submit you can still run the benchmark \
             and keep its JSON.",
            match check {
                Check::Unavailable => "it could not be checked",
                _ => "its checksum is not the release's",
            },
            official::RELEASE_REPO
        ));
    }

    let work = WorkDir(std::env::temp_dir().join(format!("neoscad-bench-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&work.0);
    std::fs::create_dir_all(work.0.join("out")).map_err(|e| e.to_string())?;

    let (loaded, archive_sha256) = resolve_kit(a.kit.as_deref(), &work.0)?;
    let k = &loaded.kit;
    println!(
        "kit {} ({} models, BOSL2 {})",
        k.version,
        k.models.len(),
        &k.sources.bosl2_commit[..k.sources.bosl2_commit.len().min(10)]
    );
    if k.version != VERSION {
        eprintln!(
            "note: this is the kit of neoscad {}, not {VERSION}; results are filed under the kit's models, so use this release's kit to submit",
            k.version
        );
    }

    let reference = resolve_openscad(a)?;

    let runs = a
        .runs
        .unwrap_or(if a.quick { k.quick.runs } else { k.runs });
    let cold_runs = if a.quick {
        k.quick.cold_start_runs
    } else {
        k.cold_start.runs.max(runs)
    };
    let timeout = Duration::from_secs_f64(k.timeout_s);
    let single_over = k.single_run_over_s;
    for m in &a.models {
        if !k.models.contains_key(m) {
            return Err(format!("unknown model '{m}'"));
        }
    }

    let started_at = bench_core::utc_now();
    let mut machine = machine::probe();
    if machine.on_battery == Some(true) {
        eprintln!("note: running on battery; times on battery power are often slower");
    }
    let libs = loaded.root.join(&k.library_path);
    let env: Vec<(&str, &OsStr)> = vec![
        ("OPENSCADPATH", libs.as_os_str()),
        ("NEOSCAD_NO_SERVER", OsStr::new("1")),
    ];
    let exe_s = exe.to_string_lossy().into_owned();
    let timed = |bin: &str, extra: &[String], input: &Path, tag: &str, n: u32| {
        let out = work.0.join("out").join(format!("{tag}.stl"));
        let mut cmd = vec![bin.to_string()];
        cmd.extend(extra.iter().cloned());
        cmd.push("-o".into());
        cmd.push(out.to_string_lossy().into_owned());
        cmd.push(input.to_string_lossy().into_owned());
        let log = work.0.join("out").join(format!("{tag}.stderr"));
        let r = timing::measure(&cmd, &work.0, &env, n, single_over, timeout, &log);
        let _ = std::fs::remove_file(&out);
        r
    };

    let with_ref = reference.is_some();
    println!();
    println!(
        "{:<22} {:>12} {:>12} {:>9}",
        "model",
        "neoscad",
        if with_ref { "OpenSCAD" } else { "" },
        if with_ref { "speedup" } else { "" }
    );

    // Cold start first: the least disturbed by what ran before.
    let cs_input = work.0.join("cold_start.scad");
    copy(&loaded.root.join(&k.cold_start.file), &cs_input)?;
    let cold_start = Pair {
        neoscad: timed(&exe_s, &[], &cs_input, "cold_start.neoscad", cold_runs)?,
        openscad: match &reference {
            Some((bin, r)) => Some(timed(
                bin,
                &r.args,
                &cs_input,
                "cold_start.openscad",
                cold_runs,
            )?),
            None => None,
        },
    };
    print_row("cold_start", &cold_start);

    let mut models = std::collections::BTreeMap::new();
    let mut skipped = std::collections::BTreeMap::new();
    for (id, m) in &k.models {
        let wanted = if a.models.is_empty() {
            !a.quick || m.quick
        } else {
            a.models.contains(id)
        };
        if !wanted {
            continue;
        }
        // Imported files are made once, by this neoscad, next to the model.
        let mut failed = None;
        for (name, src) in &m.inputs {
            let target = work.0.join(name);
            if target.is_file() {
                continue;
            }
            eprintln!("  (generating {name})");
            let ok = Command::new(&exe)
                .arg("-o")
                .arg(&target)
                .arg(loaded.root.join(src))
                .current_dir(&work.0)
                .env("NEOSCAD_NO_SERVER", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success());
            if !ok {
                failed = Some(format!("generating {name} failed"));
            }
        }
        if let Some(why) = failed {
            eprintln!("  skipping {id}: {why}");
            skipped.insert(id.clone(), why);
            continue;
        }
        let input = work.0.join(format!("{id}.scad"));
        copy(&loaded.root.join(&m.file), &input)?;
        let pair = Pair {
            neoscad: timed(&exe_s, &[], &input, &format!("{id}.neoscad"), runs)?,
            openscad: match &reference {
                Some((bin, r)) => Some(timed(
                    bin,
                    &r.args,
                    &input,
                    &format!("{id}.openscad"),
                    runs,
                )?),
                None => None,
            },
        };
        print_row(id, &pair);
        models.insert(id.clone(), pair);
    }
    machine.load_after = machine::load_average();

    let result = BenchResult {
        schema: SCHEMA,
        source,
        neoscad: Neoscad {
            version: VERSION.to_string(),
            target: TARGET.to_string(),
            sha256,
            official: check.official(),
            official_check: check,
        },
        kit: KitInfo {
            version: k.version.clone(),
            archive_sha256,
            content_sha256: loaded.content_sha256.clone(),
            neoscad_commit: k.sources.neoscad_commit.clone(),
            bosl2_commit: k.sources.bosl2_commit.clone(),
            openscad_commit: k.sources.openscad_commit.clone(),
        },
        method: Method {
            version: timing::METHOD_VERSION,
            runs,
            cold_start_runs: cold_runs,
            single_run_over_s: single_over,
            timeout_s: k.timeout_s,
            quick: a.quick,
        },
        threads: machine.cores_logical,
        machine,
        openscad: reference.map(|(_, r)| r),
        cold_start,
        models,
        skipped,
        started_at,
        finished_at: bench_core::utc_now(),
    };
    let pretty = serde_json::to_string_pretty(&result).map_err(|e| e.to_string())? + "\n";
    let saved = match &a.json {
        Some(p) => {
            std::fs::write(p, &pretty).map_err(|e| format!("{}: {e}", p.display()))?;
            Some(p.clone())
        }
        None => save_in_cache(&pretty),
    };
    print_summary(&result, saved.as_deref());

    if a.submit {
        submit_result(&result, &pretty, saved.as_deref())?;
    } else if check.official() && result.source == Source::User {
        println!("\nTo share it on neoscad.org, run again with --submit.");
    }
    Ok(0)
}

fn copy(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::copy(from, to)
        .map(|_| ())
        .map_err(|e| format!("{}: {e}", from.display()))
}

/// Fetch `url` with curl: `-f` so an HTTP error is an error, https only.
fn fetch(url: &str) -> Result<Vec<u8>, String> {
    let out = Command::new("curl")
        .args([
            "-fsSL",
            "--proto",
            "=https",
            "--retry",
            "2",
            "--max-time",
            "300",
            url,
        ])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| format!("cannot run curl (needed to download from GitHub): {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "downloading {url} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(out.stdout)
}

/// The user's cache directory for neoscad; `None` when the OS names none.
fn cache_dir() -> Option<PathBuf> {
    let home = || std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        home().map(|h| h.join("Library/Caches/neoscad"))
    } else if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("neoscad").join("cache"))
    } else {
        std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| home().map(|h| h.join(".cache")))
            .map(|d| d.join("neoscad"))
    }
}

/// The kit, unpacked under `work` when it is an archive, with the
/// archive's SHA-256.
fn resolve_kit(arg: Option<&Path>, work: &Path) -> Result<(LoadedKit, Option<String>), String> {
    let archive = match arg {
        Some(p) if p.is_dir() => return Ok((kit::load_dir(p)?, None)),
        Some(p) => std::fs::read(p).map_err(|e| format!("{}: {e}", p.display()))?,
        None => download_kit()?,
    };
    let sha = bench_core::sha256_hex(&archive);
    let dest = work.join("kit");
    std::fs::create_dir_all(&dest).map_err(|e| e.to_string())?;
    kit::extract_tar_gz(&archive, &dest)?;
    Ok((kit::load_dir(&dest)?, Some(sha)))
}

/// This release's kit: from the cache when its hash is the published one,
/// else downloaded and checked.
fn download_kit() -> Result<Vec<u8>, String> {
    let name = kit::asset_name(VERSION);
    let cached = cache_dir().map(|d| d.join("bench-kit").join(&name));
    let published = fetch(&official::release_asset_url(
        VERSION,
        &format!("{name}.sha256"),
    ))
    .map(|b| {
        String::from_utf8_lossy(&b)
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase()
    });
    if let Some(c) = &cached
        && let Ok(bytes) = std::fs::read(c)
    {
        match &published {
            Ok(p) if *p == bench_core::sha256_hex(&bytes) => return Ok(bytes),
            // Offline: the cached copy was checked when it was downloaded.
            Err(_) => {
                eprintln!("note: could not reach GitHub; using the kit cached at download time");
                return Ok(bytes);
            }
            Ok(_) => {}
        }
    }
    let expected = published.map_err(|e| {
        format!(
            "no bench kit for neoscad {VERSION}: {e}\n(a self-built or development version has no \
             published kit; build one with scripts/release/bench-kit.sh and pass --kit)"
        )
    })?;
    eprintln!("downloading {name}");
    let bytes = fetch(&official::release_asset_url(VERSION, &name))?;
    let got = bench_core::sha256_hex(&bytes);
    if got != expected {
        return Err(format!(
            "{name}: SHA-256 {got} is not the published {expected}; not using it"
        ));
    }
    if let Some(c) = &cached
        && let Some(dir) = c.parent()
        && std::fs::create_dir_all(dir).is_ok()
    {
        let _ = std::fs::write(c, &bytes);
    }
    Ok(bytes)
}

/// Where OpenSCAD is usually installed, most likely first.
fn openscad_candidates() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = Vec::new();
    if cfg!(target_os = "macos") {
        v.push("/Applications/OpenSCAD.app/Contents/MacOS/OpenSCAD".into());
        if let Some(h) = std::env::var_os("HOME") {
            v.push(PathBuf::from(h).join("Applications/OpenSCAD.app/Contents/MacOS/OpenSCAD"));
        }
        // Nightlies and versioned installs ("OpenSCAD-2021.01.app").
        if let Ok(rd) = std::fs::read_dir("/Applications") {
            let mut apps: Vec<PathBuf> = rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n.starts_with("OpenSCAD") && n.ends_with(".app"))
                })
                .map(|p| p.join("Contents/MacOS/OpenSCAD"))
                .collect();
            apps.sort();
            apps.reverse();
            v.extend(apps);
        }
    } else if cfg!(windows) {
        for base in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
            if let Some(pf) = std::env::var_os(base) {
                let pf = PathBuf::from(pf);
                v.push(pf.join("OpenSCAD").join("openscad.exe"));
                v.push(pf.join("OpenSCAD (Nightly)").join("openscad.exe"));
            }
        }
    } else {
        for p in [
            "/usr/bin/openscad",
            "/usr/local/bin/openscad",
            "/snap/bin/openscad",
            "/usr/bin/openscad-nightly",
        ] {
            v.push(p.into());
        }
    }
    // Anything on PATH.
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            for name in ["openscad", "openscad.exe", "openscad-nightly"] {
                v.push(dir.join(name));
            }
        }
    }
    v
}

/// Output of `bin args`, stdout then stderr (OpenSCAD prints its version
/// on stderr).
fn output_of(bin: &Path, args: &[&str]) -> Option<String> {
    let o = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let s = String::from_utf8_lossy(&o.stdout).into_owned() + &String::from_utf8_lossy(&o.stderr);
    Some(s.trim().to_string()).filter(|s| !s.is_empty())
}

/// An OpenSCAD's version line and backend. Builds that have `--backend`
/// run with Manifold (what current OpenSCAD recommends, and what neoscad
/// is compared with); older ones only have CGAL.
fn describe_openscad(bin: &Path) -> Result<Reference, String> {
    let version = output_of(bin, &["--version"])
        .and_then(|v| v.lines().last().map(str::to_string))
        .filter(|v| v.to_ascii_lowercase().contains("openscad"))
        .ok_or_else(|| format!("{} does not answer --version like OpenSCAD", bin.display()))?;
    let manifold = output_of(bin, &["--help"]).is_some_and(|h| h.contains("--backend"));
    Ok(if manifold {
        Reference {
            version,
            backend: "manifold".into(),
            args: vec!["--backend=manifold".into()],
        }
    } else {
        Reference {
            version,
            backend: "cgal".into(),
            args: Vec::new(),
        }
    })
}

/// The OpenSCAD to compare with, if any: `--openscad`, or one found and
/// agreed to.
fn resolve_openscad(a: &Args) -> Result<Option<(String, Reference)>, String> {
    if a.no_openscad {
        return Ok(None);
    }
    if let Some(p) = &a.openscad {
        if !p.is_file() {
            return Err(format!("--openscad {}: no such file", p.display()));
        }
        let r = describe_openscad(p)?;
        println!("comparing with {} ({})", r.version, r.backend);
        return Ok(Some((p.to_string_lossy().into_owned(), r)));
    }
    let found = openscad_candidates()
        .into_iter()
        .find(|p| p.is_file())
        .and_then(|p| describe_openscad(&p).ok().map(|r| (p, r)));
    let Some((path, r)) = found else {
        println!(
            "No OpenSCAD found. Results without one are accepted, but a comparison is what makes \
             them most useful: pass --openscad PATH to include one."
        );
        return Ok(None);
    };
    let interactive = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    if !interactive {
        println!(
            "Found {} at {}, not included (no terminal to ask): pass --openscad {} to compare with it.",
            r.version,
            path.display(),
            path.display()
        );
        return Ok(None);
    }
    eprintln!(
        "Found {} ({}) at {}.\nInclude it as a reference? Comparisons with OpenSCAD on neoscad.org \
         need it; it makes the run longer. Only its version and backend are recorded. [Y/n] ",
        r.version,
        r.backend,
        path.display()
    );
    if ask(true) {
        Ok(Some((path.to_string_lossy().into_owned(), r)))
    } else {
        Ok(None)
    }
}

/// A yes/no answer from stdin; empty takes `default`.
fn ask(default: bool) -> bool {
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    if std::io::stdin().lock().read_line(&mut line).is_err() {
        return false;
    }
    match line.trim().to_ascii_lowercase().as_str() {
        "" => default,
        "y" | "yes" => true,
        _ => false,
    }
}

/// A time for the table: ms under a second.
fn fmt_time(m: &Measurement) -> String {
    match (m.best_s, m.rc) {
        (_, timing::Rc::Word(RcWord::Timeout)) => "timeout".into(),
        (Some(s), timing::Rc::Code(0)) if s < 1.0 => format!("{:.1} ms", s * 1000.0),
        (Some(s), timing::Rc::Code(0)) => format!("{s:.2} s"),
        (_, timing::Rc::Code(c)) => format!("rc={c}"),
        (_, timing::Rc::Word(RcWord::Signal)) => "crashed".into(),
    }
}

fn print_row(id: &str, p: &Pair) {
    let (o, speed) = match &p.openscad {
        Some(o) => {
            let speed = match (p.neoscad.best_s, o.best_s) {
                (Some(n), Some(t)) if p.neoscad.rc.ok() && o.rc.ok() && n > 0.0 => {
                    format!("{:.1}x", t / n)
                }
                _ => "-".into(),
            };
            (fmt_time(o), speed)
        }
        None => (String::new(), String::new()),
    };
    println!("{id:<22} {:>12} {o:>12} {speed:>9}", fmt_time(&p.neoscad));
}

fn print_summary(r: &BenchResult, saved: Option<&Path>) {
    let s = r.summary();
    println!();
    println!("Summary");
    println!(
        "  {} models; neoscad finished {} (best times total {:.2} s), cold start {}",
        s.models,
        s.neoscad_ok,
        s.neoscad_total_s,
        fmt_time(&r.cold_start.neoscad)
    );
    if let Some(o) = &r.openscad {
        match s.geomean_speedup {
            Some(g) => println!(
                "  vs {} ({}): {} models compared, geometric mean speedup {g:.1}x",
                o.version, o.backend, s.compared
            ),
            None => println!(
                "  vs {} ({}): no model finished by both",
                o.version, o.backend
            ),
        }
    }
    if !r.skipped.is_empty() {
        println!(
            "  skipped: {}",
            r.skipped.keys().cloned().collect::<Vec<_>>().join(", ")
        );
    }
    if let Some(p) = saved {
        println!("  result: {}", p.display());
    }
}

/// Keep a result the user did not ask to save, so it can be submitted by
/// hand later.
fn save_in_cache(pretty: &str) -> Option<PathBuf> {
    let dir = cache_dir()?.join("bench-results");
    std::fs::create_dir_all(&dir).ok()?;
    let stamp = bench_core::utc_now().replace(':', "-");
    let p = dir.join(format!("neoscad-bench-{VERSION}-{stamp}.json"));
    std::fs::write(&p, pretty).ok()?;
    Some(p)
}

/// Show the exact payload, ask, and file the issue: with `gh` when it is
/// installed and logged in, else through a pre-filled browser URL.
fn submit_result(r: &BenchResult, pretty: &str, saved: Option<&Path>) -> Result<(), String> {
    println!(
        "\nThis is exactly what will be submitted, as a public issue on github.com/{}:\n",
        submit::BENCH_REPO
    );
    print!("{pretty}");
    if !std::io::stdin().is_terminal() {
        return Err("--submit asks for confirmation, and stdin is not a terminal".into());
    }
    eprint!("\nSubmit this result publicly? [y/N] ");
    if !ask(false) {
        println!("Not submitted.");
        return Ok(());
    }
    let title = r.title();
    let gh_ok = Command::new("gh")
        .args(["auth", "status"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if gh_ok {
        let body = submit::issue_body(pretty, None);
        let body_file =
            std::env::temp_dir().join(format!("neoscad-bench-issue-{}.md", std::process::id()));
        std::fs::write(&body_file, body).map_err(|e| e.to_string())?;
        let status = Command::new("gh")
            .args([
                "issue",
                "create",
                "--repo",
                submit::BENCH_REPO,
                "--title",
                &title,
                "--body-file",
            ])
            .arg(&body_file)
            .status();
        let _ = std::fs::remove_file(&body_file);
        if status.is_ok_and(|s| s.success()) {
            return Ok(());
        }
        eprintln!("gh issue create failed; falling back to the browser");
    }
    let compact = serde_json::to_string(r).map_err(|e| e.to_string())?;
    let (url, prefilled) = submit::issue_url(&title, &compact);
    if !prefilled {
        match saved {
            Some(p) => println!(
                "The result is too long for a link: paste the contents of {} into the form's \
                 \"{}\" field.",
                p.display(),
                submit::LABEL_RESULT
            ),
            None => println!(
                "The result is too long for a link: paste the JSON above into the form's \"{}\" field.",
                submit::LABEL_RESULT
            ),
        }
    }
    println!("Open this link to file it:\n{url}");
    if crate::mcp::open_in_browser(&url).is_err() {
        println!("(could not open a browser; copy the link)");
    }
    Ok(())
}
