//! Connecting an AI agent client, for the apps' "Connect your AI agent"
//! sheet (`docs/mcp.md`, "Setup from the apps";
//! `docs/audits/agent-connection-desktop.md`, Option A), for Swift and C#.
//! The rows (what each client needs, as data) come from
//! `client::agent_setup`; what needs the machine (the environment, finding
//! and running `claude`, editing Claude Desktop's config file with a
//! backup) is `agent_link::setup`, which the Linux app calls directly. This
//! module only carries both across UniFFI.
//!
//! Every function here acts at once. Asking first is the app's part: the
//! sheet shows the exact command or change, and Claude Desktop's config is
//! written only after the user agreed to it (the owner's decision,
//! 2026-10-02). Cursor and VS Code need nothing here: the app opens the
//! row's link, and the client asks the user itself.
//!
//! The Linux app does not link this crate; its Flatpak runs nothing on
//! the host.

use std::path::Path;

use agent_link::setup as machine;
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

impl From<setup::Host> for AgentSetupHost {
    fn from(h: setup::Host) -> Self {
        match h {
            setup::Host::MacOs => AgentSetupHost::MacOs,
            setup::Host::Windows => AgentSetupHost::Windows,
            setup::Host::Linux => AgentSetupHost::Linux,
            setup::Host::LinuxFlatpak => AgentSetupHost::LinuxFlatpak,
        }
    }
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

impl From<machine::ClaudeCodeOutcome> for ClaudeCodeOutcome {
    fn from(o: machine::ClaudeCodeOutcome) -> Self {
        match o {
            machine::ClaudeCodeOutcome::Added { output } => ClaudeCodeOutcome::Added { output },
            machine::ClaudeCodeOutcome::AlreadyExists { output } => {
                ClaudeCodeOutcome::AlreadyExists { output }
            }
            machine::ClaudeCodeOutcome::Failed { output } => ClaudeCodeOutcome::Failed { output },
        }
    }
}

impl From<machine::ClaudeDesktopOutcome> for ClaudeDesktopOutcome {
    fn from(o: machine::ClaudeDesktopOutcome) -> Self {
        match o {
            machine::ClaudeDesktopOutcome::Written {
                path,
                backup,
                replaced_entry,
            } => ClaudeDesktopOutcome::Written {
                path,
                backup,
                replaced_entry,
            },
            machine::ClaudeDesktopOutcome::Unchanged { path } => {
                ClaudeDesktopOutcome::Unchanged { path }
            }
            machine::ClaudeDesktopOutcome::NotInstalled { path } => {
                ClaudeDesktopOutcome::NotInstalled { path }
            }
            machine::ClaudeDesktopOutcome::Refused { path, reason } => {
                ClaudeDesktopOutcome::Refused { path, reason }
            }
        }
    }
}

/// The host this core was built for. `LinuxFlatpak` inside a Flatpak
/// sandbox (`/.flatpak-info` exists there, and only there).
#[uniffi::export]
pub fn agent_setup_host() -> AgentSetupHost {
    machine::host().into()
}

/// The sheet's rows for this host, with `cli` (the app's absolute
/// `neoscad`: the stable link on macOS, `bin\neoscad.exe` in the install
/// folder on Windows) as the server's command.
#[uniffi::export]
pub fn agent_setup_rows(cli: String) -> Vec<AgentSetupRow> {
    machine::rows(&cli)
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
    machine::find_claude()
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
        let server = setup::ServerCommand::for_host(machine::host(), &cli);
        Ok(machine::add_to_claude_code(Path::new(&claude), &server, replace).into())
    })
}

/// Adds the server to Claude Desktop's config (macOS and Windows), after
/// the user agreed: `client::agent_setup::merge_claude_desktop_config` on
/// the file's text, a backup beside it, then an atomic replace. Refuses a
/// file it cannot read as JSON rather than rewrite it.
#[uniffi::export]
pub fn agent_setup_add_to_claude_desktop(cli: String) -> Result<ClaudeDesktopOutcome, CoreError> {
    guarded(|| {
        let server = setup::ServerCommand::for_host(machine::host(), &cli);
        let path = machine::claude_desktop_config().ok_or_else(|| CoreError::InvalidArgument {
            message: "Claude Desktop does not run on this system".to_string(),
        })?;
        machine::add_to_claude_desktop(Path::new(&path), &server, machine::now())
            .map(Into::into)
            .map_err(|e| CoreError::Failed {
                message: format!("could not update {path}: {e}"),
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // The file and process work is tested where it lives,
    // `crates/agent-link/src/setup.rs`.

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
