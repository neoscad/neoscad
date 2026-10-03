//! Where `neoscad serve --socket` listens and how the command line reaches
//! it: a Unix socket on Unix, a named pipe on Windows
//! (docs/serve-protocol.md, "Platforms"). Both carry the same byte stream
//! (`Content-Length`-framed JSON-RPC), so `serve.rs` and `client.rs` see
//! only an [`Address`], a [`Listener`] and a [`Conn`].
//!
//! The transport itself, with its owner checks, is
//! `agent_link::transport`, shared with the desktop apps' agent link; this
//! module adds the server's default address. Each transport is
//! restricted to the user who started the server, since a client sends
//! its working directory, environment and command line and prints what
//! comes back (see that module for how).

use std::path::Path;

pub use agent_link::transport::{Address, Conn, ConnectError, connect, may_be_listening, remove};

/// Environment variable naming the server's address (instead of the
/// default).
pub const ADDRESS_ENV: &str = "NEOSCAD_SOCKET";

/// The address `--socket VALUE` (or `NEOSCAD_SOCKET`) names. On Windows a
/// bare name becomes `\\.\pipe\NAME`, so `--socket neoscad-test` works as
/// it reads; a value with the prefix is used as it is.
pub fn from_arg(value: &str) -> Address {
    agent_link::transport::from_arg(value)
}

/// The per-user default: `$NEOSCAD_SOCKET` when set, else the platform's
/// (see [`platform_default`]).
pub fn default_address() -> Address {
    match std::env::var(ADDRESS_ENV) {
        Ok(v) if !v.is_empty() => from_arg(&v),
        _ => platform_default(),
    }
}

/// `$XDG_RUNTIME_DIR/neoscad/serve.sock`, else
/// `<temp dir>/neoscad-<uid>/serve.sock` (on macOS the temp dir is
/// already per user).
#[cfg(not(windows))]
fn platform_default() -> Address {
    agent_link::transport::runtime_dir().join("serve.sock")
}

/// `\\.\pipe\neoscad-<SID>`: the user's security identifier names the
/// pipe, as the user id names the Unix default's directory. A user name
/// would do as well if it were always there and unique; the SID is both,
/// across domains too.
#[cfg(windows)]
fn platform_default() -> Address {
    let user = agent_link::transport::current_user_sid()
        .unwrap_or_else(|_| std::env::var("USERNAME").unwrap_or_else(|_| "user".to_string()));
    from_arg(&format!("neoscad-{user}"))
}

/// A listening server.
#[derive(Debug)]
pub struct Listener(agent_link::transport::Listener);

impl Listener {
    /// Listen at `address`, made private to this user. Fails when a server
    /// already listens there, and (on Unix) rather than replace a file that
    /// is not a socket. The default address's directory must be this
    /// user's alone; a socket placed elsewhere on purpose is only made
    /// user-only itself.
    pub fn bind(address: &Path) -> Result<Listener, String> {
        let private_dir = address == default_address();
        agent_link::transport::Listener::bind(address, private_dir).map(Listener)
    }

    /// Wait for the next client.
    pub fn accept(&self) -> std::io::Result<Conn> {
        self.0.accept()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// An address no other test (or test run) uses. On Unix, in a fresh
    /// private directory, as the server's own default is.
    fn scratch_address(name: &str) -> Address {
        static N: AtomicUsize = AtomicUsize::new(0);
        let tag = format!(
            "nst-{}-{}-{name}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        );
        if cfg!(windows) {
            from_arg(&tag)
        } else {
            // Short: socket paths are limited to about 100 bytes.
            let d = std::env::temp_dir().join(tag);
            let _ = std::fs::remove_dir_all(&d);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new().mode(0o700).create(&d).unwrap();
            }
            d.join("s.sock")
        }
    }

    fn cleanup(address: &Path) {
        remove(address);
        if cfg!(unix)
            && let Some(d) = address.parent()
        {
            let _ = std::fs::remove_dir_all(d);
        }
    }

    #[test]
    fn a_message_goes_both_ways() {
        let address = scratch_address("echo");
        let listener = Listener::bind(&address).unwrap();
        // The server side: answer each request with its own params, from
        // another thread than the reader (as serve.rs's requests do).
        let server = std::thread::spawn(move || {
            let conn = listener.accept().unwrap();
            let mut r = std::io::BufReader::new(conn.reader);
            let mut w = conn.writer;
            let msg = crate::rpc::read(&mut r).unwrap().unwrap();
            let reply = crate::rpc::response(&msg["id"], msg["params"].clone());
            std::thread::spawn(move || crate::rpc::write(&mut w, &reply).unwrap())
                .join()
                .unwrap();
            // The client's end of input ends the connection.
            assert!(crate::rpc::read(&mut r).unwrap().is_none());
        });
        assert!(may_be_listening(&address));
        let conn = connect(&address).unwrap();
        let mut w = conn.writer;
        let params = json!({"text": "cube(1);\n".repeat(2000)});
        crate::rpc::write(&mut w, &crate::rpc::request(7, "echo", params.clone())).unwrap();
        let mut r = std::io::BufReader::new(conn.reader);
        let reply = crate::rpc::read(&mut r).unwrap().unwrap();
        assert_eq!(reply["id"], 7);
        assert_eq!(reply["result"], params);
        drop(w);
        drop(r);
        server.join().unwrap();
        cleanup(&address);
    }

    #[test]
    fn a_second_server_at_the_same_address_is_refused() {
        let address = scratch_address("twice");
        let _first = Listener::bind(&address).unwrap();
        let e = Listener::bind(&address).unwrap_err();
        assert!(e.contains("already listening"), "{e}");
        cleanup(&address);
    }

    #[test]
    fn nothing_listening_is_no_server() {
        let address = scratch_address("absent");
        assert!(matches!(connect(&address), Err(ConnectError::NoServer)));
        cleanup(&address);
    }

    #[test]
    fn addresses_from_the_command_line() {
        if cfg!(windows) {
            assert_eq!(from_arg("x"), PathBuf::from(r"\\.\pipe\x"));
            assert_eq!(from_arg(r"\\.\pipe\x"), PathBuf::from(r"\\.\pipe\x"));
            assert_eq!(from_arg(r"\\.\PIPE\x"), PathBuf::from(r"\\.\PIPE\x"));
            let d = platform_default();
            let name = d.to_str().unwrap();
            assert!(name.starts_with(r"\\.\pipe\neoscad-S-1-"), "{name}");
        } else {
            assert_eq!(from_arg("dir/s.sock"), PathBuf::from("dir/s.sock"));
            assert!(platform_default().ends_with("serve.sock"));
        }
    }
}
