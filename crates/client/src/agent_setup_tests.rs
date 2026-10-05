//! Tests of `agent_setup`: the per-client setups, and the Claude Desktop
//! merge against existing configs (none, empty, other servers, an existing
//! entry, and files it must refuse to rewrite).

use super::*;

const MAC_CLI: &str = "/home/someone/Library/Application Support/NeoSCAD/bin/neoscad";
const WIN_CLI: &str = r"C:\Program Files\NeoSCAD\bin\neoscad.exe";

fn mac() -> ServerCommand {
    ServerCommand::bundled(MAC_CLI)
}

fn value(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap()
}

fn percent_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' {
            out.push(u8::from_str_radix(&s[i + 1..i + 3], 16).unwrap());
            i += 3;
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    String::from_utf8(out).unwrap()
}

fn base64_decode(s: &str) -> Vec<u8> {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut bits = 0u32;
    let mut n = 0;
    let mut out = Vec::new();
    for c in s.bytes().filter(|&c| c != b'=') {
        bits = bits << 6 | A.iter().position(|&a| a == c).unwrap() as u32;
        n += 6;
        if n >= 8 {
            n -= 8;
            out.push((bits >> n) as u8);
            bits &= (1 << n) - 1;
        }
    }
    out
}

// --- The merge ---------------------------------------------------------

/// The exact file a fresh config becomes.
const FRESH: &str = r#"{
  "mcpServers": {
    "neoscad": {
      "command": "/home/someone/Library/Application Support/NeoSCAD/bin/neoscad",
      "args": [
        "mcp"
      ]
    }
  }
}
"#;

#[test]
fn no_file_empty_file_and_whitespace_become_a_fresh_config() {
    for existing in [None, Some(""), Some("  \n\t\n")] {
        let m = merge_claude_desktop_config(existing, &mac()).unwrap();
        assert_eq!(m.text, FRESH, "from {existing:?}");
        assert_eq!(m.change, ConfigChange::Added);
        assert_eq!(m.previous, None);
    }
    // An empty object and an empty mcpServers add the same entry.
    for existing in ["{}", "{\"mcpServers\": {}}"] {
        let m = merge_claude_desktop_config(Some(existing), &mac()).unwrap();
        assert_eq!(m.text, FRESH, "from {existing}");
    }
}

/// A config with other servers and other settings, keys deliberately not
/// in alphabetical order, with a float, a large integer, escapes and
/// non-ASCII text.
const OTHERS: &str = r#"{
  "preferences": {"sidebarMode": "chat", "zoom": 1.25, "quickEntryShortcut": "off"},
  "mcpServers": {
    "zeta": {"command": "npx", "args": ["-y", "@example/zeta"], "env": {"TOKEN": "a\"b\\c"}},
    "filesystem": {"command": "npx", "args": ["-y", "@modelcontextprotocol/server-filesystem", "/home/someone/Desktop"]}
  },
  "isUsingBuiltInNodeForMcp": true,
  "big": 12345678901234567890,
  "name": "Grüße ✓",
  "nothing": null
}"#;

#[test]
fn other_servers_and_settings_are_kept_in_their_order() {
    let m = merge_claude_desktop_config(Some(OTHERS), &mac()).unwrap();
    assert_eq!(m.change, ConfigChange::Added);
    // Everything that was there means the same, and only neoscad is new.
    let mut want = value(OTHERS);
    want["mcpServers"]["neoscad"] = serde_json::json!({"command": MAC_CLI, "args": ["mcp"]});
    assert_eq!(value(&m.text), want);
    // And the keys come out in the file's order, neoscad last.
    let order = |text: &str, keys: &[&str]| {
        let at: Vec<usize> = keys
            .iter()
            .map(|k| text.find(&format!("\"{k}\"")).unwrap())
            .collect();
        assert!(
            at.windows(2).all(|w| w[0] < w[1]),
            "{keys:?} out of order in\n{text}"
        );
    };
    order(
        &m.text,
        &[
            "preferences",
            "sidebarMode",
            "zoom",
            "quickEntryShortcut",
            "mcpServers",
            "zeta",
            "filesystem",
            "neoscad",
            "isUsingBuiltInNodeForMcp",
            "big",
            "name",
            "nothing",
        ],
    );
    assert!(m.text.contains("12345678901234567890"), "{}", m.text);
    assert!(m.text.contains("1.25"));
    assert!(m.text.contains("Grüße ✓"));
    assert!(m.text.ends_with("}\n"));
    // Merging again changes nothing.
    let again = merge_claude_desktop_config(Some(&m.text), &mac()).unwrap();
    assert_eq!(again.change, ConfigChange::Unchanged);
    assert_eq!(again.text, m.text);
}

#[test]
fn an_existing_entry_is_updated_in_place_keeping_its_other_fields() {
    let existing = r#"{"mcpServers": {
        "neoscad": {"command": "neoscad", "args": ["mcp", "--browser"], "env": {"OPENSCADPATH": "/libs"}},
        "other": {"command": "x"}
    }}"#;
    let m = merge_claude_desktop_config(Some(existing), &mac()).unwrap();
    assert_eq!(m.change, ConfigChange::Replaced);
    assert_eq!(
        m.previous.as_deref(),
        Some(r#"{"command":"neoscad","args":["mcp","--browser"],"env":{"OPENSCADPATH":"/libs"}}"#)
    );
    assert_eq!(
        value(&m.text),
        serde_json::json!({"mcpServers": {
            "neoscad": {"command": MAC_CLI, "args": ["mcp"], "env": {"OPENSCADPATH": "/libs"}},
            "other": {"command": "x"}
        }})
    );
    // Still before "other".
    assert!(m.text.find("\"neoscad\"").unwrap() < m.text.find("\"other\"").unwrap());

    // The same command already: nothing to write.
    let same =
        format!(r#"{{"mcpServers": {{"neoscad": {{"command": "{MAC_CLI}", "args": ["mcp"]}}}}}}"#);
    let m = merge_claude_desktop_config(Some(&same), &mac()).unwrap();
    assert_eq!(m.change, ConfigChange::Unchanged);
    assert_eq!(m.previous, None);

    // An entry missing its args gets them.
    let partial = format!(r#"{{"mcpServers": {{"neoscad": {{"command": "{MAC_CLI}"}}}}}}"#);
    let m = merge_claude_desktop_config(Some(&partial), &mac()).unwrap();
    assert_eq!(m.change, ConfigChange::Replaced);
    assert_eq!(
        value(&m.text)["mcpServers"]["neoscad"]["args"],
        serde_json::json!(["mcp"])
    );
}

#[test]
fn files_it_does_not_understand_are_refused_not_clobbered() {
    // Comments and trailing commas: what a hand-edited file often has,
    // and what Claude Desktop's strict JSON reader refuses too.
    let commented = "{\n  // my servers\n  \"mcpServers\": {}\n}";
    match merge_claude_desktop_config(Some(commented), &mac()) {
        Err(ConfigError::InvalidJson { line, .. }) => assert_eq!(line, 2),
        other => panic!("{other:?}"),
    }
    for bad in [
        "{\"mcpServers\": {},}",
        "{\"mcpServers\": {\"a\": {\"command\": \"x\"}}",
        "not json",
        "{\"a\": 1} trailing",
    ] {
        assert!(
            matches!(
                merge_claude_desktop_config(Some(bad), &mac()),
                Err(ConfigError::InvalidJson { .. })
            ),
            "{bad}"
        );
    }
    assert_eq!(
        merge_claude_desktop_config(Some("[1, 2]"), &mac()),
        Err(ConfigError::NotAnObject)
    );
    assert_eq!(
        merge_claude_desktop_config(Some("\"text\""), &mac()),
        Err(ConfigError::NotAnObject)
    );
    assert_eq!(
        merge_claude_desktop_config(Some(r#"{"mcpServers": []}"#), &mac()),
        Err(ConfigError::ServersNotAnObject)
    );
    assert_eq!(
        merge_claude_desktop_config(Some(r#"{"mcpServers": {"neoscad": "x"}}"#), &mac()),
        Err(ConfigError::EntryNotAnObject)
    );
    let e = merge_claude_desktop_config(Some("{,}"), &mac()).unwrap_err();
    assert!(
        e.to_string()
            .starts_with("the file is not valid JSON (line 1"),
        "{e}"
    );
}

#[test]
fn a_duplicate_key_edits_its_last_occurrence() {
    // JSON.parse and serde_json both keep the last; so does Claude.
    let dup = r#"{"mcpServers": {"a": {}}, "mcpServers": {"b": {}}}"#;
    let m = merge_claude_desktop_config(Some(dup), &mac()).unwrap();
    let second = &m.text[m.text.rfind("\"mcpServers\"").unwrap()..];
    assert!(
        second.contains("\"b\"") && second.contains("\"neoscad\""),
        "{}",
        m.text
    );
    let first = &m.text[..m.text.rfind("\"mcpServers\"").unwrap()];
    assert!(!first.contains("neoscad"), "{}", m.text);
}

#[test]
fn backup_names_are_utc_stamped_and_numbered() {
    let f = "claude_desktop_config.json";
    assert_eq!(
        backup_file_name(f, 0, 0),
        format!("{f}.neoscad-backup-19700101T000000Z")
    );
    // A leap day, and a time of day.
    assert_eq!(
        backup_file_name(f, 951_782_400, 0),
        format!("{f}.neoscad-backup-20000229T000000Z")
    );
    assert_eq!(
        backup_file_name(f, 1_791_037_805, 0),
        format!("{f}.neoscad-backup-20261003T143005Z")
    );
    assert_eq!(
        backup_file_name(f, 1_791_037_805, 2),
        format!("{f}.neoscad-backup-20261003T143005Z-2")
    );
    // Before 1970 (a wrong clock) still gives a valid name.
    assert_eq!(
        backup_file_name(f, -1, 0),
        format!("{f}.neoscad-backup-19691231T235959Z")
    );
}

// --- Links and commands ---------------------------------------------------

#[test]
fn base64_matches_cursors_documented_example() {
    // cursor.com/docs/context/mcp/install-links, the postgres example.
    let config = r#"{"command":"npx","args":["-y","@modelcontextprotocol/server-postgres","postgresql://localhost/mydb"]}"#;
    assert_eq!(
        base64(config.as_bytes()),
        "eyJjb21tYW5kIjoibnB4IiwiYXJncyI6WyIteSIsIkBtb2RlbGNvbnRleHRwcm90b2NvbC9zZXJ2ZXItcG9zdGdyZXMiLCJwb3N0Z3Jlc3FsOi8vbG9jYWxob3N0L215ZGIiXX0="
    );
    assert_eq!(base64(b""), "");
    assert_eq!(base64(b"f"), "Zg==");
    assert_eq!(base64(b"fo"), "Zm8=");
    assert_eq!(base64(b"foo"), "Zm9v");
}

#[test]
fn cursor_link_carries_the_entry_as_base64() {
    let url = cursor_install_url(&mac());
    let rest = url
        .strip_prefix("cursor://anysphere.cursor-deeplink/mcp/install?name=neoscad&config=")
        .unwrap();
    // URL-safe as a query value: no raw + / = to be misread.
    assert!(!rest.contains(['+', '/', '=', ' ']), "{rest}");
    let json = String::from_utf8(base64_decode(&percent_decode(rest))).unwrap();
    assert_eq!(
        value(&json),
        serde_json::json!({"command": MAC_CLI, "args": ["mcp"]})
    );
}

#[test]
fn vscode_link_carries_the_named_entry_url_encoded() {
    let url = vscode_install_url(&ServerCommand::bundled(WIN_CLI));
    let rest = url.strip_prefix("vscode:mcp/install?").unwrap();
    assert!(!rest.contains([' ', '"', '{', '\\']), "{rest}");
    let json = percent_decode(rest);
    assert!(
        json.starts_with(r#"{"name":"neoscad","type":"stdio","command":"#),
        "{json}"
    );
    assert_eq!(
        value(&json),
        serde_json::json!({"name": "neoscad", "type": "stdio", "command": WIN_CLI, "args": ["mcp"]})
    );
}

#[test]
fn claude_code_command_is_user_scope_with_the_absolute_path() {
    assert_eq!(
        claude_code_add_args(&mac()),
        [
            "mcp", "add", "--scope", "user", "neoscad", "--", MAC_CLI, "mcp"
        ]
    );
    assert_eq!(
        claude_code_remove_args(),
        ["mcp", "remove", "--scope", "user", "neoscad"]
    );
    assert!(claude_code_reports_existing(
        "MCP server neoscad already exists in user config\n"
    ));
    assert!(!claude_code_reports_existing(
        "Added stdio MCP server neoscad"
    ));

    let row = setup(
        Client::ClaudeCode,
        Host::MacOs,
        &mac(),
        "/home/someone",
        None,
    )
    .unwrap();
    assert_eq!(
        row.copy_text,
        "claude mcp add --scope user neoscad -- '/home/someone/Library/Application Support/NeoSCAD/bin/neoscad' mcp"
    );
    assert!(matches!(row.action, SetupAction::RunClaude { .. }));

    let win = ServerCommand::bundled(WIN_CLI);
    let row = setup(
        Client::ClaudeCode,
        Host::Windows,
        &win,
        r"C:\Users\someone",
        None,
    )
    .unwrap();
    assert_eq!(
        row.copy_text,
        r#"claude mcp add --scope user neoscad -- "C:\Program Files\NeoSCAD\bin\neoscad.exe" mcp"#
    );
    // A quote inside a POSIX path is escaped, not ended.
    assert_eq!(
        ServerCommand::bundled("/x/it's/neoscad").display(Host::Linux),
        r"'/x/it'\''s/neoscad' mcp"
    );
}

#[test]
fn claude_is_looked_for_beyond_a_gui_apps_path() {
    // launchd's PATH for a GUI app.
    let c = claude_code_candidates(
        Host::MacOs,
        "/home/someone",
        "/usr/bin:/bin:/usr/sbin:/sbin",
        None,
    );
    assert_eq!(c[0], "/usr/bin/claude");
    for want in [
        "/home/someone/.local/bin/claude",
        "/opt/homebrew/bin/claude",
        "/usr/local/bin/claude",
        "/home/someone/.npm-global/bin/claude",
    ] {
        assert!(c.contains(&want.to_string()), "{want} not in {c:?}");
    }
    // No duplicates when PATH already holds one of them.
    let c = claude_code_candidates(Host::Linux, "/home/u", "/home/u/.local/bin:/usr/bin", None);
    assert_eq!(
        c.iter()
            .filter(|p| *p == "/home/u/.local/bin/claude")
            .count(),
        1
    );
    assert!(!c.iter().any(|p| p.starts_with("/opt/homebrew")));

    let c = claude_code_candidates(
        Host::Windows,
        r"C:\Users\u",
        r"C:\Windows\system32;C:\Tools\",
        Some(r"C:\Users\u\AppData\Roaming"),
    );
    assert_eq!(c[0], r"C:\Windows\system32\claude.exe");
    assert!(c.contains(&r"C:\Tools\claude.cmd".to_string()));
    assert!(c.contains(&r"C:\Users\u\.local\bin\claude.exe".to_string()));
    assert!(c.contains(&r"C:\Users\u\AppData\Roaming\npm\claude.cmd".to_string()));
}

#[test]
fn claude_desktop_config_lives_where_its_docs_say() {
    assert_eq!(
        claude_desktop_config_path(Host::MacOs, "/home/someone/", None).as_deref(),
        Some("/home/someone/Library/Application Support/Claude/claude_desktop_config.json")
    );
    assert_eq!(
        claude_desktop_config_path(
            Host::Windows,
            r"C:\Users\u",
            Some(r"C:\Users\u\AppData\Roaming")
        )
        .as_deref(),
        Some(r"C:\Users\u\AppData\Roaming\Claude\claude_desktop_config.json")
    );
    assert_eq!(
        claude_desktop_config_path(Host::Windows, r"C:\Users\u", None),
        None
    );
    assert_eq!(
        claude_desktop_config_path(Host::Linux, "/home/u", None),
        None
    );
    assert_eq!(
        claude_desktop_config_path(Host::LinuxFlatpak, "/home/u", None),
        None
    );
}

#[test]
fn every_host_gets_its_rows() {
    let ids = |rows: &[ClientSetup]| rows.iter().map(|r| r.client).collect::<Vec<_>>();
    let rows = setups(Host::MacOs, &mac(), "/home/someone", None);
    assert_eq!(ids(&rows), Client::ALL);
    let desktop = &rows[1];
    assert_eq!(
        desktop.action,
        SetupAction::MergeConfig {
            path: "/home/someone/Library/Application Support/Claude/claude_desktop_config.json"
                .to_string()
        }
    );
    // Claude Desktop's server gets a folder to write in: the rest of the
    // JSON is the plain entry's.
    assert_eq!(
        value(&desktop.copy_text),
        serde_json::json!({"mcpServers": {"neoscad": {"command": MAC_CLI,
            "args": ["mcp", "--root", "/home/someone/Documents/NeoSCAD"]}}})
    );
    assert!(desktop.note.contains("reopen Claude"));
    assert!(desktop.note.contains("/home/someone/Documents/NeoSCAD"));
    for row in &rows {
        assert!(
            row.copy_text.contains(MAC_CLI) || row.copy_text.contains("Application Support"),
            "{row:?}"
        );
    }

    let win = ServerCommand::bundled(WIN_CLI);
    let rows = setups(
        Host::Windows,
        &win,
        r"C:\Users\u",
        Some(r"C:\Users\u\AppData\Roaming"),
    );
    assert_eq!(ids(&rows), Client::ALL);
    // Without the shell's Documents folder, the one in the profile.
    assert_eq!(
        value(&rows[1].copy_text)["mcpServers"]["neoscad"]["args"][2],
        r"C:\Users\u\Documents\NeoSCAD"
    );
    // A redirected Documents folder (OneDrive's backup) is the one used.
    let rows = setups_in(
        Host::Windows,
        &win,
        r"C:\Users\u",
        Some(r"C:\Users\u\AppData\Roaming"),
        r"C:\Users\u\OneDrive\Documents\",
    );
    assert_eq!(
        value(&rows[1].copy_text),
        serde_json::json!({"mcpServers": {"neoscad": {"command": WIN_CLI,
            "args": ["mcp", "--root", r"C:\Users\u\OneDrive\Documents\NeoSCAD"]}}})
    );
    // Only Claude Desktop's: the others start in a project.
    for r in rows.iter().filter(|r| r.client != Client::ClaudeDesktop) {
        assert!(!r.copy_text.contains("\"--root\""), "{r:?}");
    }

    // Linux: no Claude Desktop.
    let rows = setups(
        Host::Linux,
        &ServerCommand::bundled("/usr/bin/neoscad"),
        "/home/u",
        None,
    );
    assert_eq!(
        ids(&rows),
        [
            Client::ClaudeCode,
            Client::Cursor,
            Client::VsCode,
            Client::Other
        ]
    );
    assert!(matches!(rows[0].action, SetupAction::RunClaude { .. }));
}

#[test]
fn the_flatpak_copies_the_claude_command_and_runs_through_flatpak() {
    let server = ServerCommand::for_host(Host::LinuxFlatpak, "/app/bin/neoscad");
    assert_eq!(server, ServerCommand::flatpak());
    assert_eq!(
        server.display(Host::LinuxFlatpak),
        "flatpak run --command=neoscad org.neoscad.NeoSCAD mcp"
    );
    let rows = setups(Host::LinuxFlatpak, &server, "/home/u", None);
    assert_eq!(rows[0].client, Client::ClaudeCode);
    assert_eq!(rows[0].action, SetupAction::CopyOnly);
    assert_eq!(
        rows[0].copy_text,
        "claude mcp add --scope user neoscad -- flatpak run --command=neoscad org.neoscad.NeoSCAD mcp"
    );
    assert!(!rows.iter().any(|r| r.client == Client::ClaudeDesktop));
    let SetupAction::OpenUrl { url } = &rows[2].action else {
        panic!("{:?}", rows[2]);
    };
    let json = percent_decode(url.strip_prefix("vscode:mcp/install?").unwrap());
    assert_eq!(
        value(&json),
        serde_json::json!({"name": "neoscad", "type": "stdio", "command": "flatpak",
                           "args": ["run", "--command=neoscad", "org.neoscad.NeoSCAD", "mcp"]})
    );
}

#[test]
fn rows_serialize_as_tagged_records() {
    // What the web or a script would read: kebab-case tags.
    let row = setup(Client::VsCode, Host::MacOs, &mac(), "/home/someone", None).unwrap();
    let j = serde_json::to_value(&row).unwrap();
    assert_eq!(j["client"], "vs-code");
    assert_eq!(j["action"]["kind"], "open-url");
}

#[test]
fn claude_desktops_folder_is_in_documents() {
    assert_eq!(
        default_documents(Host::MacOs, "/Users/ada/"),
        "/Users/ada/Documents"
    );
    assert_eq!(
        default_documents(Host::Windows, r"C:\Users\ada"),
        r"C:\Users\ada\Documents"
    );
    assert_eq!(
        claude_desktop_folder(Host::MacOs, "/Users/ada/Documents/").as_deref(),
        Some("/Users/ada/Documents/NeoSCAD")
    );
    assert_eq!(
        claude_desktop_folder(Host::Windows, r"D:\Docs").as_deref(),
        Some(r"D:\Docs\NeoSCAD")
    );
    assert_eq!(
        claude_desktop_folder(Host::Linux, "/home/u/Documents"),
        None
    );
    assert_eq!(
        claude_desktop_server(Host::LinuxFlatpak, &mac(), "/home/u"),
        None
    );
    let s = claude_desktop_server(Host::MacOs, &mac(), "/Users/ada/Documents").unwrap();
    assert_eq!(s.args, ["mcp", "--root", "/Users/ada/Documents/NeoSCAD"]);
    assert_eq!(
        s.display(Host::MacOs),
        format!("'{MAC_CLI}' mcp --root /Users/ada/Documents/NeoSCAD")
    );
}

/// An entry from before the setup passed `--root` is found as outdated,
/// and the merge upgrades it in place, keeping its other fields.
#[test]
fn an_entry_without_the_root_is_outdated_and_upgraded() {
    let wanted = mac().with_root("/Users/ada/Documents/NeoSCAD");
    let old = format!(
        r#"{{"mcpServers": {{"neoscad": {{"command": "{MAC_CLI}", "args": ["mcp"], "env": {{"A": "1"}}}}}}}}"#
    );
    assert_eq!(
        claude_desktop_entry(None, &wanted),
        Ok(ClaudeDesktopEntry::Missing)
    );
    assert_eq!(
        claude_desktop_entry(Some("{\"mcpServers\": {\"x\": {}}}"), &wanted),
        Ok(ClaudeDesktopEntry::Missing)
    );
    assert_eq!(
        claude_desktop_entry(Some(&old), &wanted),
        Ok(ClaudeDesktopEntry::Outdated)
    );
    // Another folder is outdated too: the app's setup is the one wanted.
    let elsewhere = merge_claude_desktop_config(None, &mac().with_root("/tmp/x"))
        .unwrap()
        .text;
    assert_eq!(
        claude_desktop_entry(Some(&elsewhere), &wanted),
        Ok(ClaudeDesktopEntry::Outdated)
    );
    // A command of the user's own is theirs, not an old setup.
    assert_eq!(
        claude_desktop_entry(
            Some(r#"{"mcpServers": {"neoscad": {"command": "neoscad", "args": ["mcp"]}}}"#),
            &wanted
        ),
        Ok(ClaudeDesktopEntry::Other)
    );
    assert!(matches!(
        claude_desktop_entry(Some("{,}"), &wanted),
        Err(ConfigError::InvalidJson { .. })
    ));

    let m = merge_claude_desktop_config(Some(&old), &wanted).unwrap();
    assert_eq!(m.change, ConfigChange::Replaced);
    assert_eq!(
        value(&m.text),
        serde_json::json!({"mcpServers": {"neoscad": {"command": MAC_CLI,
            "args": ["mcp", "--root", "/Users/ada/Documents/NeoSCAD"], "env": {"A": "1"}}}})
    );
    assert_eq!(
        claude_desktop_entry(Some(&m.text), &wanted),
        Ok(ClaudeDesktopEntry::Current)
    );
}
