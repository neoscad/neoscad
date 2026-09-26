//! The command line as a client of a running `neoscad serve`: when a
//! server listens on the default socket, `neoscad IN -o OUT` exports and
//! `neoscad snapshot` send the work there, so an agent's edit loop gets
//! the server's warm caches instead of a cold process each time.
//!
//! The server's answer is an [`Outcome`]: exactly what the command would
//! have printed and its exit code. Anything that stops the server from
//! answering (no socket, a stale socket, a server of another build or
//! started in another environment, a dropped connection) makes the
//! command run in-process instead, silently, as if no server were there.
//!
//! `--no-server` (or `NEOSCAD_NO_SERVER=1` in the environment, which the
//! conformance harness sets) never asks.

use std::path::PathBuf;

use serde_json::{Value, json};

use crate::outcome::Outcome;
use crate::rpc;

/// Environment variable naming the socket (instead of the default).
pub const SOCKET_ENV: &str = "NEOSCAD_SOCKET";
/// Environment variable that turns the client off, like `--no-server`.
pub const NO_SERVER_ENV: &str = "NEOSCAD_NO_SERVER";

/// The per-user default socket: `$NEOSCAD_SOCKET`, else
/// `$XDG_RUNTIME_DIR/neoscad/serve.sock`, else
/// `<temp dir>/neoscad-<uid>/serve.sock` (on macOS the temp dir is
/// already per user).
pub fn default_socket() -> PathBuf {
    if let Some(p) = std::env::var_os(SOCKET_ENV).filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    if let Some(d) = std::env::var_os("XDG_RUNTIME_DIR").filter(|p| !p.is_empty()) {
        return PathBuf::from(d).join("neoscad").join("serve.sock");
    }
    std::env::temp_dir()
        .join(format!("neoscad-{}", uid()))
        .join("serve.sock")
}

#[cfg(unix)]
fn uid() -> u32 {
    nix::unistd::getuid().as_raw()
}

#[cfg(not(unix))]
fn uid() -> u32 {
    0
}

/// Whether the client should try the server at all.
pub fn wanted(no_server: bool) -> bool {
    !no_server && std::env::var_os(NO_SERVER_ENV).is_none_or(|v| v.is_empty() || v == "0")
}

/// What identifies this build: a server of another build may print other
/// bytes, so a client only uses a server of its own build.
pub fn binary_id() -> String {
    let exe = std::env::current_exe().ok();
    let meta = exe.as_ref().and_then(|e| std::fs::metadata(e).ok());
    let mtime = meta
        .as_ref()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    format!(
        "{} {} {} {}",
        env!("CARGO_PKG_VERSION"),
        exe.map(|e| e.display().to_string()).unwrap_or_default(),
        meta.map_or(0, |m| m.len()),
        mtime
    )
}

/// The environment a run's results depend on beyond its arguments: the
/// library path and the fonts. A server started in another environment
/// would resolve `include <...>` and fonts differently. Search paths are
/// resolved against the working directory, as `LibraryPath::from_env` and
/// `Host::font_sources` resolve them, so a relative `OPENSCADPATH` only
/// matches a server that resolved it to the same directories.
pub fn environment() -> Value {
    let var = |k: &str| std::env::var_os(k).map(|v| v.to_string_lossy().into_owned());
    let cwd = std::env::current_dir().unwrap_or_default();
    let sep = if cfg!(windows) { ';' } else { ':' };
    let resolved = |k: &str| {
        var(k).map(|v| {
            v.split(sep)
                .map(|p| cwd.join(p).to_string_lossy().into_owned())
                .collect::<Vec<_>>()
        })
    };
    json!({
        "OPENSCADPATH": resolved("OPENSCADPATH"),
        "OPENSCAD_FONT_PATH": resolved("OPENSCAD_FONT_PATH"),
        "NEOSCAD_FONT_DIR": var(crate::host::FONT_DIR_ENV)
            .map(|v| cwd.join(v).to_string_lossy().into_owned()),
        "HOME": var("HOME"),
    })
}

/// Why a call did not produce an answer.
#[derive(Debug)]
pub enum CallError {
    /// No server is listening.
    NoServer,
    /// The server answered with a JSON-RPC error.
    Rpc(i64, String),
    /// The connection failed part-way.
    Io(String),
    /// The socket is not one this user's server made (see [`trusted`]).
    Untrusted(String),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CallError::NoServer => f.write_str("no server is running"),
            CallError::Rpc(c, m) => write!(f, "{m} ({c})"),
            CallError::Io(e) => f.write_str(e),
            CallError::Untrusted(e) => f.write_str(e),
        }
    }
}

/// Call `method` on the server at `socket` and wait for its result,
/// skipping notifications.
#[cfg(unix)]
pub fn call(socket: &std::path::Path, method: &str, params: Value) -> Result<Value, CallError> {
    use std::io::BufReader;
    use std::os::unix::net::UnixStream;
    if !socket.exists() {
        return Err(CallError::NoServer);
    }
    trusted(socket).map_err(CallError::Untrusted)?;
    let stream = UnixStream::connect(socket).map_err(|_| CallError::NoServer)?;
    let mut w = stream
        .try_clone()
        .map_err(|e| CallError::Io(e.to_string()))?;
    rpc::write(&mut w, &rpc::request(1, method, params))
        .map_err(|e| CallError::Io(e.to_string()))?;
    let mut r = BufReader::new(stream);
    loop {
        let msg = rpc::read(&mut r)
            .map_err(|e| CallError::Io(e.to_string()))?
            .ok_or_else(|| CallError::Io("the server closed the connection".into()))?;
        if msg.get("id") != Some(&json!(1)) {
            continue;
        }
        if let Some(e) = msg.get("error") {
            return Err(CallError::Rpc(
                e["code"].as_i64().unwrap_or(rpc::code::INTERNAL_ERROR),
                e["message"].as_str().unwrap_or("").to_string(),
            ));
        }
        return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
    }
}

/// Whether `socket` is this user's server's, before anything is sent to
/// it: the client sends its working directory, environment (`HOME`,
/// library and font paths) and command line, and prints what comes back.
/// The socket must be a socket owned by this user, in a directory that is
/// this user's and not writable by group or others (or a sticky one, like
/// `/tmp`, where no one else can replace a socket that is ours).
///
/// Without this, on Linux without `XDG_RUNTIME_DIR` another local user
/// could create `/tmp/neoscad-<uid>/` first, listen there, and receive a
/// victim's paths and environment and forge their output (the
/// agent-surface audit's finding 8). The server checks the default
/// directory's owner when it starts; the client now checks too, on every
/// platform, and refuses (running in-process) otherwise.
#[cfg(unix)]
pub fn trusted(socket: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let me = uid();
    let s = std::fs::symlink_metadata(socket).map_err(|e| format!("{}: {e}", socket.display()))?;
    if !s.file_type().is_socket() {
        return Err(format!("{} is not a socket", socket.display()));
    }
    if s.uid() != me {
        return Err(format!("{} belongs to another user", socket.display()));
    }
    let dir = socket
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    let d = std::fs::metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let private = d.uid() == me && d.mode() & 0o022 == 0;
    let sticky = d.mode() & 0o1000 != 0;
    if !(private || sticky) {
        return Err(format!(
            "{} is writable by other users or not this user's; not trusting the socket in it",
            dir.display()
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
pub fn trusted(_socket: &std::path::Path) -> Result<(), String> {
    Err("Unix sockets are not available here".into())
}

#[cfg(not(unix))]
pub fn call(_socket: &std::path::Path, _method: &str, _params: Value) -> Result<Value, CallError> {
    Err(CallError::NoServer)
}

/// The socket of a server the command line may use: `None` without one
/// (checked first, so a run with no server pays one `stat`).
pub fn available(no_server: bool) -> Option<PathBuf> {
    if !wanted(no_server) {
        return None;
    }
    let socket = default_socket();
    (socket.exists() && trusted(&socket).is_ok()).then_some(socket)
}

/// Run a command-line method (`cli.export`, `cli.snapshot`) on the server
/// at `socket`. `None` means: run it here.
pub fn run(socket: &std::path::Path, method: &str, mut params: Value) -> Option<Outcome> {
    params["binary"] = json!(binary_id());
    params["environment"] = environment();
    params["cwd"] = json!(std::env::current_dir().ok()?.to_string_lossy());
    // No server, or one that cannot take this run (another build, another
    // environment, a dropped connection): do the work here, silently, so
    // the output is exactly a local run's. `neoscad serve --status` shows
    // what is running.
    call(socket, method, params)
        .ok()
        .and_then(|v| Outcome::from_json(&v))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn only_a_private_directorys_socket_is_trusted() {
        let d = std::env::temp_dir().join(format!("nstrust-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let sock = d.join("s.sock");
        let _l = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(trusted(&sock).is_ok());
        // Anyone could have put it there.
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            trusted(&sock)
                .unwrap_err()
                .contains("writable by other users")
        );
        // Sticky (like /tmp): no one else can replace a socket that is ours.
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o1777)).unwrap();
        assert!(trusted(&sock).is_ok());
        // Not a socket at all.
        std::fs::write(d.join("f"), "x").unwrap();
        assert!(trusted(&d.join("f")).unwrap_err().contains("not a socket"));
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }
}
