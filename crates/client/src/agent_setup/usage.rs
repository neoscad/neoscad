//! "Using NeoSCAD with your agent": what a user needs to know once a
//! client is set up, as data, so the macOS sheet, the Windows dialog and
//! the Linux page say the same thing in each host's own words (its keys,
//! the name and place of its agent control, whether it saves by itself).
//!
//! Every statement was checked against the code that makes it true
//! (2026-10-05); when that code changes, this text has to follow:
//!
//! - **Which document.** A request without a document acts on the most
//!   recently focused one; `editor_read` lists the others
//!   (`crates/cli/src/mcp/app.rs`, the module documentation). The server
//!   looks for apps every 2 s (`RESCAN` there) and then tells the client
//!   its tools changed, but only Claude Code is known to refetch them
//!   (`docs/followups.md`, "MCP and the agent eval"), so only Claude
//!   Code's text says the order does not matter. With no app, the editor
//!   and view tools refuse (`NO_APP`) and the model tools still work on
//!   files in the server's roots and on inline source.
//! - **Saving.** An agent's edit goes into the editor's buffer as one
//!   highlighted, undoable step (`SCADDocument+Agent.swift`, `agentApply`);
//!   nothing in the app saves for the agent. On macOS the document
//!   autosaves in place (`SCADDocument.swift`, `autosavesInPlace`), so
//!   the edit reaches the file the way the user's own typing does; the
//!   Windows and Linux apps have no autosave, so the file changes only
//!   when the user saves. A file changed on disk is taken in as an
//!   undoable step when the document is clean, and reported in a bar when
//!   it has unsaved changes (`SCADDocument+Disk.swift`, and the core's
//!   `DocumentFile` the other apps share).
//! - **Seeing.** The agent's edit arrives as an editor change like a
//!   keystroke, which schedules the document's run (`editorChanged`, then
//!   `schedulePreview`). Only the macOS app has the chip over the view
//!   that clears an agent's marks (`AgentMarksChip`), so only its text
//!   mentions it.
//! - **Exports.** `check`'s `export` with no `path` or `source` works on
//!   the open document, but writes only inside the server's roots: the
//!   working directory when it looks like a project, and each `--root`
//!   (`docs/mcp.md`, "Safety"). An open document's own folder is readable,
//!   not writable. Claude Desktop starts servers in an undefined folder
//!   ("like `/`"), which is never a root, so its setup passes `--root`
//!   with a NeoSCAD folder in the user's Documents
//!   (`super::claude_desktop_folder`), which the setup creates: its agent
//!   exports there, and relative paths resolve there
//!   (`crates/cli/src/mcp/roots.rs`, `Roots::with_unsafe_cwd`).
//!
//! Pure data: no file system, environment or clock.

use serde::{Deserialize, Serialize};

use super::{Client, Host};

/// What one item is about, so a host can give it an icon or find it in a
/// test without matching on its wording.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UsageTopic {
    /// Keep the app open; which document the agent works on.
    KeepOpen,
    /// Example requests ([`Usage::examples`]).
    WhatToAsk,
    /// Edits, undo and saving.
    Edits,
    /// The preview, the view, the marks and the status control.
    Seeing,
    /// Asking before edits, Disconnect, the switch.
    Control,
    /// Render and export.
    Export,
}

/// One headed item: a title and a sentence or two.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageItem {
    pub topic: UsageTopic,
    pub title: String,
    pub body: String,
}

/// The section for one client on one host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    /// In the order to show them.
    pub items: Vec<UsageItem>,
    /// Requests to try, shown under [`UsageTopic::WhatToAsk`]'s item.
    pub examples: Vec<String>,
}

/// What differs between the apps, in the words each one's menus use.
struct Words {
    save: &'static str,
    undo: &'static str,
    export: &'static str,
    /// Where the agent's status shows.
    control: &'static str,
    /// The "ask first" setting, and where it is.
    ask_first: &'static str,
    /// The document saves itself (macOS autosave in place).
    autosaves: bool,
    /// The view has a chip that clears the agent's marks.
    marks_chip: bool,
    /// Claude Desktop's folder (`super::claude_desktop_folder`), as the
    /// host's file manager spells it.
    desktop_folder: &'static str,
}

fn words(host: Host) -> Words {
    match host {
        Host::MacOs => Words {
            save: "⌘S",
            undo: "⌘Z",
            export: "File > Export… (⇧⌘E)",
            control: "The agent control in the toolbar",
            ask_first: "“Ask before applying edits” in the agent control’s popover or Settings > Agents",
            autosaves: true,
            marks_chip: true,
            desktop_folder: "Documents/NeoSCAD",
        },
        Host::Windows => Words {
            save: "Ctrl+S",
            undo: "Ctrl+Z",
            export: "File > Export… (Ctrl+Shift+E)",
            control: "The agent control at the end of the menu row",
            // The control opens a flyout, not a menu (MainWindow.Agents.cs,
            // `AgentFlyoutContent`), and the same setting is in the Help
            // menu, which is where it stays reachable once agents are off.
            ask_first: "“Ask me before applying the agent’s edits” in the agent control’s flyout or the Help menu",
            autosaves: false,
            marks_chip: false,
            desktop_folder: "Documents\\NeoSCAD",
        },
        Host::Linux | Host::LinuxFlatpak => Words {
            save: "Ctrl+S",
            undo: "Ctrl+Z",
            export: "Export in the main menu",
            control: "The agent button in the header bar",
            ask_first: "“Ask before applying an agent’s edits” in Preferences > Agents",
            autosaves: false,
            marks_chip: false,
            // No Claude Desktop on Linux; the macOS spelling, unused.
            desktop_folder: "Documents/NeoSCAD",
        },
    }
}

/// The requests the /try page's "Things to ask" offers
/// (`web/src/ui/agent.js`, `IDEAS`), chosen per client, plus one that
/// exports: every client can write files now that Claude Desktop's setup
/// gives it a folder.
fn examples(client: Client) -> Vec<String> {
    let list: &[&str] = match client {
        Client::ClaudeDesktop => &[
            "Make the teeth smaller and show me the result.",
            "Walk me through this model, pointing at each part in the 3D view.",
            "Why won’t this print? Mark the problem spots in the view.",
            "Turn the fixed sizes into customizer parameters.",
            "Check it prints, then export it as an STL.",
        ],
        Client::ClaudeCode | Client::Cursor | Client::VsCode | Client::Other => &[
            "Make the teeth smaller and show me the result.",
            "Why won’t this print? Mark the problem spots in the view.",
            "Add M3 mounting holes in each corner, then check it still prints.",
            "Check it prints, then export it as an STL.",
        ],
    };
    list.iter().map(|s| s.to_string()).collect()
}

/// "Using NeoSCAD with your agent" for `client` on `host`.
pub fn usage(host: Host, client: Client) -> Usage {
    let w = words(host);
    let item = |topic, title: &str, body: String| UsageItem {
        topic,
        title: title.to_string(),
        body,
    };

    let closed = match client {
        Client::ClaudeDesktop => format!(
            "With NeoSCAD closed, the agent can still check models in the chat or in {}, \
             but can’t see or change your windows.",
            w.desktop_folder
        ),
        _ => "With NeoSCAD closed, the agent can still check .scad files in its project, \
              but can’t see or change your windows."
            .to_string(),
    };
    // Only Claude Code is documented to refetch the tool list when the
    // server says it changed (docs/agent-bridge.md, "How it connects",
    // step 5); another client started before NeoSCAD may never see the
    // editor and view tools, so those are told to open NeoSCAD first.
    let order = match client {
        Client::ClaudeCode => {
            "Open NeoSCAD before or after starting Claude Code; they connect by themselves."
        }
        Client::ClaudeDesktop => {
            "Open NeoSCAD before Claude; if Claude can’t see your model, quit and reopen Claude."
        }
        _ => {
            "Open NeoSCAD before starting the agent; if it can’t see your model, restart the agent."
        }
    };
    let keep_open = format!(
        "The agent works on the models open in NeoSCAD while agents are allowed: the window \
         you used last, unless you name another (it can list them). {order} {closed}"
    );

    let saving = if w.autosaves {
        format!(
            "They’re saved like your own typing: NeoSCAD saves a document that has a file by \
             itself, and {} saves now.",
            w.save
        )
    } else {
        format!("The file changes only when you save ({}).", w.save)
    };
    let edits = format!(
        "Each edit lands in the editor highlighted, as one step that {} undoes. {saving} If the \
         agent rewrites the file on disk instead, NeoSCAD takes the change in as one undoable \
         step, or asks first when you have unsaved changes.",
        w.undo
    );

    let marks = if w.marks_chip {
        "; the chip over the view clears the marks"
    } else {
        ""
    };
    let seeing = format!(
        "The 3D view previews each edit, as it does your typing. The agent can look at the \
         view, turn the camera and mark spots in it{marks}. {} shows what it’s doing.",
        w.control
    );

    let control = format!(
        "Turn on {} to approve each edit first. Disconnect ends one agent’s connection until \
         its session restarts; turning off “Allow AI agents to work on open documents” \
         disconnects them all.",
        w.ask_first
    );

    let ask_agent = match client {
        Client::ClaudeDesktop => format!(
            "Or ask the agent to check and export it: it saves the file in {}.",
            w.desktop_folder
        ),
        Client::Other => "Or ask the agent to check and export it: it can write into the folder \
                          your client starts NeoSCAD in, when that is a project, and any --root."
            .to_string(),
        _ => "Or ask the agent to check and export it: it writes the file into its project folder."
            .to_string(),
    };
    let export = format!(
        "Render (F6) builds the final model, and {} saves it as STL, 3MF and more. {ask_agent}",
        w.export
    );

    Usage {
        items: vec![
            item(UsageTopic::KeepOpen, "Keep NeoSCAD open", keep_open),
            item(
                UsageTopic::WhatToAsk,
                "Things to ask",
                "Ask about the model in front of you, in your own words. For example:".to_string(),
            ),
            item(UsageTopic::Edits, "Edits and saving", edits),
            item(UsageTopic::Seeing, "Watching it work", seeing),
            item(UsageTopic::Control, "Staying in control", control),
            item(UsageTopic::Export, "Rendering and exporting", export),
        ],
        examples: examples(client),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOSTS: [Host; 4] = [Host::MacOs, Host::Windows, Host::Linux, Host::LinuxFlatpak];

    fn body(u: &Usage, topic: UsageTopic) -> &str {
        &u.items.iter().find(|i| i.topic == topic).unwrap().body
    }

    #[test]
    fn every_host_and_client_has_the_six_items_in_order() {
        for host in HOSTS {
            for client in Client::ALL {
                let u = usage(host, client);
                let topics: Vec<_> = u.items.iter().map(|i| i.topic).collect();
                assert_eq!(
                    topics,
                    [
                        UsageTopic::KeepOpen,
                        UsageTopic::WhatToAsk,
                        UsageTopic::Edits,
                        UsageTopic::Seeing,
                        UsageTopic::Control,
                        UsageTopic::Export,
                    ],
                    "{host:?} {client:?}"
                );
                assert!((3..=5).contains(&u.examples.len()));
                for i in &u.items {
                    assert!(!i.title.is_empty());
                    // Short enough to scan: a sentence or two, three at most.
                    assert!(i.body.len() < 400, "{host:?} {client:?} {}", i.title);
                    assert!(!i.body.contains("  "), "{}", i.body);
                }
            }
        }
    }

    #[test]
    fn the_picker_shortens_only_other() {
        for c in Client::ALL {
            let want = if c == Client::Other {
                "Other"
            } else {
                c.label()
            };
            assert_eq!(c.short_label(), want);
        }
    }

    #[test]
    fn keys_are_the_hosts_own() {
        let mac = usage(Host::MacOs, Client::ClaudeCode);
        assert!(body(&mac, UsageTopic::Edits).contains("⌘Z"));
        assert!(body(&mac, UsageTopic::Edits).contains("⌘S"));
        assert!(body(&mac, UsageTopic::Export).contains("⇧⌘E"));
        for host in [Host::Windows, Host::Linux, Host::LinuxFlatpak] {
            let u = usage(host, Client::ClaudeCode);
            let edits = body(&u, UsageTopic::Edits);
            assert!(
                edits.contains("Ctrl+Z") && edits.contains("Ctrl+S"),
                "{host:?}"
            );
            assert!(!u.items.iter().any(|i| i.body.contains('⌘')), "{host:?}");
        }
    }

    #[test]
    fn only_macos_autosaves_and_has_the_marks_chip() {
        let mac = usage(Host::MacOs, Client::ClaudeCode);
        assert!(body(&mac, UsageTopic::Edits).contains("by itself"));
        assert!(body(&mac, UsageTopic::Seeing).contains("chip"));
        let win = usage(Host::Windows, Client::ClaudeCode);
        assert!(body(&win, UsageTopic::Edits).contains("only when you save"));
        assert!(!body(&win, UsageTopic::Seeing).contains("chip"));
    }

    #[test]
    fn windows_names_the_flyout_and_the_help_menu() {
        // The Windows agent control opens a flyout, and the Help menu has
        // the same switch; "the agent control's menu" sent users looking
        // for a menu the control doesn't have.
        let win = usage(Host::Windows, Client::ClaudeCode);
        let control = body(&win, UsageTopic::Control);
        assert!(
            control.contains("flyout") && control.contains("Help menu"),
            "{control}"
        );
    }

    /// Claude Desktop's setup passes `--root` with its Documents folder,
    /// so its agent exports there, and each host names it its own way.
    #[test]
    fn claude_desktop_exports_into_its_documents_folder() {
        let d = usage(Host::MacOs, Client::ClaudeDesktop);
        let export = body(&d, UsageTopic::Export);
        assert!(
            export.contains("saves the file in Documents/NeoSCAD"),
            "{export}"
        );
        assert!(!export.contains("can’t write"), "{export}");
        assert!(d.examples.iter().any(|e| e.contains("export")));
        assert!(body(&d, UsageTopic::KeepOpen).contains("Documents/NeoSCAD"));
        let w = usage(Host::Windows, Client::ClaudeDesktop);
        assert!(body(&w, UsageTopic::Export).contains("Documents\\NeoSCAD"));
        assert!(!w.items.iter().any(|i| i.body.contains("Documents/")));
        let c = usage(Host::MacOs, Client::ClaudeCode);
        assert!(body(&c, UsageTopic::Export).contains("project folder"));
        assert!(c.examples.iter().any(|e| e.contains("export")));
    }

    #[test]
    fn examples_come_from_the_web_pages_list() {
        // The /try page's "Things to ask" (web/src/ui/agent.js), which the
        // apps' examples must not drift from; the export request is the
        // apps' own.
        let web = include_str!("../../../../web/src/ui/agent.js");
        for client in Client::ALL {
            for e in usage(Host::MacOs, client).examples {
                if e.contains("export") {
                    continue;
                }
                let straight = e.replace('’', "'");
                assert!(web.contains(&straight), "{e}");
            }
        }
    }
}
