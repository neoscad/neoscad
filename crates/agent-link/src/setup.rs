//! Connecting an AI agent client, for the apps' "Connect your AI agent"
//! sheets (`docs/mcp.md`, "Setup from the apps";
//! `docs/audits/agent-connection-desktop.md`, Option A): what needs the
//! machine, which `client::agent_setup` (the rows, as data) may not touch.
//! That is the environment (home, the Documents folder, `PATH`, the
//! Flatpak sandbox), finding and running `claude`, and editing Claude
//! Desktop's config file with a backup and making its project folder.
//!
//! Shared by every app: the macOS and Windows apps reach it through
//! `crates/ffi/src/agent_setup.rs` (UniFFI), and the Linux app calls it
//! directly. Keeping one copy matters most for the process and file work:
//! a timeout, a backup name or the "already exists" test that differed
//! between apps would be a bug in only one of them.
//!
//! Every function here acts at once. Asking first is the app's part: the
//! sheet shows the exact command or change, and Claude Desktop's config is
//! written only after the user agreed to it (the owner's decision,
//! 2026-10-02). Cursor and VS Code need nothing here: the app opens the
//! row's link, and the client asks the user itself.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use client::agent_setup as setup;

/// How long `claude mcp add` (or `remove`) may take before it is stopped:
/// it writes one file, so a minute means it is waiting on something.
const CLAUDE_LIMIT: Duration = Duration::from_secs(60);
/// How long the login shell may take to say where `claude` is: start-up
/// files that hang (a prompt waiting for input) must not hang the sheet.
const SHELL_LIMIT: Duration = Duration::from_secs(5);

/// What [`add_to_claude_code`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeCodeOutcome {
    /// Added (or replaced) for every project.
    Added { output: String },
    /// There is already a user-scope `neoscad`; ask the user, then call
    /// again with `replace`.
    AlreadyExists { output: String },
    /// `claude` failed or did not finish; show `output` and the copy text.
    Failed { output: String },
}

/// What [`add_to_claude_desktop`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeDesktopOutcome {
    /// Written; Claude must be quit and reopened to load it. `backup` is
    /// the previous file's copy, `None` when there was no file.
    Written {
        path: String,
        backup: Option<String>,
        replaced_entry: bool,
    },
    /// The file already runs this command; nothing was written.
    Unchanged { path: String },
    /// Claude Desktop's directory does not exist, so it is not installed
    /// (or never started) for this user.
    NotInstalled { path: String },
    /// The file is not JSON this understands (comments, a trailing comma,
    /// another shape); it was left alone. Show the reason and the copy
    /// text.
    Refused { path: String, reason: String },
}

/// What [`claude_desktop_status`] found, before the user clicks anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaudeDesktopStatus {
    /// Claude Desktop's directory does not exist.
    NotInstalled { path: String },
    /// No `neoscad` server in its config (or no config).
    NotAdded { path: String },
    /// It runs this app's server with its folder, and the folder exists.
    UpToDate { path: String },
    /// It runs this app's `neoscad` with other arguments (a setup from
    /// before `--root`, whose agent cannot export), or its folder is
    /// gone. [`add_to_claude_desktop`] brings it up to date, after the
    /// user agreed, with a backup.
    Outdated { path: String },
    /// It has a `neoscad` server running another program, one the user
    /// set up themselves; adding replaces it, after the user agreed.
    Other { path: String },
    /// The config cannot be read as JSON this understands.
    Unreadable { path: String, reason: String },
}

/// Where this process runs. `LinuxFlatpak` inside a Flatpak sandbox:
/// `/.flatpak-info` exists there, and only there, whatever the
/// environment says.
pub fn host() -> setup::Host {
    if cfg!(target_os = "macos") {
        setup::Host::MacOs
    } else if cfg!(windows) {
        setup::Host::Windows
    } else if Path::new("/.flatpak-info").exists() {
        setup::Host::LinuxFlatpak
    } else {
        setup::Host::Linux
    }
}

/// The sheet's rows for this host, with `cli` (the app's absolute
/// `neoscad`) as the server's command; inside the Flatpak, `flatpak run`
/// whatever `cli` is.
pub fn rows(cli: &str) -> Vec<setup::ClientSetup> {
    let host = host();
    let server = setup::ServerCommand::for_host(host, cli);
    let home = home();
    setup::setups_in(host, &server, &home, appdata().as_deref(), &documents())
}

/// The `claude` to run, or `None`: the usual install locations and this
/// process's `PATH` (`client::agent_setup::claude_code_candidates`), and
/// on macOS and Linux then the user's login shell, which sees the `PATH`
/// their terminal has (`$SHELL -lc 'command -v claude'`, given 5 s). It
/// blocks for up to that long; call it off the main thread. Always `None`
/// in the Flatpak, which runs no host program.
pub fn find_claude() -> Option<String> {
    let host = host();
    if !host.can_run_host_programs() {
        return None;
    }
    let path = std::env::var("PATH").unwrap_or_default();
    let found = setup::claude_code_candidates(host, &home(), &path, appdata().as_deref())
        .into_iter()
        .find(|p| is_executable(Path::new(p)));
    if found.is_some() || cfg!(windows) {
        return found;
    }
    // Without `SHELL`, each system's default login shell.
    let fallback = if cfg!(target_os = "macos") {
        "/bin/zsh"
    } else {
        "/bin/sh"
    };
    let shell = std::env::var("SHELL").unwrap_or_else(|_| fallback.to_string());
    let mut cmd = Command::new(shell);
    cmd.args(["-lc", "command -v claude"]);
    let out = run(cmd, SHELL_LIMIT).ok()?;
    let line = out.output.lines().last()?.trim().to_string();
    (out.success && line.starts_with('/') && is_executable(Path::new(&line))).then_some(line)
}

/// Runs `claude mcp add --scope user neoscad -- <server>` with `claude`
/// (from [`find_claude`]). With `replace`, removes the user-scope
/// `neoscad` first (the add refuses an existing name). Blocks for as long
/// as `claude` takes (up to 60 s); call it off the main thread.
pub fn add_to_claude_code(
    claude: &Path,
    server: &setup::ServerCommand,
    replace: bool,
) -> ClaudeCodeOutcome {
    if replace {
        // A failure here (no entry to remove) shows in the add's result.
        let _ = run(
            claude_command(claude, &setup::claude_code_remove_args()),
            CLAUDE_LIMIT,
        );
    }
    match run(
        claude_command(claude, &setup::claude_code_add_args(server)),
        CLAUDE_LIMIT,
    ) {
        Ok(r) if r.success => ClaudeCodeOutcome::Added { output: r.output },
        Ok(r) if setup::claude_code_reports_existing(&r.output) => {
            ClaudeCodeOutcome::AlreadyExists { output: r.output }
        }
        Ok(r) => ClaudeCodeOutcome::Failed { output: r.output },
        Err(e) => ClaudeCodeOutcome::Failed {
            output: format!("could not run {}: {e}", claude.display()),
        },
    }
}

/// `claude` with `args`, its own directory first on `PATH`: an npm
/// install's launcher may look for its siblings there, and a GUI app's
/// `PATH` holds none of the user's directories.
fn claude_command(claude: &Path, args: &[String]) -> Command {
    let mut cmd = Command::new(claude);
    cmd.args(args);
    if let Some(dir) = claude.parent() {
        let mut paths = vec![dir.to_path_buf()];
        if let Some(p) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&p));
        }
        if let Ok(joined) = std::env::join_paths(paths) {
            cmd.env("PATH", joined);
        }
    }
    cmd
}

/// Claude Desktop's config file on this host (macOS and Windows), or
/// `None` where Claude Desktop does not run.
pub fn claude_desktop_config() -> Option<String> {
    setup::claude_desktop_config_path(host(), &home(), appdata().as_deref())
}

/// The folder Claude Desktop's server gets as its `--root`
/// (`client::agent_setup::claude_desktop_folder`): `NeoSCAD` in the
/// user's Documents folder ([`documents`]); `None` where Claude Desktop
/// does not run.
pub fn claude_desktop_folder() -> Option<String> {
    setup::claude_desktop_folder(host(), &documents())
}

/// The server Claude Desktop is given: `cli` serving MCP with
/// [`claude_desktop_folder`] as its root, as the row's JSON shows.
pub fn claude_desktop_server(cli: &str) -> Option<setup::ServerCommand> {
    let host = host();
    setup::claude_desktop_server(
        host,
        &setup::ServerCommand::for_host(host, cli),
        &documents(),
    )
}

/// What Claude Desktop's config at `path` has, against `server` (from
/// [`claude_desktop_server`]) and its project folder `folder`; read
/// only, for the sheet to show before the user clicks.
pub fn claude_desktop_status(
    path: &Path,
    server: &setup::ServerCommand,
    folder: &Path,
) -> ClaudeDesktopStatus {
    let shown = path.display().to_string();
    if !path.parent().is_some_and(Path::is_dir) {
        return ClaudeDesktopStatus::NotInstalled { path: shown };
    }
    let text = match std::fs::read(path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(t) => Some(t),
            Err(_) => {
                return ClaudeDesktopStatus::Unreadable {
                    path: shown,
                    reason: "the file is not UTF-8 text".to_string(),
                };
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            return ClaudeDesktopStatus::Unreadable {
                path: shown,
                reason: e.to_string(),
            };
        }
    };
    match setup::claude_desktop_entry(text.as_deref(), server) {
        Ok(setup::ClaudeDesktopEntry::Missing) => ClaudeDesktopStatus::NotAdded { path: shown },
        // `neoscad mcp` drops a `--root` that does not exist, so a
        // deleted folder leaves the agent unable to export again: adding
        // once more makes it.
        Ok(setup::ClaudeDesktopEntry::Current) if folder.is_dir() => {
            ClaudeDesktopStatus::UpToDate { path: shown }
        }
        Ok(setup::ClaudeDesktopEntry::Current | setup::ClaudeDesktopEntry::Outdated) => {
            ClaudeDesktopStatus::Outdated { path: shown }
        }
        Ok(setup::ClaudeDesktopEntry::Other) => ClaudeDesktopStatus::Other { path: shown },
        Err(e) => ClaudeDesktopStatus::Unreadable {
            path: shown,
            reason: e.to_string(),
        },
    }
}

/// Adds the server to Claude Desktop's config at `path`, after the user
/// agreed: `client::agent_setup::merge_claude_desktop_config` on the
/// file's text, a backup beside it, then an atomic replace. Refuses a
/// file it cannot read as JSON rather than rewrite it. `folder`, the
/// server's `--root` ([`claude_desktop_folder`]), is created when it is
/// missing, even if the config already had the entry: `neoscad mcp`
/// ignores a root that does not exist, which would leave the agent with
/// nowhere to write. `unix_seconds` names the backup ([`now`] outside
/// the tests).
pub fn add_to_claude_desktop(
    path: &Path,
    server: &setup::ServerCommand,
    folder: &Path,
    unix_seconds: i64,
) -> std::io::Result<ClaudeDesktopOutcome> {
    let shown = path.display().to_string();
    let Some(dir) = path.parent().filter(|d| d.is_dir()) else {
        return Ok(ClaudeDesktopOutcome::NotInstalled { path: shown });
    };
    // A config kept in a dotfiles repository is often a link: edit the
    // file it points to, and keep the link.
    let target = match std::fs::canonicalize(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => path.to_path_buf(),
        Err(e) => return Err(e),
    };
    let existing = match std::fs::read(&target) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e),
    };
    let text = match existing.as_deref().map(std::str::from_utf8).transpose() {
        Ok(t) => t,
        Err(_) => {
            return Ok(ClaudeDesktopOutcome::Refused {
                path: shown,
                reason: "the file is not UTF-8 text".to_string(),
            });
        }
    };
    let merged = match setup::merge_claude_desktop_config(text, server) {
        Ok(m) => m,
        Err(e) => {
            return Ok(ClaudeDesktopOutcome::Refused {
                path: shown,
                reason: e.to_string(),
            });
        }
    };
    // Before the config: a config naming a folder that could not be made
    // would look set up and still refuse every export.
    std::fs::create_dir_all(folder).map_err(|e| {
        std::io::Error::new(
            e.kind(),
            format!("could not create {}: {e}", folder.display()),
        )
    })?;
    if merged.change == setup::ConfigChange::Unchanged {
        return Ok(ClaudeDesktopOutcome::Unchanged { path: shown });
    }
    let target_dir = target.parent().unwrap_or(dir);
    let backup = match &existing {
        Some(bytes) => Some(write_backup(target_dir, &target, bytes, unix_seconds)?),
        None => None,
    };
    write_atomically(&target, merged.text.as_bytes())?;
    Ok(ClaudeDesktopOutcome::Written {
        path: shown,
        backup: backup.map(|b| b.display().to_string()),
        replaced_entry: merged.change == setup::ConfigChange::Replaced,
    })
}

/// The previous file, byte for byte, under a name no earlier backup has
/// (`create_new`, so a backup is never overwritten).
fn write_backup(
    dir: &Path,
    original: &Path,
    bytes: &[u8],
    unix_seconds: i64,
) -> std::io::Result<PathBuf> {
    let name = original
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "claude_desktop_config.json".to_string());
    for attempt in 0..100 {
        let backup = dir.join(setup::backup_file_name(&name, unix_seconds, attempt));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&backup)
        {
            Ok(mut f) => {
                f.write_all(bytes)?;
                f.sync_all()?;
                return Ok(backup);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    }
    Err(std::io::Error::other("no free backup name"))
}

/// Write beside the file, then rename over it, so Claude never reads half
/// a file and a full disk leaves the old one. The old file's permissions
/// are kept.
fn write_atomically(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let tmp = dir.join(format!(".{name}.neoscad-{}", std::process::id()));
    let result = (|| {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        if let Ok(meta) = std::fs::metadata(path) {
            std::fs::set_permissions(&tmp, meta.permissions())?;
        }
        std::fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

// --- The machine -------------------------------------------------------------

/// The user's home directory (`USERPROFILE` on Windows), or empty.
pub fn home() -> String {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var(var).unwrap_or_default()
}

/// The user's Documents folder: on Windows the shell's known folder
/// (`FOLDERID_Documents`, through `dirs`, as `lang::loader::LibraryPath`
/// finds OpenSCAD's user library), which OneDrive's backup redirects away
/// from `%USERPROFILE%\Documents`; elsewhere `~/Documents`
/// (`client::agent_setup::default_documents`).
pub fn documents() -> String {
    #[cfg(windows)]
    if let Some(d) = dirs::document_dir() {
        return d.display().to_string();
    }
    setup::default_documents(host(), &home())
}

/// `%APPDATA%` on Windows (unset elsewhere).
pub fn appdata() -> Option<String> {
    std::env::var("APPDATA").ok().filter(|a| !a.is_empty())
}

/// Seconds since 1970, for a backup's name.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

/// A regular file this user may run (on Windows, any file).
pub fn is_executable(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.is_file() && meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        meta.is_file()
    }
}

struct Ran {
    success: bool,
    /// Standard output, then standard error.
    output: String,
}

/// Runs `cmd` with no input, collecting its output, and kills it after
/// `limit`: a login shell's start-up files or a `claude` waiting on
/// something must not hang the app's sheet.
fn run(mut cmd: Command, limit: Duration) -> std::io::Result<Ran> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let (Some(mut stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(std::io::Error::other("no pipes to the child"));
    };
    // Read on threads, so a chatty child cannot fill a pipe and stall.
    let out = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = stdout.read_to_end(&mut s);
        s
    });
    let err = std::thread::spawn(move || {
        let mut s = Vec::new();
        let _ = stderr.read_to_end(&mut s);
        s
    });
    let start = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait()? {
            break Some(s);
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut output = String::from_utf8_lossy(&out.join().unwrap_or_default()).into_owned();
    output.push_str(&String::from_utf8_lossy(&err.join().unwrap_or_default()));
    Ok(match status {
        Some(s) => Ran {
            success: s.success(),
            output,
        },
        None => Ran {
            success: false,
            output: format!("{output}\n(stopped after {} s)", limit.as_secs()),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("neoscad-agent-setup-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn server() -> setup::ServerCommand {
        setup::ServerCommand::bundled("/Apps/NeoSCAD/bin/neoscad")
    }

    /// A project folder for Claude Desktop in `dir`'s home.
    fn folder(dir: &Path) -> PathBuf {
        dir.join("home/Documents/NeoSCAD")
    }

    #[test]
    fn claude_desktop_config_is_backed_up_then_replaced() {
        let dir = scratch("desktop");
        let config = dir.join("Claude/claude_desktop_config.json");
        // No Claude directory: not installed, nothing created.
        assert_eq!(
            add_to_claude_desktop(&config, &server(), &folder(&dir), 0).unwrap(),
            ClaudeDesktopOutcome::NotInstalled {
                path: config.display().to_string()
            }
        );
        assert!(!config.parent().unwrap().exists());
        assert!(!folder(&dir).exists());

        // A directory with no file: written, no backup.
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        let r = add_to_claude_desktop(&config, &server(), &folder(&dir), 0).unwrap();
        assert!(
            matches!(
                r,
                ClaudeDesktopOutcome::Written {
                    backup: None,
                    replaced_entry: false,
                    ..
                }
            ),
            "{r:?}"
        );
        let written = std::fs::read_to_string(&config).unwrap();
        assert!(written.contains("/Apps/NeoSCAD/bin/neoscad"));
        // The project folder is made with its parents (a home with no
        // Documents yet).
        assert!(folder(&dir).is_dir());

        // The same again: unchanged, nothing new on disk.
        let r = add_to_claude_desktop(&config, &server(), &folder(&dir), 1).unwrap();
        assert!(matches!(r, ClaudeDesktopOutcome::Unchanged { .. }), "{r:?}");
        assert_eq!(
            std::fs::read_dir(config.parent().unwrap()).unwrap().count(),
            1
        );

        // Another server and an old entry: backed up byte for byte, twice
        // in one second gets two backups.
        let old = "{\"mcpServers\": {\"x\": {\"command\": \"y\"}, \"neoscad\": {\"command\": \"neoscad\"}}}";
        std::fs::write(&config, old).unwrap();
        let ClaudeDesktopOutcome::Written {
            backup: Some(b1),
            replaced_entry: true,
            ..
        } = add_to_claude_desktop(&config, &server(), &folder(&dir), 1_791_037_805).unwrap()
        else {
            panic!()
        };
        assert!(
            b1.ends_with("claude_desktop_config.json.neoscad-backup-20261003T143005Z"),
            "{b1}"
        );
        assert_eq!(std::fs::read_to_string(&b1).unwrap(), old);
        let now = std::fs::read_to_string(&config).unwrap();
        assert!(
            now.contains("\"x\"") && now.contains("/Apps/NeoSCAD/bin/neoscad"),
            "{now}"
        );
        std::fs::write(&config, old).unwrap();
        let ClaudeDesktopOutcome::Written {
            backup: Some(b2), ..
        } = add_to_claude_desktop(&config, &server(), &folder(&dir), 1_791_037_805).unwrap()
        else {
            panic!()
        };
        assert!(b2.ends_with("-20261003T143005Z-1"), "{b2}");
        assert_eq!(std::fs::read_to_string(&b1).unwrap(), old);

        // Unchanged, but the folder was deleted: made again.
        std::fs::remove_dir_all(dir.join("home")).unwrap();
        let r = add_to_claude_desktop(&config, &server(), &folder(&dir), 6).unwrap();
        assert!(matches!(r, ClaudeDesktopOutcome::Unchanged { .. }), "{r:?}");
        assert!(folder(&dir).is_dir());

        // Not JSON: refused, and the file is exactly as it was, no backup.
        let bad = "{\n  // mine\n  \"mcpServers\": {}\n}\n";
        std::fs::write(&config, bad).unwrap();
        let before = std::fs::read_dir(config.parent().unwrap()).unwrap().count();
        let r = add_to_claude_desktop(&config, &server(), &folder(&dir), 5).unwrap();
        assert!(
            matches!(&r, ClaudeDesktopOutcome::Refused { reason, .. } if reason.contains("line 2")),
            "{r:?}"
        );
        assert_eq!(std::fs::read_to_string(&config).unwrap(), bad);
        assert_eq!(
            std::fs::read_dir(config.parent().unwrap()).unwrap().count(),
            before
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_config_is_edited_through_its_link() {
        let dir = scratch("link");
        let real = dir.join("dotfiles/claude.json");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::write(&real, "{}").unwrap();
        let config = dir.join("Claude/claude_desktop_config.json");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real, &config).unwrap();
        let r = add_to_claude_desktop(&config, &server(), &dir.join("NeoSCAD"), 0).unwrap();
        assert!(
            matches!(
                r,
                ClaudeDesktopOutcome::Written {
                    backup: Some(_),
                    ..
                }
            ),
            "{r:?}"
        );
        assert!(
            std::fs::symlink_metadata(&config)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert!(std::fs::read_to_string(&real).unwrap().contains("neoscad"));
        // The backup sits beside the real file.
        assert!(std::fs::read_dir(real.parent().unwrap()).unwrap().count() == 2);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// An entry from before the setup passed `--root`: found as outdated,
    /// then brought up to date with a backup, its folder made, and found
    /// up to date; a deleted folder makes it outdated again.
    #[test]
    fn an_old_claude_desktop_entry_is_found_and_updated() {
        let dir = scratch("upgrade");
        let config = dir.join("Claude/claude_desktop_config.json");
        let folder = folder(&dir);
        let wanted = server().with_root(&folder.display().to_string());
        let status = |s: &setup::ServerCommand| claude_desktop_status(&config, s, &folder);
        let shown = config.display().to_string();
        assert_eq!(
            status(&wanted),
            ClaudeDesktopStatus::NotInstalled {
                path: shown.clone()
            }
        );
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        assert_eq!(
            status(&wanted),
            ClaudeDesktopStatus::NotAdded {
                path: shown.clone()
            }
        );

        // The entry the setup wrote before it passed --root.
        add_to_claude_desktop(&config, &server(), &dir.join("unused"), 0).unwrap();
        let old = std::fs::read_to_string(&config).unwrap();
        assert_eq!(
            status(&wanted),
            ClaudeDesktopStatus::Outdated {
                path: shown.clone()
            }
        );
        let r = add_to_claude_desktop(&config, &wanted, &folder, 1_791_037_805).unwrap();
        let ClaudeDesktopOutcome::Written {
            backup: Some(backup),
            replaced_entry: true,
            ..
        } = r
        else {
            panic!("{r:?}")
        };
        assert_eq!(std::fs::read_to_string(&backup).unwrap(), old);
        let now: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        assert_eq!(
            now["mcpServers"]["neoscad"]["args"],
            serde_json::json!(["mcp", "--root", folder.display().to_string()])
        );
        assert!(folder.is_dir());
        assert_eq!(
            status(&wanted),
            ClaudeDesktopStatus::UpToDate {
                path: shown.clone()
            }
        );
        std::fs::remove_dir(&folder).unwrap();
        assert_eq!(
            status(&wanted),
            ClaudeDesktopStatus::Outdated {
                path: shown.clone()
            }
        );

        // Another program under the name is the user's own; a file that
        // is not JSON is unreadable.
        std::fs::write(
            &config,
            r#"{"mcpServers": {"neoscad": {"command": "neoscad"}}}"#,
        )
        .unwrap();
        assert_eq!(
            status(&wanted),
            ClaudeDesktopStatus::Other {
                path: shown.clone()
            }
        );
        std::fs::write(&config, "{,}").unwrap();
        assert!(matches!(
            status(&wanted),
            ClaudeDesktopStatus::Unreadable { .. }
        ));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The rows and the server Claude Desktop is given agree, in this
    /// user's Documents folder.
    #[test]
    fn claude_desktops_row_names_its_folder() {
        let Some(folder) = claude_desktop_folder() else {
            // Only Linux has no Claude Desktop.
            assert!(matches!(
                host(),
                setup::Host::Linux | setup::Host::LinuxFlatpak
            ));
            return;
        };
        assert!(folder.ends_with("NeoSCAD"), "{folder}");
        let server = claude_desktop_server("/Apps/neoscad").unwrap();
        assert_eq!(server.args, ["mcp", "--root", folder.as_str()]);
        let row = rows("/Apps/neoscad")
            .into_iter()
            .find(|r| r.client == setup::Client::ClaudeDesktop)
            .unwrap();
        let json: serde_json::Value = serde_json::from_str(&row.copy_text).unwrap();
        assert_eq!(
            json["mcpServers"]["neoscad"]["args"],
            serde_json::json!(server.args)
        );
    }

    /// A stand-in `claude` that keeps its entries in a file and answers
    /// as Claude Code 2.1.288 does.
    #[cfg(unix)]
    #[test]
    fn claude_code_is_added_then_replaced_on_request() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("claude");
        let claude = dir.join("claude");
        let state = dir.join("state");
        std::fs::write(
            &claude,
            format!(
                "#!/bin/sh\necho \"$@\" >> '{log}'\ncase \"$2\" in\n\
                 add) if [ -f '{s}' ]; then echo \"MCP server $5 already exists in user config\"; exit 1; fi; touch '{s}'; echo \"Added stdio MCP server $5\";;\n\
                 remove) rm -f '{s}'; echo \"Removed MCP server $5\";;\nesac\n",
                log = dir.join("log").display(),
                s = state.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();

        let r = add_to_claude_code(&claude, &server(), false);
        assert!(
            matches!(&r, ClaudeCodeOutcome::Added { output } if output.contains("Added")),
            "{r:?}"
        );
        let r = add_to_claude_code(&claude, &server(), false);
        assert!(
            matches!(r, ClaudeCodeOutcome::AlreadyExists { .. }),
            "{r:?}"
        );
        let r = add_to_claude_code(&claude, &server(), true);
        assert!(matches!(r, ClaudeCodeOutcome::Added { .. }), "{r:?}");
        let log = std::fs::read_to_string(dir.join("log")).unwrap();
        assert_eq!(
            log.lines().collect::<Vec<_>>(),
            [
                "mcp add --scope user neoscad -- /Apps/NeoSCAD/bin/neoscad mcp",
                "mcp add --scope user neoscad -- /Apps/NeoSCAD/bin/neoscad mcp",
                "mcp remove --scope user neoscad",
                "mcp add --scope user neoscad -- /Apps/NeoSCAD/bin/neoscad mcp",
            ]
        );
        let r = add_to_claude_code(&dir.join("missing"), &server(), false);
        assert!(matches!(r, ClaudeCodeOutcome::Failed { .. }), "{r:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_hung_program_is_stopped() {
        let mut cmd = Command::new("/bin/sleep");
        cmd.arg("30");
        let start = Instant::now();
        let r = run(cmd, Duration::from_millis(200)).unwrap();
        assert!(!r.success && r.output.contains("stopped after"));
        assert!(start.elapsed() < Duration::from_secs(5));
    }
}
