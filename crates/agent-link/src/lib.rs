//! The live link between a running NeoSCAD app and `neoscad mcp`
//! (docs/agent-bridge.md, "Desktop apps").
//!
//! An agent (Claude Code, Cursor, VS Code, Claude Desktop) runs `neoscad
//! mcp`. When the user has allowed agents in the app, the app listens on a
//! per-user local socket (a named pipe on Windows) in a well-known place
//! ([`discovery`]); plain `neoscad mcp` looks there, connects, and its
//! editor and view tools then act on the app's open document, as they act
//! on the web page with `--browser`. No TCP port, no token, no link: a
//! socket in a directory only this user can enter, or a pipe only this
//! user can open, admits only processes that can already read and write
//! the user's files ([`transport`]).
//!
//! A host crate, not a library one (`CLAUDE.md`, "Rules"): it uses
//! sockets, threads, `std::fs` and the environment. What the messages
//! mean and how an app answers them is `client::agent`, pure and shared;
//! this crate carries them:
//! - [`transport`]: the sockets and pipes, with their owner checks, which
//!   `neoscad serve --socket` uses too;
//! - [`discovery`]: where an app listens and where the command line looks;
//! - [`frame`]: one JSON message per line, bounded;
//! - [`AgentLink`]: the app's side, which the macOS and Windows apps reach
//!   through `crates/ffi` and the Linux app directly. It does nothing, and
//!   costs nothing, until the app calls [`AgentLink::start`] after the
//!   user's consent;
//! - [`setup`]: setting up agent clients to run the app's `neoscad`
//!   (finding and running `claude`, Claude Desktop's config), the part of
//!   the apps' "Connect your AI agent" sheet that needs the machine.
//!
//! The command line's side is `crates/cli/src/mcp/app.rs`.

pub mod discovery;
pub mod frame;
mod link;
pub mod setup;
pub mod transport;

pub use link::{AgentLink, AgentObserver, LinkConfig};
