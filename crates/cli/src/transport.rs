//! Where `neoscad serve --socket` listens and how the command line reaches
//! it: a Unix socket on Unix, a named pipe on Windows
//! (docs/serve-protocol.md, "Platforms"). Both carry the same byte stream
//! (`Content-Length`-framed JSON-RPC), so `serve.rs` and `client.rs` see
//! only an [`Address`], a [`Listener`] and a [`Conn`].
//!
//! Each transport is restricted to the user who started the server, since
//! a client sends its working directory, environment and command line and
//! prints what comes back:
//! - Unix: the socket is 0600 in a directory that must be the user's
//!   (0700 for the default one), and a client trusts a socket only when it
//!   and its directory pass `imp::trusted`.
//! - Windows: the pipe's security descriptor names the user as its owner
//!   and grants access to no one else, and a client proceeds only when the
//!   pipe it opened is owned by its own user. Pipe names are one global
//!   namespace, so another user can create `\\.\pipe\neoscad-<SID>` first;
//!   without the owner check the client would hand that user its paths and
//!   print their forged output.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// A server's address: a socket path on Unix, a pipe name
/// (`\\.\pipe\...`) on Windows. A `PathBuf` on both, so the protocol's
/// `status` and the command line's messages show it the same way.
pub type Address = PathBuf;

/// Environment variable naming the server's address (instead of the
/// default).
pub const ADDRESS_ENV: &str = "NEOSCAD_SOCKET";

/// One connection's two directions. Separate objects, because a
/// connection's requests run on their own threads and write their answers
/// while its reader waits for the next message.
pub struct Conn {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
}

impl std::fmt::Debug for Conn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Conn")
    }
}

/// Why a client could not reach a server.
#[derive(Debug)]
pub enum ConnectError {
    /// Nothing listens at the address.
    NoServer,
    /// Something listens, but not this user's server.
    Untrusted(String),
}

/// The address `--socket VALUE` (or `NEOSCAD_SOCKET`) names. On Windows a
/// bare name becomes `\\.\pipe\NAME`, so `--socket neoscad-test` works as
/// it reads; a value with the prefix is used as it is.
pub fn from_arg(value: &str) -> Address {
    imp::from_arg(value)
}

/// The per-user default: `$NEOSCAD_SOCKET` when set, else the platform's
/// (see `imp::default_address`).
pub fn default_address() -> Address {
    match std::env::var(ADDRESS_ENV) {
        Ok(v) if !v.is_empty() => from_arg(&v),
        _ => imp::default_address(),
    }
}

/// Whether a server may be listening at `address`, as cheaply as the
/// platform allows: the command line asks before every export, and a run
/// with no server should pay one `stat`. On Windows there is no way to ask
/// without connecting (opening a pipe takes an instance), so this is always
/// true there and [`connect`] answers.
pub fn may_be_listening(address: &Path) -> bool {
    imp::may_be_listening(address)
}

/// Connect to the server at `address`, checking first that it is this
/// user's (nothing is sent before the check).
pub fn connect(address: &Path) -> Result<Conn, ConnectError> {
    imp::connect(address)
}

/// A listening server.
#[derive(Debug)]
pub struct Listener(imp::Listener);

impl Listener {
    /// Listen at `address`, made private to this user. Fails when a server
    /// already listens there, and (on Unix) rather than replace a file that
    /// is not a socket.
    pub fn bind(address: &Path) -> Result<Listener, String> {
        imp::bind(address).map(Listener)
    }

    /// Wait for the next client.
    pub fn accept(&self) -> std::io::Result<Conn> {
        self.0.accept()
    }
}

/// Remove what the listener left behind (the socket file on Unix; a pipe
/// goes when its handles close).
pub fn remove(address: &Path) {
    imp::remove(address);
}

#[cfg(unix)]
mod imp {
    use super::{Address, Conn, ConnectError};
    use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};

    pub fn from_arg(value: &str) -> Address {
        PathBuf::from(value)
    }

    /// `$XDG_RUNTIME_DIR/neoscad/serve.sock`, else
    /// `<temp dir>/neoscad-<uid>/serve.sock` (on macOS the temp dir is
    /// already per user).
    pub fn default_address() -> Address {
        if let Some(d) = std::env::var_os("XDG_RUNTIME_DIR").filter(|p| !p.is_empty()) {
            return PathBuf::from(d).join("neoscad").join("serve.sock");
        }
        std::env::temp_dir()
            .join(format!("neoscad-{}", uid()))
            .join("serve.sock")
    }

    fn uid() -> u32 {
        nix::unistd::getuid().as_raw()
    }

    pub fn may_be_listening(address: &Path) -> bool {
        address.exists() && trusted(address).is_ok()
    }

    pub fn connect(address: &Path) -> Result<Conn, ConnectError> {
        if !address.exists() {
            return Err(ConnectError::NoServer);
        }
        trusted(address).map_err(ConnectError::Untrusted)?;
        let stream = UnixStream::connect(address).map_err(|_| ConnectError::NoServer)?;
        let writer = stream.try_clone().map_err(|_| ConnectError::NoServer)?;
        Ok(Conn {
            reader: Box::new(stream),
            writer: Box::new(writer),
        })
    }

    /// Whether `socket` is this user's server's, before anything is sent
    /// to it. The socket must be a socket owned by this user, in a
    /// directory that is this user's and not writable by group or others
    /// (or a sticky one, like `/tmp`, where no one else can replace a
    /// socket that is ours).
    ///
    /// Without this, on Linux without `XDG_RUNTIME_DIR` another local user
    /// could create `/tmp/neoscad-<uid>/` first, listen there, and receive
    /// a victim's paths and environment and forge their output (the
    /// agent-surface audit's finding 8). The server checks the default
    /// directory's owner when it starts; the client checks too, and
    /// refuses (running in-process) otherwise.
    pub fn trusted(socket: &Path) -> Result<(), String> {
        let me = uid();
        let s =
            std::fs::symlink_metadata(socket).map_err(|e| format!("{}: {e}", socket.display()))?;
        if !s.file_type().is_socket() {
            return Err(format!("{} is not a socket", socket.display()));
        }
        if s.uid() != me {
            return Err(format!("{} belongs to another user", socket.display()));
        }
        let dir = socket
            .parent()
            .filter(|d| !d.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
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

    #[derive(Debug)]
    pub struct Listener(UnixListener);

    pub fn bind(path: &Path) -> Result<Listener, String> {
        let dir = path
            .parent()
            .ok_or_else(|| format!("no directory in socket path {}", path.display()))?;
        if !dir.exists() {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(dir)
                .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        }
        // The socket is how anyone talks to this user's server. The
        // default one lives in a directory that must be this user's alone;
        // a socket placed elsewhere on purpose is only made user-only
        // itself.
        if path == super::default_address() {
            let meta = std::fs::metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            if meta.uid() != uid() {
                return Err(format!("{} belongs to another user", dir.display()));
            }
            if meta.mode() & 0o077 != 0 {
                std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                    .map_err(|e| format!("cannot make {} private: {e}", dir.display()))?;
            }
        }
        if let Ok(meta) = std::fs::symlink_metadata(path) {
            // Only a socket is ever replaced: `--socket notes.txt` (or
            // `--socket model.scad`) deleted the file as a "stale socket"
            // (the agent-surface audit's finding 7).
            if !meta.file_type().is_socket() {
                return Err(format!(
                    "{} exists and is not a socket; refusing to replace it (choose another --socket path)",
                    path.display()
                ));
            }
            if UnixStream::connect(path).is_ok() {
                return Err(format!(
                    "a server is already listening at {}",
                    path.display()
                ));
            }
            // Left by a server that did not exit cleanly.
            std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        let listener = UnixListener::bind(path)
            .map_err(|e| format!("cannot listen at {}: {e}", path.display()))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Listener(listener))
    }

    impl Listener {
        pub fn accept(&self) -> std::io::Result<Conn> {
            let (stream, _) = self.0.accept()?;
            let writer = stream.try_clone()?;
            Ok(Conn {
                reader: Box::new(stream),
                writer: Box::new(writer),
            })
        }
    }

    pub fn remove(address: &Path) {
        let _ = std::fs::remove_file(address);
    }
}

#[cfg(windows)]
mod win;

#[cfg(windows)]
mod imp {
    use super::{Address, Conn, ConnectError, win};
    use interprocess::os::windows::ToWtf16;
    use interprocess::os::windows::named_pipe::{PipeListener, PipeListenerOptions, pipe_mode};
    use interprocess::os::windows::security_descriptor::SecurityDescriptor;
    use std::ffi::OsStr;
    use std::os::windows::fs::OpenOptionsExt;
    use std::path::{Path, PathBuf};
    use std::time::{Duration, Instant};

    const PREFIX: &str = r"\\.\pipe\";

    pub fn from_arg(value: &str) -> Address {
        let lower = value.to_ascii_lowercase();
        if lower.starts_with(PREFIX) || lower.starts_with(r"\\?\pipe\") {
            PathBuf::from(value)
        } else {
            PathBuf::from(format!("{PREFIX}{value}"))
        }
    }

    /// `\\.\pipe\neoscad-<SID>`: the user's security identifier names
    /// the pipe, as the user id names the Unix default's directory. A user
    /// name would do as well if it were always there and unique; the SID
    /// is both, across domains too.
    pub fn default_address() -> Address {
        let user = win::current_user_sid()
            .unwrap_or_else(|_| std::env::var("USERNAME").unwrap_or_else(|_| "user".to_string()));
        PathBuf::from(format!("{PREFIX}neoscad-{user}"))
    }

    pub fn may_be_listening(_address: &Path) -> bool {
        true
    }

    /// Windows' `ERROR_PIPE_BUSY`: every instance is taken. The listener
    /// makes the next instance as soon as it accepts, so this lasts only
    /// that long.
    const ERROR_PIPE_BUSY: i32 = 231;

    pub fn connect(address: &Path) -> Result<Conn, ConnectError> {
        let deadline = Instant::now() + Duration::from_secs(2);
        let pipe = loop {
            // Identification only: a pipe server can act as its client
            // (`ImpersonateNamedPipeClient`), and without this flag at the
            // client's full rights. Someone else's pipe at our name must
            // not get that before the owner check below refuses it.
            let opened = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .security_qos_flags(
                    windows_sys::Win32::Storage::FileSystem::SECURITY_IDENTIFICATION,
                )
                .open(address);
            match opened {
                Ok(p) => break p,
                Err(e)
                    if e.raw_os_error() == Some(ERROR_PIPE_BUSY) && Instant::now() < deadline =>
                {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(_) => return Err(ConnectError::NoServer),
            }
        };
        match win::owned_by_current_user(&pipe) {
            Ok(true) => {}
            Ok(false) => {
                return Err(ConnectError::Untrusted(format!(
                    "{} belongs to another user",
                    address.display()
                )));
            }
            Err(e) => {
                return Err(ConnectError::Untrusted(format!(
                    "cannot read the owner of {}: {e}",
                    address.display()
                )));
            }
        }
        // One synchronous handle for both directions: a client writes its
        // request and then reads, never both at once.
        let writer = pipe.try_clone().map_err(|_| ConnectError::NoServer)?;
        Ok(Conn {
            reader: Box::new(pipe),
            writer: Box::new(writer),
        })
    }

    pub struct Listener(PipeListener<pipe_mode::Bytes, pipe_mode::Bytes>);

    impl std::fmt::Debug for Listener {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Listener")
        }
    }

    pub fn bind(address: &Path) -> Result<Listener, String> {
        let sid = win::current_user_sid()
            .map_err(|e| format!("cannot read this user's security identifier: {e}"))?;
        // Owner: this user (not the token's default owner, which for an
        // elevated administrator is the Administrators group, and which
        // clients compare against their own user). DACL, protected from
        // inheritance: full access for this user and no one else. The
        // default DACL would also let Everyone and Anonymous read.
        let sddl = format!("O:{sid}D:P(A;;GA;;;{sid})");
        let wide = OsStr::new(&sddl)
            .to_wtf_16()
            .map_err(|e| format!("security descriptor {sddl}: {e}"))?;
        let sd = SecurityDescriptor::deserialize(&wide)
            .map_err(|e| format!("security descriptor {sddl}: {e}"))?;
        // interprocess creates the first instance with
        // FILE_FLAG_FIRST_PIPE_INSTANCE, so a pipe of that name that exists
        // already (a running server, or another user's pipe) is an error
        // rather than a second server beside it; remote clients are
        // rejected (PIPE_REJECT_REMOTE_CLIENTS) by default.
        let listener = PipeListenerOptions::new()
            .path(address)
            .security_descriptor(Some(sd))
            .create_duplex::<pipe_mode::Bytes>()
            .map_err(|e| {
                if e.kind() == std::io::ErrorKind::PermissionDenied {
                    format!(
                        "a server is already listening at {} (or another program owns that pipe name)",
                        address.display()
                    )
                } else {
                    format!("cannot listen at {}: {e}", address.display())
                }
            })?;
        Ok(Listener(listener))
    }

    impl Listener {
        pub fn accept(&self) -> std::io::Result<Conn> {
            let (reader, writer) = self.0.accept()?.split();
            Ok(Conn {
                reader: Box::new(reader),
                writer: Box::new(writer),
            })
        }
    }

    pub fn remove(_address: &Path) {}
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use super::{Address, Conn, ConnectError};
    use std::path::{Path, PathBuf};

    pub fn from_arg(value: &str) -> Address {
        PathBuf::from(value)
    }
    pub fn default_address() -> Address {
        std::env::temp_dir().join("neoscad-serve.sock")
    }
    pub fn may_be_listening(_address: &Path) -> bool {
        false
    }
    pub fn connect(_address: &Path) -> Result<Conn, ConnectError> {
        Err(ConnectError::NoServer)
    }
    #[derive(Debug)]
    pub struct Listener;
    pub fn bind(_address: &Path) -> Result<Listener, String> {
        Err("no socket transport on this platform; use `neoscad serve` on stdio".into())
    }
    impl Listener {
        pub fn accept(&self) -> std::io::Result<Conn> {
            Err(std::io::ErrorKind::Unsupported.into())
        }
    }
    pub fn remove(_address: &Path) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
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
            let d = imp::default_address();
            let name = d.to_str().unwrap();
            assert!(name.starts_with(r"\\.\pipe\neoscad-S-1-"), "{name}");
        } else {
            assert_eq!(from_arg("dir/s.sock"), PathBuf::from("dir/s.sock"));
            assert!(imp::default_address().ends_with("serve.sock"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn only_a_private_directorys_socket_is_trusted() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("nstrust-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let sock = d.join("s.sock");
        let _l = std::os::unix::net::UnixListener::bind(&sock).unwrap();
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
        assert!(imp::trusted(&sock).is_ok());
        // Anyone could have put it there.
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            imp::trusted(&sock)
                .unwrap_err()
                .contains("writable by other users")
        );
        assert!(matches!(connect(&sock), Err(ConnectError::Untrusted(_))));
        // Sticky (like /tmp): no one else can replace a socket that is ours.
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o1777)).unwrap();
        assert!(imp::trusted(&sock).is_ok());
        // Not a socket at all.
        std::fs::write(d.join("f"), "x").unwrap();
        assert!(
            imp::trusted(&d.join("f"))
                .unwrap_err()
                .contains("not a socket")
        );
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(windows)]
    #[test]
    fn the_pipe_is_owned_by_this_user() {
        let address = scratch_address("owner");
        let _listener = Listener::bind(&address).unwrap();
        let pipe = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&address)
            .unwrap();
        assert!(win::owned_by_current_user(&pipe).unwrap());
    }
}
