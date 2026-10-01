//! The "a newer release exists" notice (owner decision 2026-09-30,
//! `docs/audits/auto-update.md`; `docs/privacy.md` says what it sends).
//!
//! At most once a day, a person at a terminal running `neoscad` causes one
//! background fetch of the signed stable feed. When the feed names a newer
//! release, one line goes to stderr after the command's own output:
//!
//!     neoscad 0.3.0 is available (you have 0.2.0): https://github.com/…
//!
//! It never downloads or installs anything; package managers do the
//! updating. It stays silent unless both stdout and stderr are terminals,
//! in CI (`CI` set) and with `NEOSCAD_NO_UPDATE_CHECK` set, so scripts,
//! agents, pipes and the conformance harness never see it, and the
//! protocol commands (`serve`, `mcp`, `lsp`) never check.
//!
//! The fetch runs in a detached child process (this binary, started with
//! [`CHILD_ARG`]) that is never waited for, so it can't delay the command,
//! and it outlives a command that ends first, as `neoscad --version` does
//! (a thread would die with it and never finish a check). The result,
//! stored in the cache directory, is shown at the end of the next
//! interactive command, or of this one if it took longer than the fetch.
//! The feed's signature, channel and serial are checked by
//! `neoscad_client::update::check` (`crates/client`), the same code the
//! apps use.

use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use neoscad_client::update::{self, Channel};
use serde_json::{Value, json};

/// Set to anything to turn the check off.
pub const NO_CHECK_ENV: &str = "NEOSCAD_NO_UPDATE_CHECK";

/// Another directory to fetch `stable.json` and its `.minisig` from,
/// ending in `/`: for testing a feed (`scripts/release/test-update-feed.sh`).
/// The signature is still checked against the compiled-in keys, so this
/// can't make neoscad believe an unsigned file. Plain http is allowed only
/// to the loopback address.
pub const FEED_URL_ENV: &str = "NEOSCAD_UPDATE_FEED_URL";

/// The first argument of the background check's process. Not an OpenSCAD
/// input anyone would name a file.
pub const CHILD_ARG: &str = "__neoscad-update-check";

/// How often to look.
const INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Commands that never check: long-running servers whose stdio may be a
/// protocol, where an extra line on stderr is noise at best.
const QUIET_COMMANDS: &[&str] = &["serve", "mcp", "lsp"];

const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Where the notice's state lives, when this run may show it.
#[derive(Debug)]
pub struct Pending {
    path: PathBuf,
}

/// Starts the day's check when one is due, and returns what [`finish`]
/// needs; `None` when the notice is off for this run. `first_arg` is the
/// subcommand, if any.
pub fn start(first_arg: Option<&std::ffi::OsStr>) -> Option<Pending> {
    if std::env::var_os(NO_CHECK_ENV).is_some()
        || std::env::var_os("CI").is_some()
        || !std::io::stdout().is_terminal()
        || !std::io::stderr().is_terminal()
        || first_arg.is_some_and(|a| QUIET_COMMANDS.iter().any(|q| a == *q))
    {
        return None;
    }
    let path = crate::bench::cache_dir()?.join("update-check.json");
    let mut state = load(&path);
    let now = now();
    let last = state["checked"].as_u64().unwrap_or(0);
    // A last check "in the future" means the clock went back: check.
    if now < last || now - last >= INTERVAL.as_secs() {
        // Recorded before fetching, so a burst of commands (a shell loop)
        // starts one fetch, not one each; a failed fetch waits a day too.
        state["checked"] = json!(now);
        save(&path, &state);
        if let Ok(exe) = std::env::current_exe() {
            // Detached from the terminal: no output can reach it, and
            // nothing waits for it. curl's timeouts bound its life.
            let _ = Command::new(exe)
                .arg(CHILD_ARG)
                .arg(&path)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn();
        }
    }
    Some(Pending { path })
}

/// The background process: `neoscad __neoscad-update-check STATE_FILE`.
pub fn child(args: Vec<std::ffi::OsString>) -> u8 {
    if let Some(path) = args.first() {
        fetch_and_store(Path::new(path));
    }
    0
}

/// Prints the notice when a check found a newer release that hasn't been
/// shown yet.
pub fn finish(pending: Option<Pending>) {
    let Some(pending) = pending else {
        return;
    };
    let mut state = load(&pending.path);
    let notice = &state["notice"];
    if state["shown"] != json!(false) {
        return;
    }
    let (Some(version), Some(url)) = (notice["version"].as_str(), notice["url"].as_str()) else {
        return;
    };
    // The user may have upgraded since the check.
    if update::is_newer(version, VERSION).unwrap_or(false) {
        eprintln!("neoscad {version} is available (you have {VERSION}): {url}");
    }
    state["shown"] = json!(true);
    save(&pending.path, &state);
}

/// The fetch, in the background process: both files, checked, and the result
/// stored. Errors (offline, a feed between releases, a bad signature) are
/// silent: none of them is the user's problem.
fn fetch_and_store(path: &Path) {
    let base = std::env::var(FEED_URL_ENV).unwrap_or_else(|_| update::FEED_BASE_URL.into());
    let feed_url = Channel::Stable.feed_url(&base);
    let (Ok(feed), Ok(sig)) = (get(&feed_url), get(&format!("{feed_url}.minisig"))) else {
        return;
    };
    let mut state = load(path);
    let last_serial = state["serial"].as_u64();
    let Ok(checked) = update::check(&feed, &sig, VERSION, Channel::Stable, None, last_serial)
    else {
        return;
    };
    state["serial"] = json!(checked.serial);
    match checked.update {
        Some(u) => {
            // A notice already shown for this version is not repeated
            // until the next day's check finds it again.
            state["notice"] = json!({"version": u.version, "url": u.url});
            state["shown"] = json!(false);
        }
        None => {
            state["notice"] = Value::Null;
            state["shown"] = json!(true);
        }
    }
    save(path, &state);
}

/// One small file, quickly or not at all: a short timeout and no retry,
/// since nobody waits for it. The User-Agent is generic, without the
/// version or the OS (`docs/privacy.md`).
fn get(url: &str) -> Result<Vec<u8>, String> {
    let loopback = ["http://127.0.0.1:", "http://localhost:", "http://[::1]:"]
        .iter()
        .any(|p| url.starts_with(p));
    let proto = if loopback { "=http" } else { "=https" };
    crate::bench::curl(
        url,
        &[
            "--proto",
            proto,
            "--proto-redir",
            proto,
            "--connect-timeout",
            "3",
            "--max-time",
            "5",
            "--max-filesize",
            "65536",
            "-A",
            "neoscad",
        ],
    )
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

fn load(path: &Path) -> Value {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}))
}

/// Written to a temporary file and renamed, so a command reading it while
/// the background check writes it sees the old file or the new one, never
/// half of one.
fn save(path: &Path, state: &Value) {
    let Some(dir) = path.parent() else {
        return;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let tmp = dir.join(format!("update-check.{}.tmp", std::process::id()));
    if std::fs::write(&tmp, state.to_string()).is_ok() && std::fs::rename(&tmp, path).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}
