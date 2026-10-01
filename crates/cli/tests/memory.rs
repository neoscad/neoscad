//! The memory limit end to end, on programs that materialise small values
//! the estimate once missed (see `crates/eval/tests/memory_limit.rs`): a
//! one-shot `-o x.echo` run and `neoscad mcp`'s `render` each stop with a
//! `resource-limit` error, holding at most about three times the limit.
//!
//! Printing a shared list stops at the string limit the same way, under
//! the agent surface's default limits (no `--limit` at all).
//!
//! Each server or run is watched from here: its resident memory is sampled
//! every few milliseconds, and it is killed past 1 GB so a regression fails
//! the test instead of filling the machine's swap.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_neoscad");

/// The resident memory a run may reach under `--limit memory=64M`: three
/// times the limit, and the binary's own 30 MB or so.
const MAX_RSS_MB: u64 = 3 * 64 + 32;

/// Whether a watched peak is within `max`. Where memory can be read
/// ([`rss_mb`] finds this process's own), the peak must also be above
/// zero: a reader that found nothing would otherwise pass every bound.
fn resident_within(peak: u64, max: u64) -> bool {
    if rss_readable() {
        (1..=max).contains(&peak)
    } else {
        peak <= max
    }
}

/// Whether [`rss_mb`] can read memory here, probed on this test process.
/// Windows has no reader, and a build sandbox may have no `ps` (Nix on
/// macOS): there the limits' errors are checked but memory is not.
fn rss_readable() -> bool {
    static READABLE: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *READABLE.get_or_init(|| rss_mb(std::process::id()) > 0)
}

/// How long one `render` through MCP may take to stop at a limit, in the
/// debug build the tests run. Stopping takes work in proportion to the
/// limit, not to the value: printing up to 64 MiB takes about 2 s on an
/// Apple M-series core, and took 11 and 14 s on CI's Linux x86_64 runner
/// with the other tests of this file running beside it (a 10 s bound
/// failed there). A limit checked too late shows up as memory past
/// [`MAX_RSS_MB`] (a value built whole is hundreds of megabytes) or, for
/// the 2^40-element prints, as a walk that takes hours; this bound only
/// has to tell that apart from a slow machine.
const MAX_SECONDS: f64 = 60.0;

const TREE: &str = "function f(p, n) = n == 0 ? p : f([p, p], n - 1);\n";

fn programs() -> Vec<(&'static str, String)> {
    let tree = |s: &str| format!("{TREE}{s}");
    vec![
        ("negate", tree("echo(len(-f([1], 26)));")),
        // Inside a `let`: a top-level variable holding the tree would be
        // digested by the session's memo, which walks it whole (see
        // docs/followups.md).
        ("add", tree("echo(let (t = f([1], 26)) len(t + t));")),
        ("str", tree("echo(len(str(f([1], 26))));")),
        ("echo", tree("echo(f([1], 26));")),
        (
            "fuzzer",
            "function f2(p, n) = n == 0 ? p : f2([each false, p, for (i = [0:0]) p], n - 1);\n\
             echo(len(-f2([1], 26)));"
                .into(),
        ),
        (
            "small lists",
            "x = [for (i = [0:1999]) for (j = [0:1999]) [j]]; echo(len(x));".into(),
        ),
        (
            "short strings",
            "x = [for (i = [0:1999]) for (j = [0:1499]) str(\"item \", j)]; echo(len(x));".into(),
        ),
        (
            "function literals",
            "x = [for (i = [0:1999]) for (j = [0:1499]) function(y) y + j]; echo(len(x));".into(),
        ),
        ("echoes", "for (i = [0:1999], j = [0:1499]) echo(j);".into()),
    ]
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nsmem-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

/// `pid`'s resident memory (0 once it has exited): `VmRSS` from `/proc`
/// on Linux, `ps` elsewhere. Each watch samples every 5 ms and the tests
/// run side by side, so `ps` would be forked hundreds of times a second
/// on a CI runner with a few cores, taking CPU from the servers whose
/// response time is being measured.
///
/// Windows has neither (CI's Windows job runs these tests too), so there
/// every reading is 0: the limits' errors are checked, memory is not.
fn rss_mb(pid: u32) -> u64 {
    if cfg!(target_os = "linux") {
        return std::fs::read_to_string(format!("/proc/{pid}/status"))
            .ok()
            .and_then(|s| {
                let line = s.lines().find(|l| l.starts_with("VmRSS:"))?;
                line.split_whitespace().nth(1)?.parse::<u64>().ok()
            })
            .map_or(0, |kb| kb / 1024);
    }
    // By absolute path on macOS, where a build sandbox's PATH (Nix's) may
    // leave `ps` out although the system has it.
    let ps = if cfg!(target_os = "macos") {
        "/bin/ps"
    } else {
        "ps"
    };
    Command::new(ps)
        .args(["-o", "rss=", "-p", &pid.to_string()])
        .output()
        .ok()
        .and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<u64>()
                .ok()
        })
        .map_or(0, |kb| kb / 1024)
}

/// Samples a process's resident memory until dropped, keeping the peak,
/// and kills the process past 1 GB.
struct Watch {
    peak: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl Watch {
    fn new(pid: u32) -> Watch {
        let peak = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let thread = {
            let (peak, stop) = (peak.clone(), stop.clone());
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    let mb = rss_mb(pid);
                    peak.fetch_max(mb, Ordering::Relaxed);
                    if mb > 1024 {
                        let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
                        return;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
            })
        };
        Watch {
            peak,
            stop,
            thread: Some(thread),
        }
    }

    /// The peak so far, and start again from zero.
    fn take_peak(&self) -> u64 {
        self.peak.swap(0, Ordering::Relaxed)
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[test]
fn a_one_shot_run_stops_at_the_memory_limit() {
    let dir = scratch("cli");
    for (i, (name, src)) in programs().into_iter().enumerate() {
        let file = dir.join(format!("p{i}.scad"));
        let echo = dir.join(format!("p{i}.echo"));
        std::fs::write(&file, src).unwrap();
        let mut child = Command::new(BIN)
            .arg(&file)
            .arg("-o")
            .arg(&echo)
            .args(["--limit", "memory=64M"])
            .current_dir(&dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let watch = Watch::new(child.id());
        let status = child.wait().unwrap();
        let peak = watch.take_peak();
        drop(watch);
        let out = std::fs::read_to_string(&echo).unwrap_or_default();
        // An echo export exits 0 after an evaluation error, the limit's
        // included (as before this test); the error is in the file.
        assert!(status.code().is_some(), "{name}: {status:?}\n{out}");
        assert!(
            out.contains("ERROR: Resource limit exceeded")
                && out.contains("memory limit of 64 MiB"),
            "{name}: {out}"
        );
        assert!(
            resident_within(peak, MAX_RSS_MB),
            "{name}: {peak} MB resident"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Mcp {
    fn start(dir: &Path, args: &[&str]) -> Mcp {
        let mut child = Command::new(BIN)
            .arg("mcp")
            .args(args)
            .current_dir(dir)
            .env_remove("OPENSCADPATH")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Mcp {
            stdin: child.stdin.take().unwrap(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
            next: 1,
        }
    }

    fn tool(&mut self, name: &str, args: Value) -> Value {
        let id = self.next;
        self.next += 1;
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
                         "params": {"name": name, "arguments": args}});
        writeln!(self.stdin, "{msg}").unwrap();
        self.stdin.flush().unwrap();
        let mut line = String::new();
        assert!(
            self.stdout.read_line(&mut line).unwrap() > 0,
            "server closed"
        );
        let r: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(r["id"], id, "{r}");
        r["result"].clone()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn mcp_render_stops_at_the_memory_limit() {
    let dir = scratch("mcp");
    for (name, src) in programs() {
        // A fresh server each time: memory an allocator keeps after one
        // render would count against the next.
        let mut s = Mcp::start(&dir, &["--limit", "memory=64M"]);
        let watch = Watch::new(s.child.id());
        let t0 = std::time::Instant::now();
        let r = s.tool("render", json!({"source": src}));
        let peak = watch.take_peak();
        assert!(
            t0.elapsed().as_secs_f64() < MAX_SECONDS,
            "{name}: {:?}",
            t0.elapsed()
        );
        let st = &r["structuredContent"];
        assert_eq!(st["exit_code"], 1, "{name}: {r}");
        let d = &st["diagnostics"][0];
        assert_eq!(d["code"], "resource-limit", "{name}: {r}");
        assert!(
            d["message"]
                .as_str()
                .unwrap()
                .contains("memory limit of 64 MiB"),
            "{name}: {d}"
        );
        assert!(
            resident_within(peak, MAX_RSS_MB),
            "{name}: {peak} MB resident"
        );
        // The server carries on: a model under the limit renders.
        let r = s.tool("render", json!({"source": "cube(1);"}));
        assert_eq!(r["structuredContent"]["exit_code"], 0, "{name}: {r}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// A model with a geometry node, so the limit is checked at least once.
const SMALL: &str = "difference() { cube(2); sphere(1, $fn = 24); }\n";

/// Whether this platform has a probe (`src/memory.rs`).
const MEASURES: bool = cfg!(any(target_os = "macos", target_os = "linux", windows));

#[test]
fn a_one_shot_run_is_measured() {
    // Every process holds more than a megabyte, so a run under a 1 MiB
    // limit stops at its first check, by the measurement: the estimate
    // of this model is a few kilobytes. Without the limit, or under a
    // generous one, the output is the same file.
    let dir = scratch("measured");
    let file = dir.join("m.scad");
    std::fs::write(&file, SMALL).unwrap();
    let run = |limit: Option<&str>, out: &str| {
        let mut c = Command::new(BIN);
        c.arg(&file).arg("-o").arg(dir.join(out)).current_dir(&dir);
        if let Some(l) = limit {
            c.args(["--limit", l]);
        }
        c.output().unwrap()
    };
    let o = run(Some("memory=1M"), "low.stl");
    let err = String::from_utf8_lossy(&o.stderr);
    if MEASURES {
        assert_eq!(o.status.code(), Some(1), "{err}");
        assert!(
            err.contains("over the memory limit of 1 MiB (measured)"),
            "{err}"
        );
    }
    assert!(run(None, "none.stl").status.success());
    assert!(run(Some("memory=1G"), "high.stl").status.success());
    assert_eq!(
        std::fs::read(dir.join("none.stl")).unwrap(),
        std::fs::read(dir.join("high.stl")).unwrap()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn mcp_render_is_measured() {
    // `neoscad mcp` (and `serve`, whose session it runs) measures the
    // server process: under 1 MiB nothing renders, and under the agent
    // limits (4 GiB) the same model does.
    let dir = scratch("mcp-measured");
    let mut s = Mcp::start(&dir, &["--limit", "memory=1M"]);
    let r = s.tool("render", json!({"source": SMALL}));
    let st = &r["structuredContent"];
    if MEASURES {
        assert_eq!(st["exit_code"], 1, "{r}");
        let d = &st["diagnostics"][0];
        assert_eq!(d["code"], "resource-limit", "{r}");
        assert!(d["message"].as_str().unwrap().contains("(measured)"), "{d}");
    }
    let mut s = Mcp::start(&dir, &[]);
    let r = s.tool("render", json!({"source": SMALL}));
    assert_eq!(r["structuredContent"]["exit_code"], 0, "{r}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// Values that print as 2^40 elements from a few lists: printing them
/// stops at the 64 MiB string limit (the audit's `echo(str(t(40)))` built
/// 2 GB of text in 3.3 s through MCP before the limit was checked).
fn printing_programs() -> Vec<(&'static str, String, &'static str)> {
    let t = "function t(n) = n == 0 ? [1] : let (c = t(n - 1)) [c, c];\n";
    vec![
        ("str", format!("{t}echo(str(t(40)));"), "str()"),
        ("echo", format!("{t}echo(t(40));"), "echo()"),
        ("assert", format!("{t}assert(false, t(40));"), "a message"),
    ]
}

/// What printing may hold: the 64 MiB text (and the vector doubling it
/// grows by), besides the binary's own.
const MAX_PRINT_RSS_MB: u64 = 400;

#[test]
fn mcp_printing_stops_at_the_string_limit() {
    let dir = scratch("mcp-print");
    for (name, src, what) in printing_programs() {
        let mut s = Mcp::start(&dir, &[]);
        let watch = Watch::new(s.child.id());
        let t0 = std::time::Instant::now();
        let r = s.tool("render", json!({"source": src}));
        let peak = watch.take_peak();
        assert!(
            t0.elapsed().as_secs_f64() < MAX_SECONDS,
            "{name}: {:?}",
            t0.elapsed()
        );
        let d = &r["structuredContent"]["diagnostics"][0];
        assert_eq!(d["code"], "resource-limit", "{name}: {r}");
        let msg = d["message"].as_str().unwrap();
        assert!(
            msg.contains(&format!("{what} would make at least"))
                && msg.contains("over the string limit of 67,108,864"),
            "{name}: {d}"
        );
        assert!(
            resident_within(peak, MAX_PRINT_RSS_MB),
            "{name}: {peak} MB resident"
        );
        let r = s.tool("render", json!({"source": "echo(str([1, 2]));"}));
        assert_eq!(r["structuredContent"]["exit_code"], 0, "{name}: {r}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_one_shot_run_stops_printing_at_the_string_limit() {
    let dir = scratch("cli-print");
    for (i, (name, src, what)) in printing_programs().into_iter().enumerate() {
        let file = dir.join(format!("p{i}.scad"));
        let echo = dir.join(format!("p{i}.echo"));
        std::fs::write(&file, src).unwrap();
        // A one-shot run has no limits unless asked, as OpenSCAD has none.
        let mut child = Command::new(BIN)
            .arg(&file)
            .arg("-o")
            .arg(&echo)
            .args(["--limit", "string=67108864"])
            .current_dir(&dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let watch = Watch::new(child.id());
        let status = child.wait().unwrap();
        let peak = watch.take_peak();
        drop(watch);
        let out = std::fs::read_to_string(&echo).unwrap_or_default();
        assert!(status.code().is_some(), "{name}: {status:?}\n{out}");
        assert!(
            out.starts_with(&format!(
                "ERROR: Resource limit exceeded: {what} would make at least"
            )) && out.contains("over the string limit of 67,108,864"),
            "{name}: {out}"
        );
        assert!(
            resident_within(peak, MAX_PRINT_RSS_MB),
            "{name}: {peak} MB resident"
        );
    }
    let _ = std::fs::remove_dir_all(&dir);
}
