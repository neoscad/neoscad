//! Setting up AI agent clients to use the `neoscad` an app ships
//! (`docs/audits/agent-connection-desktop.md`, Option A; `docs/mcp.md`,
//! "Setup from the apps"): for each client, the exact command, install
//! link or config change, as data. Nothing here runs a program, opens a
//! link or touches a file: the hosts do that (`crates/ffi/src/agent_setup.rs`
//! for the macOS and Windows apps), so this stays a library module and the
//! same table can feed every surface.
//!
//! Every setup names the server by an absolute command, never a bare
//! `neoscad`. A GUI client on macOS starts servers with launchd's minimal
//! `PATH`, not the shell's, so a bare name that works in a terminal fails
//! in Claude Desktop; and the app's own copy is the one whose version
//! matches the app. The command is the app's stable link on macOS
//! (`~/Library/Application Support/NeoSCAD/bin/neoscad`, refreshed at every
//! launch), the installed `bin\neoscad.exe` on Windows, and `flatpak run
//! --command=neoscad org.neoscad.NeoSCAD` from a Flatpak, which cannot put
//! anything on the host's `PATH`.
//!
//! How each client is set up, and where that was checked (2026-10-02):
//!
//! - **Claude Code:** `claude mcp add --scope user neoscad -- <command>`
//!   (user scope: every project). Run by the app when it finds `claude`
//!   ([`claude_code_candidates`]); a second `add` of the same name fails
//!   with "MCP server neoscad already exists in user config" (exit 1;
//!   checked with Claude Code 2.1.288 in an isolated `CLAUDE_CONFIG_DIR`),
//!   so replacing an entry is `remove` then `add`.
//! - **Cursor:** its install link, `cursor://anysphere.cursor-deeplink/
//!   mcp/install?name=neoscad&config=<base64 of the JSON server>`
//!   (cursor.com/docs/context/mcp/install-links). Cursor asks the user.
//! - **VS Code:** its install link, `vscode:mcp/install?<URL-encoded JSON
//!   with the name>` (code.visualstudio.com/api/extension-guides/ai/mcp).
//!   VS Code asks the user.
//! - **Claude Desktop** (macOS and Windows only): no link or command
//!   exists, so the app merges `mcpServers.neoscad` into
//!   `claude_desktop_config.json`, with the user's consent and a backup
//!   ([`merge_claude_desktop_config`], [`backup_file_name`]), and then
//!   asks for a restart (modelcontextprotocol.io/docs/develop/
//!   connect-local-servers). Its entry also passes `--root` with a
//!   folder in the user's Documents ([`claude_desktop_folder`]): Claude
//!   Desktop starts servers in an undefined working directory, which
//!   `neoscad mcp` never takes as a root, so without one its agent could
//!   read but never write an export.
//! - **Other:** the JSON to copy.
//!
//! After the setup, what using it is like ("Using NeoSCAD with your
//! agent": which document, saving, undo, the view, export) is
//! [`usage`], per host and client, from `agent_setup/usage.rs`.

use serde::de::{self, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, SerializeSeq, Serializer};
use serde::{Deserialize, Serialize};

mod usage;
pub use usage::{Usage, UsageItem, UsageTopic, usage};

/// The name every setup registers the server under.
pub const SERVER_NAME: &str = "neoscad";

/// The Linux app's Flatpak id, for `flatpak run`.
pub const FLATPAK_APP_ID: &str = "org.neoscad.NeoSCAD";

/// Where the app runs, which decides the paths, the quoting of shown
/// commands, and what the app may do itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Host {
    MacOs,
    Windows,
    /// A Linux app that is not sandboxed (a package or a build).
    Linux,
    /// The Flatpak: it cannot run host programs (no
    /// `--talk-name=org.freedesktop.Flatpak`, the owner's decision), so
    /// Claude Code's row is a command to copy, and the server is started
    /// through `flatpak run`.
    LinuxFlatpak,
}

impl Host {
    fn windows(self) -> bool {
        self == Host::Windows
    }

    /// Whether the app may run a host program such as `claude`.
    pub fn can_run_host_programs(self) -> bool {
        self != Host::LinuxFlatpak
    }
}

/// How a client starts the server: a program and its arguments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerCommand {
    /// Absolute, except `flatpak`, which the host finds on its own
    /// `PATH` (it is in `/usr/bin` wherever Flatpak is installed).
    pub command: String,
    pub args: Vec<String>,
}

impl ServerCommand {
    /// The app's own CLI at `cli` (absolute), serving MCP. No `--root`:
    /// the working directory is the root, which the client chooses (the
    /// project, for Claude Code, Cursor and VS Code).
    pub fn bundled(cli: &str) -> ServerCommand {
        ServerCommand {
            command: cli.to_string(),
            args: vec!["mcp".to_string()],
        }
    }

    /// The Flatpak's CLI, from the host: `flatpak run
    /// --command=neoscad org.neoscad.NeoSCAD mcp`.
    pub fn flatpak() -> ServerCommand {
        ServerCommand {
            command: "flatpak".to_string(),
            args: vec![
                "run".to_string(),
                "--command=neoscad".to_string(),
                FLATPAK_APP_ID.to_string(),
                "mcp".to_string(),
            ],
        }
    }

    /// The command for `host`: the Flatpak form there, else
    /// [`ServerCommand::bundled`] with `cli`.
    pub fn for_host(host: Host, cli: &str) -> ServerCommand {
        match host {
            Host::LinuxFlatpak => ServerCommand::flatpak(),
            _ => ServerCommand::bundled(cli),
        }
    }

    /// `{"command": ..., "args": [...]}`, the entry every JSON config
    /// takes.
    fn entry(&self) -> Json {
        Json::Object(vec![
            ("command".to_string(), Json::String(self.command.clone())),
            (
                "args".to_string(),
                Json::Array(self.args.iter().cloned().map(Json::String).collect()),
            ),
        ])
    }

    /// This command with `--root <dir>` added: `dir` becomes a folder the
    /// agent may write in whatever directory the client starts it in.
    pub fn with_root(&self, dir: &str) -> ServerCommand {
        let mut s = self.clone();
        s.args.push("--root".to_string());
        s.args.push(dir.to_string());
        s
    }

    /// The command line, quoted for `host`'s usual shell.
    pub fn display(&self, host: Host) -> String {
        std::iter::once(&self.command)
            .chain(&self.args)
            .map(|a| quote(a, host))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// The clients the apps set up, in the order they are listed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Client {
    ClaudeCode,
    ClaudeDesktop,
    Cursor,
    VsCode,
    Other,
}

impl Client {
    pub const ALL: [Client; 5] = [
        Client::ClaudeCode,
        Client::ClaudeDesktop,
        Client::Cursor,
        Client::VsCode,
        Client::Other,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Client::ClaudeCode => "Claude Code",
            Client::ClaudeDesktop => "Claude Desktop",
            Client::Cursor => "Cursor",
            Client::VsCode => "VS Code",
            Client::Other => "Other MCP clients",
        }
    }

    /// The label in a picker of all five side by side (a segmented
    /// control, tabs): "Other MCP clients" would make its segment twice
    /// the width of the rest, so it is "Other" there, as on the /try page.
    pub fn short_label(self) -> &'static str {
        match self {
            Client::Other => "Other",
            c => c.label(),
        }
    }
}

/// What the app's one click does for a client.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SetupAction {
    /// Run `claude` (found with [`claude_code_candidates`]) with `args`.
    /// If it fails because the entry exists, ask, then run
    /// `remove_args` and `args` again.
    RunClaude {
        args: Vec<String>,
        remove_args: Vec<String>,
    },
    /// Open the client's install link; the client asks the user.
    OpenUrl { url: String },
    /// Merge the server into the JSON file at `path` (Claude Desktop),
    /// after consent, with a backup: [`merge_claude_desktop_config`].
    MergeConfig { path: String },
    /// Nothing to click: show [`ClientSetup::copy_text`].
    CopyOnly,
}

/// One client's row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientSetup {
    pub client: Client,
    pub label: String,
    pub action: SetupAction,
    /// Always offered: the command or JSON that does the same by hand,
    /// with the absolute command filled in.
    pub copy_text: String,
    /// Where the copied text goes, or what to do after the click.
    pub note: String,
}

/// The setups for `host`, serving `server`. `home` is the user's home
/// directory; `appdata` is `%APPDATA%` on Windows (unused elsewhere).
/// Claude Desktop is listed only where it exists (macOS and Windows) and
/// its config's directory is known. Its project folder is taken to be in
/// `home`'s `Documents` ([`default_documents`]); a host that knows the
/// real Documents folder (Windows can redirect it) calls [`setups_in`].
pub fn setups(
    host: Host,
    server: &ServerCommand,
    home: &str,
    appdata: Option<&str>,
) -> Vec<ClientSetup> {
    setups_in(host, server, home, appdata, &default_documents(host, home))
}

/// [`setups`] with the user's Documents folder, `documents`, given: the
/// Claude Desktop row's `--root` is [`claude_desktop_folder`] in it.
pub fn setups_in(
    host: Host,
    server: &ServerCommand,
    home: &str,
    appdata: Option<&str>,
    documents: &str,
) -> Vec<ClientSetup> {
    Client::ALL
        .iter()
        .filter_map(|&c| setup_in(c, host, server, home, appdata, documents))
        .collect()
}

/// One client's setup (see [`setups`]); `None` where the client does not
/// run on `host`.
pub fn setup(
    client: Client,
    host: Host,
    server: &ServerCommand,
    home: &str,
    appdata: Option<&str>,
) -> Option<ClientSetup> {
    setup_in(
        client,
        host,
        server,
        home,
        appdata,
        &default_documents(host, home),
    )
}

/// [`setup`] with the user's Documents folder given ([`setups_in`]).
pub fn setup_in(
    client: Client,
    host: Host,
    server: &ServerCommand,
    home: &str,
    appdata: Option<&str>,
    documents: &str,
) -> Option<ClientSetup> {
    let row = |action, copy_text, note: &str| ClientSetup {
        client,
        label: client.label().to_string(),
        action,
        copy_text,
        note: note.to_string(),
    };
    Some(match client {
        Client::ClaudeCode => {
            let args = claude_code_add_args(server);
            let shown = std::iter::once("claude".to_string())
                .chain(args.iter().map(|a| quote(a, host)))
                .collect::<Vec<_>>()
                .join(" ");
            if host.can_run_host_programs() {
                row(
                    SetupAction::RunClaude {
                        args,
                        remove_args: claude_code_remove_args(),
                    },
                    shown,
                    "Adds NeoSCAD for all your projects. If Claude Code is not found, run this in a terminal.",
                )
            } else {
                row(
                    SetupAction::CopyOnly,
                    shown,
                    "Run this in a terminal: it adds NeoSCAD for all your projects.",
                )
            }
        }
        Client::ClaudeDesktop => {
            let path = claude_desktop_config_path(host, home, appdata)?;
            let folder = claude_desktop_folder(host, documents)?;
            row(
                SetupAction::MergeConfig { path },
                servers_json("mcpServers", &server.with_root(&folder), false),
                &format!(
                    "Then quit and reopen Claude to finish. Its agent saves files in {folder}."
                ),
            )
        }
        Client::Cursor => row(
            SetupAction::OpenUrl {
                url: cursor_install_url(server),
            },
            servers_json("mcpServers", server, false),
            "Or add this to ~/.cursor/mcp.json for all projects, or .cursor/mcp.json in one.",
        ),
        Client::VsCode => row(
            SetupAction::OpenUrl {
                url: vscode_install_url(server),
            },
            servers_json("servers", server, true),
            "Or add this to .vscode/mcp.json in your project.",
        ),
        Client::Other => row(
            SetupAction::CopyOnly,
            servers_json("mcpServers", server, false),
            "Most MCP clients take this shape. The server speaks MCP on stdio. It writes files \
             only in the folder the client starts it in, when that is a project: a client that \
             starts it elsewhere needs \"--root\" and a folder added to \"args\".",
        ),
    })
}

/// `claude`'s arguments to add the server for every project.
pub fn claude_code_add_args(server: &ServerCommand) -> Vec<String> {
    let mut a: Vec<String> = ["mcp", "add", "--scope", "user", SERVER_NAME, "--"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    a.push(server.command.clone());
    a.extend(server.args.iter().cloned());
    a
}

/// `claude`'s arguments to remove the user-scope entry, before adding it
/// again with another command.
pub fn claude_code_remove_args() -> Vec<String> {
    ["mcp", "remove", "--scope", "user", SERVER_NAME]
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// Whether `claude mcp add`'s output says the entry is already there
/// (Claude Code 2.1.288: "MCP server neoscad already exists in user
/// config", exit 1).
pub fn claude_code_reports_existing(output: &str) -> bool {
    output.contains("already exists")
}

/// Where to look for `claude`, most likely first. A GUI app's `PATH` is
/// launchd's minimal one on macOS, which holds none of the places Claude
/// Code installs to, so the usual install locations follow `path` (the
/// `PATH` the app was given): the native installer's launcher
/// (`~/.local/bin/claude`, `%USERPROFILE%\.local\bin\claude.exe`), the
/// Homebrew cask's link, Linux packages, and npm's global directories
/// (code.claude.com/docs/en/setup, 2026-10-02). The host takes the first
/// that is an executable file.
pub fn claude_code_candidates(
    host: Host,
    home: &str,
    path: &str,
    appdata: Option<&str>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |p: String| {
        if !p.is_empty() && !out.contains(&p) {
            out.push(p);
        }
    };
    if host.windows() {
        for dir in path.split(';').filter(|d| !d.is_empty()) {
            let dir = dir.trim_end_matches('\\');
            push(format!("{dir}\\claude.exe"));
            push(format!("{dir}\\claude.cmd"));
        }
        push(format!("{home}\\.local\\bin\\claude.exe"));
        if let Some(appdata) = appdata {
            push(format!("{appdata}\\npm\\claude.cmd"));
        }
    } else {
        for dir in path.split(':').filter(|d| !d.is_empty()) {
            push(format!("{}/claude", dir.trim_end_matches('/')));
        }
        push(format!("{home}/.local/bin/claude"));
        if host == Host::MacOs {
            push("/opt/homebrew/bin/claude".to_string());
        }
        push("/usr/local/bin/claude".to_string());
        push("/usr/bin/claude".to_string());
        push(format!("{home}/.npm-global/bin/claude"));
        push(format!("{home}/.claude/local/claude"));
    }
    out
}

/// Claude Desktop's config file: `~/Library/Application
/// Support/Claude/claude_desktop_config.json` on macOS and
/// `%APPDATA%\Claude\claude_desktop_config.json` on Windows; none on
/// Linux, where Claude Desktop does not run.
pub fn claude_desktop_config_path(host: Host, home: &str, appdata: Option<&str>) -> Option<String> {
    match host {
        Host::MacOs => Some(format!(
            "{}/Library/Application Support/Claude/claude_desktop_config.json",
            home.trim_end_matches('/')
        )),
        Host::Windows => appdata.filter(|a| !a.is_empty()).map(|a| {
            format!(
                "{}\\Claude\\claude_desktop_config.json",
                a.trim_end_matches('\\')
            )
        }),
        Host::Linux | Host::LinuxFlatpak => None,
    }
}

/// The Documents folder in `home` when the host knows no better:
/// `~/Documents` on macOS (where the system keeps it, and where OpenSCAD
/// looks for it, `PlatformUtils-mac.mm`), `%USERPROFILE%\Documents` on
/// Windows, which is wrong when the folder is redirected (OneDrive's
/// backup does that), so the Windows host passes the shell's known folder
/// to [`setups_in`] instead. Linux has no Claude Desktop to use it.
pub fn default_documents(host: Host, home: &str) -> String {
    if host.windows() {
        format!("{}\\Documents", home.trim_end_matches('\\'))
    } else {
        format!("{}/Documents", home.trim_end_matches('/'))
    }
}

/// The folder Claude Desktop's `neoscad mcp` gets as its `--root`, in the
/// user's Documents folder `documents`: `~/Documents/NeoSCAD` on macOS,
/// `Documents\NeoSCAD` on Windows; `None` on Linux, where Claude Desktop
/// does not run. A folder of its own rather than Documents itself: the
/// agent may write anywhere in a root, and the user agreed to NeoSCAD's
/// files, not to everything they keep in Documents.
pub fn claude_desktop_folder(host: Host, documents: &str) -> Option<String> {
    match host {
        Host::MacOs => Some(format!("{}/NeoSCAD", documents.trim_end_matches('/'))),
        Host::Windows => Some(format!("{}\\NeoSCAD", documents.trim_end_matches('\\'))),
        Host::Linux | Host::LinuxFlatpak => None,
    }
}

/// Claude Desktop's server: `server` with [`claude_desktop_folder`] as
/// its `--root`, as its row's JSON shows and the app writes; `None` on
/// Linux.
pub fn claude_desktop_server(
    host: Host,
    server: &ServerCommand,
    documents: &str,
) -> Option<ServerCommand> {
    claude_desktop_folder(host, documents).map(|f| server.with_root(&f))
}

/// Cursor's install link: the server entry as compact JSON, base64,
/// then URL-encoded (base64's `+` would read as a space in a query).
pub fn cursor_install_url(server: &ServerCommand) -> String {
    let config = compact(&server.entry());
    format!(
        "cursor://anysphere.cursor-deeplink/mcp/install?name={}&config={}",
        encode_uri_component(SERVER_NAME),
        encode_uri_component(&base64(config.as_bytes()))
    )
}

/// VS Code's install link: the entry with its name and type, as
/// URL-encoded JSON.
pub fn vscode_install_url(server: &ServerCommand) -> String {
    let Json::Object(mut fields) = server.entry() else {
        unreachable!("an entry is an object")
    };
    fields.insert(0, ("type".to_string(), Json::String("stdio".to_string())));
    fields.insert(
        0,
        ("name".to_string(), Json::String(SERVER_NAME.to_string())),
    );
    format!(
        "vscode:mcp/install?{}",
        encode_uri_component(&compact(&Json::Object(fields)))
    )
}

/// `{"<key>": {"neoscad": <entry>}}`, pretty, for copying.
fn servers_json(key: &str, server: &ServerCommand, stdio_type: bool) -> String {
    let mut entry = server.entry();
    if stdio_type && let Json::Object(fields) = &mut entry {
        fields.insert(0, ("type".to_string(), Json::String("stdio".to_string())));
    }
    let doc = Json::Object(vec![(
        key.to_string(),
        Json::Object(vec![(SERVER_NAME.to_string(), entry)]),
    )]);
    pretty(&doc)
}

// --- Claude Desktop's config ------------------------------------------------

/// What [`merge_claude_desktop_config`] changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConfigChange {
    /// There was no `neoscad` entry; one was added.
    Added,
    /// There was one with another command or arguments; they were
    /// replaced, and its other fields (`env`, say) kept.
    Replaced,
    /// The entry already runs this command: nothing to write.
    Unchanged,
}

/// The merged file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigMerge {
    /// The whole new file: two-space JSON, keys in their original order,
    /// a final newline. Equal to the input's meaning when `Unchanged`.
    pub text: String,
    pub change: ConfigChange,
    /// The entry that was replaced, as compact JSON.
    pub previous: Option<String>,
}

/// Why the file was left alone. Each means the user's file is not what
/// this code understands, so it must not be rewritten: the app shows the
/// JSON to add by hand instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ConfigError {
    /// Not JSON (comments and trailing commas included: Claude Desktop
    /// reads strict JSON). `line` and `column` are 1-based.
    InvalidJson {
        line: u64,
        column: u64,
        message: String,
    },
    /// JSON, but not an object at the top.
    NotAnObject,
    /// `mcpServers` is there but not an object.
    ServersNotAnObject,
    /// `mcpServers.neoscad` is there but not an object.
    EntryNotAnObject,
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::InvalidJson {
                line,
                column,
                message,
            } => write!(
                f,
                "the file is not valid JSON (line {line}, column {column}: {message})"
            ),
            ConfigError::NotAnObject => f.write_str("the file's JSON is not an object"),
            ConfigError::ServersNotAnObject => f.write_str("its \"mcpServers\" is not an object"),
            ConfigError::EntryNotAnObject => {
                f.write_str("its \"mcpServers\" has a \"neoscad\" that is not an object")
            }
        }
    }
}

impl std::error::Error for ConfigError {}

/// Adds `server` as `mcpServers.neoscad` to the text of Claude Desktop's
/// config, `None` when the file does not exist. Every other key and
/// server is kept, in its order; an existing `neoscad` entry keeps its
/// place and its other fields, with `command` and `args` replaced.
/// Refuses, rather than guessing, anything it cannot read as that shape.
pub fn merge_claude_desktop_config(
    existing: Option<&str>,
    server: &ServerCommand,
) -> Result<ConfigMerge, ConfigError> {
    let mut doc = match existing {
        // A file that is empty or only whitespace holds nothing to keep;
        // Claude's own "Edit Config" can leave one.
        None => Json::Object(Vec::new()),
        Some(t) if t.trim().is_empty() => Json::Object(Vec::new()),
        Some(t) => serde_json::from_str::<Json>(t).map_err(|e| ConfigError::InvalidJson {
            line: e.line() as u64,
            column: e.column() as u64,
            message: e.to_string(),
        })?,
    };
    let Json::Object(top) = &mut doc else {
        return Err(ConfigError::NotAnObject);
    };
    // A duplicate key reads as its last occurrence, as JSON.parse and
    // serde_json do, so that is the one edited.
    let servers = match top.iter().rposition(|(k, _)| k == "mcpServers") {
        Some(i) => &mut top[i].1,
        None => {
            top.push(("mcpServers".to_string(), Json::Object(Vec::new())));
            &mut top.last_mut().expect("just pushed").1
        }
    };
    let Json::Object(servers) = servers else {
        return Err(ConfigError::ServersNotAnObject);
    };
    let Json::Object(wanted) = server.entry() else {
        unreachable!("an entry is an object")
    };
    let (change, previous) = match servers.iter().rposition(|(k, _)| k == SERVER_NAME) {
        None => {
            servers.push((SERVER_NAME.to_string(), Json::Object(wanted)));
            (ConfigChange::Added, None)
        }
        Some(i) => {
            let Json::Object(fields) = &mut servers[i].1 else {
                return Err(ConfigError::EntryNotAnObject);
            };
            let before = compact(&Json::Object(fields.clone()));
            let mut changed = false;
            for (key, value) in wanted {
                match fields.iter_mut().rev().find(|(k, _)| *k == key) {
                    Some((_, v)) if *v == value => {}
                    Some((_, v)) => {
                        *v = value;
                        changed = true;
                    }
                    None => {
                        fields.push((key, value));
                        changed = true;
                    }
                }
            }
            if changed {
                (ConfigChange::Replaced, Some(before))
            } else {
                (ConfigChange::Unchanged, None)
            }
        }
    };
    Ok(ConfigMerge {
        text: pretty(&doc),
        change,
        previous,
    })
}

/// What Claude Desktop's config already has, against what
/// [`merge_claude_desktop_config`] would write for `server`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ClaudeDesktopEntry {
    /// No file, or no `neoscad` entry in it.
    Missing,
    /// The entry runs `server` with its arguments: nothing to do.
    Current,
    /// The entry runs the same program with other arguments: an earlier
    /// setup, such as one from before the setup passed `--root` (whose
    /// agent cannot export). Offer to update it.
    Outdated,
    /// The entry runs another program, one the user set up themselves.
    Other,
}

/// Reads `existing` (the config's text, `None` when there is no file)
/// as [`merge_claude_desktop_config`] does, and says how its `neoscad`
/// entry compares with `server`. Refuses the same files the merge does.
pub fn claude_desktop_entry(
    existing: Option<&str>,
    server: &ServerCommand,
) -> Result<ClaudeDesktopEntry, ConfigError> {
    let merged = merge_claude_desktop_config(existing, server)?;
    Ok(match merged.change {
        ConfigChange::Added => ClaudeDesktopEntry::Missing,
        ConfigChange::Unchanged => ClaudeDesktopEntry::Current,
        ConfigChange::Replaced => {
            let previous = merged
                .previous
                .as_deref()
                .and_then(|p| serde_json::from_str::<serde_json::Value>(p).ok());
            let command = previous.as_ref().and_then(|p| p["command"].as_str());
            if command == Some(server.command.as_str()) {
                ClaudeDesktopEntry::Outdated
            } else {
                ClaudeDesktopEntry::Other
            }
        }
    })
}

/// The backup's file name, beside the original: `<name>.neoscad-backup-
/// <UTC time>`, so backups sort by time and are plainly ours, and an
/// editor still sees the original as the only `.json`. `unix_seconds` is
/// the host's clock (this module has none); `attempt` is 0, then 1, 2, …
/// if a file of that name exists (two backups within a second).
pub fn backup_file_name(file_name: &str, unix_seconds: i64, attempt: u32) -> String {
    let days = unix_seconds.div_euclid(86_400);
    let secs = unix_seconds.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let stamp = format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    );
    if attempt == 0 {
        format!("{file_name}.neoscad-backup-{stamp}")
    } else {
        format!("{file_name}.neoscad-backup-{stamp}-{attempt}")
    }
}

/// Days since 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's
/// `civil_from_days`).
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

// --- Quoting and encoding -----------------------------------------------

/// `arg` as one word for the usual shell: POSIX single quotes on macOS
/// and Linux, double quotes on Windows (PowerShell and cmd both read a
/// double-quoted path with spaces as one argument; Windows paths cannot
/// contain `"`).
fn quote(arg: &str, host: Host) -> String {
    // Characters no shell of that kind treats specially. `%` stays out on
    // Windows (cmd expands `%NAME%`) and `\` out elsewhere.
    let safe: &[u8] = if host.windows() {
        b"\\:./-_=+,@"
    } else {
        b"@%+=:,./-_"
    };
    let plain = !arg.is_empty()
        && arg
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || safe.contains(&b));
    if plain {
        arg.to_string()
    } else if host.windows() {
        format!("\"{arg}\"")
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

/// JavaScript's `encodeURIComponent`, which both install-link pages use.
fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Standard base64 with padding (RFC 4648), as the browser's `btoa`.
fn base64(bytes: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(A[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

// --- JSON that keeps its key order ------------------------------------------

/// A JSON value whose objects keep their keys in order. The workspace's
/// `serde_json::Value` sorts keys (its `preserve_order` feature is off,
/// and turning it on would change every crate's output through feature
/// unification), and rewriting a user's config in alphabetical order
/// would be a needless diff in a file they edit by hand.
#[derive(Debug, Clone, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

fn pretty(j: &Json) -> String {
    let mut s = serde_json::to_string_pretty(j).expect("JSON values always serialize");
    s.push('\n');
    s
}

fn compact(j: &Json) -> String {
    serde_json::to_string(j).expect("JSON values always serialize")
}

impl Serialize for Json {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            Json::Null => s.serialize_unit(),
            Json::Bool(b) => s.serialize_bool(*b),
            Json::Number(n) => n.serialize(s),
            Json::String(t) => s.serialize_str(t),
            Json::Array(a) => {
                let mut seq = s.serialize_seq(Some(a.len()))?;
                for v in a {
                    seq.serialize_element(v)?;
                }
                seq.end()
            }
            Json::Object(o) => {
                let mut map = s.serialize_map(Some(o.len()))?;
                for (k, v) in o {
                    map.serialize_entry(k, v)?;
                }
                map.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Json, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Json;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("JSON")
            }
            fn visit_unit<E>(self) -> Result<Json, E> {
                Ok(Json::Null)
            }
            fn visit_none<E>(self) -> Result<Json, E> {
                Ok(Json::Null)
            }
            fn visit_bool<E>(self, b: bool) -> Result<Json, E> {
                Ok(Json::Bool(b))
            }
            fn visit_i64<E>(self, n: i64) -> Result<Json, E> {
                Ok(Json::Number(n.into()))
            }
            fn visit_u64<E>(self, n: u64) -> Result<Json, E> {
                Ok(Json::Number(n.into()))
            }
            fn visit_f64<E: de::Error>(self, n: f64) -> Result<Json, E> {
                serde_json::Number::from_f64(n)
                    .map(Json::Number)
                    .ok_or_else(|| E::custom("a number JSON cannot hold"))
            }
            fn visit_str<E>(self, s: &str) -> Result<Json, E> {
                Ok(Json::String(s.to_string()))
            }
            fn visit_string<E>(self, s: String) -> Result<Json, E> {
                Ok(Json::String(s))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Json, A::Error> {
                let mut v = Vec::new();
                while let Some(x) = a.next_element()? {
                    v.push(x);
                }
                Ok(Json::Array(v))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Json, A::Error> {
                let mut v = Vec::new();
                while let Some(e) = m.next_entry::<String, Json>()? {
                    v.push(e);
                }
                Ok(Json::Object(v))
            }
        }
        d.deserialize_any(V)
    }
}

#[cfg(test)]
#[path = "agent_setup_tests.rs"]
mod tests;
