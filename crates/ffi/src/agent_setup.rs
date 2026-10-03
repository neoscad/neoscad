//! Connecting an AI agent client, for the apps' "Connect your AI agent"
//! sheet (`docs/mcp.md`, "Setup from the apps";
//! `docs/audits/agent-connection-desktop.md`, Option A). The rows (what
//! each client needs, as data) come from `client::agent_setup`; this adds
//! what needs the machine, which a library crate may not touch: the
//! environment (home, `PATH`), finding and running `claude`, and editing
//! Claude Desktop's config file with a backup.
//!
//! Every function here acts at once. Asking first is the app's part: the
//! sheet shows the exact command or change, and Claude Desktop's config is
//! written only after the user agreed to it (the owner's decision,
//! 2026-10-02). Cursor and VS Code need nothing here: the app opens the
//! row's link, and the client asks the user itself.
//!
//! The Linux app does not link this crate. It reads the same rows from
//! `client::agent_setup` directly; its Flatpak runs nothing on the host.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use client::agent_setup as setup;

use crate::{CoreError, guarded};

/// Where the app runs (`client::agent_setup::Host`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AgentSetupHost {
    MacOs,
    Windows,
    Linux,
    LinuxFlatpak,
}

impl From<AgentSetupHost> for setup::Host {
    fn from(h: AgentSetupHost) -> Self {
        match h {
            AgentSetupHost::MacOs => setup::Host::MacOs,
            AgentSetupHost::Windows => setup::Host::Windows,
            AgentSetupHost::Linux => setup::Host::Linux,
            AgentSetupHost::LinuxFlatpak => setup::Host::LinuxFlatpak,
        }
    }
}

/// An agent client the sheet lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AgentSetupClient {
    ClaudeCode,
    ClaudeDesktop,
    Cursor,
    VsCode,
    Other,
}

impl From<setup::Client> for AgentSetupClient {
    fn from(c: setup::Client) -> Self {
        match c {
            setup::Client::ClaudeCode => AgentSetupClient::ClaudeCode,
            setup::Client::ClaudeDesktop => AgentSetupClient::ClaudeDesktop,
            setup::Client::Cursor => AgentSetupClient::Cursor,
            setup::Client::VsCode => AgentSetupClient::VsCode,
            setup::Client::Other => AgentSetupClient::Other,
        }
    }
}

/// What a row's button does.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum AgentSetupAction {
    /// Call [`agent_setup_add_to_claude_code`] (after
    /// [`agent_setup_find_claude`] found it; else show the copy text).
    RunClaude,
    /// Open the link with the system (`NSWorkspace.open`,
    /// `Launcher.LaunchUriAsync`); the client asks the user.
    OpenUrl { url: String },
    /// After the user agrees, call [`agent_setup_add_to_claude_desktop`];
    /// `path` is the file it edits, to name in the question.
    MergeConfig { path: String },
    /// Only the copy text.
    CopyOnly,
}

/// One client's row in the sheet.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AgentSetupRow {
    pub client: AgentSetupClient,
    pub label: String,
    pub action: AgentSetupAction,
    /// The command or JSON that does the same by hand.
    pub copy_text: String,
    pub note: String,
}

/// What [`agent_setup_add_to_claude_code`] did.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum ClaudeCodeOutcome {
    /// Added (or replaced) for every project.
    Added { output: String },
    /// There is already a user-scope `neoscad`; ask the user, then call
    /// again with `replace`.
    AlreadyExists { output: String },
    /// `claude` failed or did not finish; show `output` and the copy text.
    Failed { output: String },
}

/// What [`agent_setup_add_to_claude_desktop`] did.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
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

/// The host this core was built for. `LinuxFlatpak` inside a Flatpak
/// sandbox (`/.flatpak-info` exists there, and only there).
#[uniffi::export]
pub fn agent_setup_host() -> AgentSetupHost {
    if cfg!(target_os = "macos") {
        AgentSetupHost::MacOs
    } else if cfg!(windows) {
        AgentSetupHost::Windows
    } else if Path::new("/.flatpak-info").exists() {
        AgentSetupHost::LinuxFlatpak
    } else {
        AgentSetupHost::Linux
    }
}

/// The sheet's rows for this host, with `cli` (the app's absolute
/// `neoscad`: the stable link on macOS, `bin\neoscad.exe` in the install
/// folder on Windows) as the server's command.
#[uniffi::export]
pub fn agent_setup_rows(cli: String) -> Vec<AgentSetupRow> {
    let host = agent_setup_host().into();
    let server = setup::ServerCommand::for_host(host, &cli);
    setup::setups(host, &server, &home(), appdata().as_deref())
        .into_iter()
        .map(|r| AgentSetupRow {
            client: r.client.into(),
            label: r.label,
            action: match r.action {
                setup::SetupAction::RunClaude { .. } => AgentSetupAction::RunClaude,
                setup::SetupAction::OpenUrl { url } => AgentSetupAction::OpenUrl { url },
                setup::SetupAction::MergeConfig { path } => AgentSetupAction::MergeConfig { path },
                setup::SetupAction::CopyOnly => AgentSetupAction::CopyOnly,
            },
            copy_text: r.copy_text,
            note: r.note,
        })
        .collect()
}

/// The `claude` to run, or `None`: the usual install locations and this
/// process's `PATH` (`client::agent_setup::claude_code_candidates`), and
/// on macOS and Linux then the user's login shell, which sees the `PATH`
/// their terminal has (`$SHELL -lc 'command -v claude'`, given 5 s). It
/// blocks for up to that long; call it off the main thread.
#[uniffi::export]
pub fn agent_setup_find_claude() -> Option<String> {
    let host: setup::Host = agent_setup_host().into();
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
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());
    let mut cmd = Command::new(shell);
    cmd.args(["-lc", "command -v claude"]);
    let out = run(cmd, Duration::from_secs(5)).ok()?;
    let line = out.output.lines().last()?.trim().to_string();
    (out.success && line.starts_with('/') && is_executable(Path::new(&line))).then_some(line)
}

/// Runs `claude mcp add --scope user neoscad -- <cli> mcp` with the
/// `claude` found by [`agent_setup_find_claude`]. With `replace`, removes
/// the user-scope `neoscad` first (the add refuses an existing name).
/// Blocks for as long as `claude` takes (up to 60 s); call it off the main
/// thread.
#[uniffi::export]
pub fn agent_setup_add_to_claude_code(
    claude: String,
    cli: String,
    replace: bool,
) -> Result<ClaudeCodeOutcome, CoreError> {
    guarded(|| {
        let host = agent_setup_host().into();
        let server = setup::ServerCommand::for_host(host, &cli);
        Ok(add_to_claude_code(Path::new(&claude), &server, replace))
    })
}

/// Adds the server to Claude Desktop's config (macOS and Windows), after
/// the user agreed: `client::agent_setup::merge_claude_desktop_config` on
/// the file's text, a backup beside it, then an atomic replace. Refuses a
/// file it cannot read as JSON rather than rewrite it.
#[uniffi::export]
pub fn agent_setup_add_to_claude_desktop(cli: String) -> Result<ClaudeDesktopOutcome, CoreError> {
    guarded(|| {
        let host = agent_setup_host().into();
        let server = setup::ServerCommand::for_host(host, &cli);
        let path = setup::claude_desktop_config_path(host, &home(), appdata().as_deref())
            .ok_or_else(|| CoreError::InvalidArgument {
                message: "Claude Desktop does not run on this system".to_string(),
            })?;
        add_to_claude_desktop(Path::new(&path), &server, now()).map_err(|e| CoreError::Failed {
            message: format!("could not update {path}: {e}"),
        })
    })
}

// --- The work, with its inputs explicit for the tests ----------------------

fn add_to_claude_code(
    claude: &Path,
    server: &setup::ServerCommand,
    replace: bool,
) -> ClaudeCodeOutcome {
    let limit = Duration::from_secs(60);
    if replace {
        // A failure here (no entry to remove) shows in the add's result.
        let _ = run(
            claude_command(claude, &setup::claude_code_remove_args()),
            limit,
        );
    }
    match run(
        claude_command(claude, &setup::claude_code_add_args(server)),
        limit,
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

fn add_to_claude_desktop(
    path: &Path,
    server: &setup::ServerCommand,
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

fn home() -> String {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var(var).unwrap_or_default()
}

fn appdata() -> Option<String> {
    std::env::var("APPDATA").ok().filter(|a| !a.is_empty())
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn is_executable(p: &Path) -> bool {
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
    let mut stdout = child.stdout.take().expect("piped");
    let mut stderr = child.stderr.take().expect("piped");
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

    #[test]
    fn claude_desktop_config_is_backed_up_then_replaced() {
        let dir = scratch("desktop");
        let config = dir.join("Claude/claude_desktop_config.json");
        // No Claude directory: not installed, nothing created.
        assert_eq!(
            add_to_claude_desktop(&config, &server(), 0).unwrap(),
            ClaudeDesktopOutcome::NotInstalled {
                path: config.display().to_string()
            }
        );
        assert!(!config.parent().unwrap().exists());

        // A directory with no file: written, no backup.
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        let r = add_to_claude_desktop(&config, &server(), 0).unwrap();
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

        // The same again: unchanged, nothing new on disk.
        let r = add_to_claude_desktop(&config, &server(), 1).unwrap();
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
        } = add_to_claude_desktop(&config, &server(), 1_791_037_805).unwrap()
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
        } = add_to_claude_desktop(&config, &server(), 1_791_037_805).unwrap()
        else {
            panic!()
        };
        assert!(b2.ends_with("-20261003T143005Z-1"), "{b2}");
        assert_eq!(std::fs::read_to_string(&b1).unwrap(), old);

        // Not JSON: refused, and the file is exactly as it was, no backup.
        let bad = "{\n  // mine\n  \"mcpServers\": {}\n}\n";
        std::fs::write(&config, bad).unwrap();
        let before = std::fs::read_dir(config.parent().unwrap()).unwrap().count();
        let r = add_to_claude_desktop(&config, &server(), 5).unwrap();
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
        let r = add_to_claude_desktop(&config, &server(), 0).unwrap();
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

    #[test]
    fn rows_name_the_given_cli() {
        let rows = agent_setup_rows("/Apps/NeoSCAD/bin/neoscad".to_string());
        assert_eq!(rows[0].client, AgentSetupClient::ClaudeCode);
        assert!(
            rows.iter()
                .any(|r| r.copy_text.contains("/Apps/NeoSCAD/bin/neoscad"))
        );
        if cfg!(target_os = "macos") {
            assert!(
                rows.iter()
                    .any(|r| matches!(r.action, AgentSetupAction::MergeConfig { .. }))
            );
        }
    }
}
