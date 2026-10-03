//! Per-user local connections: a Unix socket on Unix, a named pipe on
//! Windows. `neoscad serve --socket` listens on one
//! (docs/serve-protocol.md, "Platforms"), and so does a desktop app with
//! agent access turned on (docs/agent-bridge.md, "Desktop apps"). Both
//! carry a byte stream, so their users see only an [`Address`], a
//! [`Listener`] and a [`Conn`].
//!
//! Moved here from the command line so the apps' listener shares the
//! checks rather than repeating them. Each transport is restricted to the
//! user who started the listener, since a client sends what the user is
//! working on (the server's paths and environment; an app's unsaved text
//! and pictures of its view) and acts on what comes back:
//! - Unix: the socket is 0600 in a directory that must be the user's
//!   (0700 for the default ones), and a client trusts a socket only when it
//!   and its directory pass [`trusted`]. A listener may also refuse a peer
//!   of another user outright ([`Listener::accept_from_user`]).
//! - Windows: the pipe's security descriptor names the user as its owner
//!   and grants access to no one else, and a client proceeds only when the
//!   pipe it opened is owned by its own user. Pipe names are one global
//!   namespace, so another user can create the pipe first; without the
//!   owner check the client would hand that user its paths and act on
//!   their forged answers.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};

/// A listener's address: a socket path on Unix, a pipe name
/// (`\\.\pipe\...`) on Windows. A `PathBuf` on both, so messages show it
/// the same way.
pub type Address = PathBuf;

/// One connection's two directions. Separate objects, because a
/// connection's requests run on their own threads and write their answers
/// while its reader waits for the next message.
pub struct Conn {
    pub reader: Box<dyn Read + Send>,
    pub writer: Box<dyn Write + Send>,
    /// Ends the connection from any thread, waking a reader blocked on it.
    pub closer: Closer,
}

impl std::fmt::Debug for Conn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Conn")
    }
}

/// Ends a connection from another thread than its reader's. On Unix it
/// shuts the socket down, which ends a blocked read at once. A Windows
/// pipe's halves cannot be shut down apart from their handles, so there it
/// does nothing, and a listener asks the peer to close instead (the agent
/// link's `bye`).
#[derive(Default)]
pub struct Closer(Option<Box<dyn Fn() + Send + Sync>>);

impl Closer {
    pub fn close(&self) {
        if let Some(f) = &self.0 {
            f();
        }
    }

    /// Whether [`Closer::close`] can end a blocked read on this platform.
    pub fn works(&self) -> bool {
        self.0.is_some()
    }
}

impl std::fmt::Debug for Closer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Closer")
    }
}

/// Why a client could not reach a listener.
#[derive(Debug)]
pub enum ConnectError {
    /// Nothing listens at the address.
    NoServer,
    /// Something listens, but not this user's listener.
    Untrusted(String),
}

/// The address `--socket VALUE` names. On Windows a bare name becomes
/// `\\.\pipe\NAME`, so `--socket neoscad-test` works as it reads; a value
/// with the prefix is used as it is.
pub fn from_arg(value: &str) -> Address {
    imp::from_arg(value)
}

/// This user's private directory for sockets, on Unix:
/// `$XDG_RUNTIME_DIR/neoscad`, else `<temp dir>/neoscad-<uid>` (on macOS
/// the temp dir is already per user). On Windows, where pipes have no
/// directory, the temp dir (unused).
pub fn runtime_dir() -> PathBuf {
    imp::runtime_dir()
}

/// Whether a listener may be at `address`, as cheaply as the platform
/// allows: the command line asks before every export, and a run with no
/// server should pay one `stat`. On Windows there is no way to ask without
/// connecting (opening a pipe takes an instance), so this is always true
/// there and [`connect`] answers.
pub fn may_be_listening(address: &Path) -> bool {
    imp::may_be_listening(address)
}

/// Connect to the listener at `address`, checking first that it is this
/// user's (nothing is sent before the check).
pub fn connect(address: &Path) -> Result<Conn, ConnectError> {
    imp::connect(address)
}

/// A listener.
#[derive(Debug)]
pub struct Listener(imp::Listener);

impl Listener {
    /// Listen at `address`, made private to this user. Fails when a
    /// listener is already there, and (on Unix) rather than replace a file
    /// that is not a socket. `private_dir`: the directory is a default
    /// one, which must be this user's and is made 0700 (a socket placed
    /// elsewhere on purpose is only made user-only itself).
    pub fn bind(address: &Path, private_dir: bool) -> Result<Listener, String> {
        imp::bind(address, private_dir).map(Listener)
    }

    /// Wait for the next client.
    pub fn accept(&self) -> std::io::Result<Conn> {
        self.0.accept()
    }

    /// Wait for the next client, refusing (closing at once) one that runs
    /// as another user: on Unix the peer's credentials must name this
    /// user (`SO_PEERCRED`, `getpeereid`), on top of the socket's 0600 and
    /// its directory's 0700, which already keep other users out unless
    /// they are root. On Windows the pipe's security descriptor admits this
    /// user alone, which is the check. `Ok(None)` is a refused peer.
    pub fn accept_from_user(&self) -> std::io::Result<Option<Conn>> {
        self.0.accept_from_user()
    }
}

/// Remove what the listener left behind (the socket file on Unix; a pipe
/// goes when its handles close).
pub fn remove(address: &Path) {
    imp::remove(address);
}

/// This user's id, which names the per-user directories.
#[cfg(unix)]
pub fn uid() -> u32 {
    nix::unistd::getuid().as_raw()
}

/// This process's user, as `S-1-5-21-...`, which names the per-user pipes.
#[cfg(windows)]
pub fn current_user_sid() -> std::io::Result<String> {
    win::current_user_sid()
}

/// Whether `socket` is this user's listener's, before anything is sent to
/// it (see [`imp::trusted`]).
#[cfg(unix)]
pub fn trusted(socket: &Path) -> Result<(), String> {
    imp::trusted(socket)
}

/// Whether `address` is a socket of this user's that nothing listens at
/// any more (a listener that did not exit cleanly left it). Never true for
/// a pipe, which goes with its listener.
pub fn is_stale(address: &Path) -> bool {
    #[cfg(unix)]
    {
        imp::trusted(address).is_ok()
            && std::os::unix::net::UnixStream::connect(address)
                .is_err_and(|e| e.kind() == std::io::ErrorKind::ConnectionRefused)
    }
    #[cfg(not(unix))]
    {
        let _ = address;
        false
    }
}

#[cfg(unix)]
mod imp {
    use super::{Address, Closer, Conn, ConnectError};
    use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::{Path, PathBuf};

    pub fn from_arg(value: &str) -> Address {
        PathBuf::from(value)
    }

    pub fn runtime_dir() -> PathBuf {
        if let Some(d) = std::env::var_os("XDG_RUNTIME_DIR").filter(|p| !p.is_empty()) {
            return PathBuf::from(d).join("neoscad");
        }
        std::env::temp_dir().join(format!("neoscad-{}", super::uid()))
    }

    pub fn may_be_listening(address: &Path) -> bool {
        address.exists() && trusted(address).is_ok()
    }

    /// The writer's half, shared with the closer only weakly: the closer
    /// must not keep the socket open. Holding a descriptor of its own, it
    /// did: a peer waiting for the end of the stream after both halves
    /// were dropped waited for ever (`neoscad serve`'s transport test).
    struct Shared(std::sync::Arc<UnixStream>);

    impl std::io::Write for Shared {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            (&*self.0).write(buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            (&*self.0).flush()
        }
    }

    fn conn(stream: UnixStream) -> std::io::Result<Conn> {
        let writer = std::sync::Arc::new(stream.try_clone()?);
        let weak = std::sync::Arc::downgrade(&writer);
        Ok(Conn {
            reader: Box::new(stream),
            writer: Box::new(Shared(writer)),
            // Shutting the socket down ends every descriptor's reads, the
            // reader's too; once the writer is gone, nothing is left to do.
            closer: Closer(Some(Box::new(move || {
                if let Some(s) = weak.upgrade() {
                    let _ = s.shutdown(std::net::Shutdown::Both);
                }
            }))),
        })
    }

    pub fn connect(address: &Path) -> Result<Conn, ConnectError> {
        if !address.exists() {
            return Err(ConnectError::NoServer);
        }
        trusted(address).map_err(ConnectError::Untrusted)?;
        let stream = UnixStream::connect(address).map_err(|_| ConnectError::NoServer)?;
        conn(stream).map_err(|_| ConnectError::NoServer)
    }

    /// Whether `socket` is this user's listener's, before anything is sent
    /// to it. The socket must be a socket owned by this user, in a
    /// directory that is this user's and not writable by group or others
    /// (or a sticky one, like `/tmp`, where no one else can replace a
    /// socket that is ours).
    ///
    /// Without this, on Linux without `XDG_RUNTIME_DIR` another local user
    /// could create `/tmp/neoscad-<uid>/` first, listen there, and receive
    /// a victim's paths and environment and forge their output (the
    /// agent-surface audit's finding 8). The listener checks the default
    /// directory's owner when it starts; the client checks too, and
    /// refuses otherwise.
    pub fn trusted(socket: &Path) -> Result<(), String> {
        let me = super::uid();
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

    pub fn bind(path: &Path, private_dir: bool) -> Result<Listener, String> {
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
        // The socket is how anyone talks to this user's listener. A
        // default one lives in a directory that must be this user's alone;
        // a socket placed elsewhere on purpose is only made user-only
        // itself.
        if private_dir {
            let meta = std::fs::metadata(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            if meta.uid() != super::uid() {
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
            // Left by a listener that did not exit cleanly.
            std::fs::remove_file(path).map_err(|e| format!("{}: {e}", path.display()))?;
        }
        let listener = UnixListener::bind(path)
            .map_err(|e| format!("cannot listen at {}: {e}", path.display()))?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|e| format!("{}: {e}", path.display()))?;
        Ok(Listener(listener))
    }

    /// The user id of the process at the other end of `stream`.
    #[cfg(any(target_os = "linux", target_os = "android"))]
    fn peer_uid(stream: &UnixStream) -> std::io::Result<u32> {
        use nix::sys::socket::{getsockopt, sockopt::PeerCredentials};
        let cred = getsockopt(stream, PeerCredentials).map_err(std::io::Error::from)?;
        Ok(cred.uid())
    }

    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    fn peer_uid(stream: &UnixStream) -> std::io::Result<u32> {
        let (uid, _) = nix::unistd::getpeereid(stream).map_err(std::io::Error::from)?;
        Ok(uid.as_raw())
    }

    impl Listener {
        pub fn accept(&self) -> std::io::Result<Conn> {
            let (stream, _) = self.0.accept()?;
            conn(stream)
        }

        pub fn accept_from_user(&self) -> std::io::Result<Option<Conn>> {
            let (stream, _) = self.0.accept()?;
            // A peer whose credentials cannot be read is refused like one
            // of another user: the check is the point.
            if peer_uid(&stream).ok() != Some(super::uid()) {
                let _ = stream.shutdown(std::net::Shutdown::Both);
                return Ok(None);
            }
            conn(stream).map(Some)
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
    use super::{Address, Closer, Conn, ConnectError, win};
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

    pub fn runtime_dir() -> PathBuf {
        std::env::temp_dir()
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
            closer: Closer::default(),
        })
    }

    pub struct Listener(PipeListener<pipe_mode::Bytes, pipe_mode::Bytes>);

    impl std::fmt::Debug for Listener {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("Listener")
        }
    }

    pub fn bind(address: &Path, _private_dir: bool) -> Result<Listener, String> {
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
                closer: Closer::default(),
            })
        }

        pub fn accept_from_user(&self) -> std::io::Result<Option<Conn>> {
            // The security descriptor admits this user alone (`bind`).
            self.accept().map(Some)
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
    pub fn runtime_dir() -> PathBuf {
        std::env::temp_dir()
    }
    pub fn may_be_listening(_address: &Path) -> bool {
        false
    }
    pub fn connect(_address: &Path) -> Result<Conn, ConnectError> {
        Err(ConnectError::NoServer)
    }
    #[derive(Debug)]
    pub struct Listener;
    pub fn bind(_address: &Path, _private_dir: bool) -> Result<Listener, String> {
        Err("no socket transport on this platform".into())
    }
    impl Listener {
        pub fn accept(&self) -> std::io::Result<Conn> {
            Err(std::io::ErrorKind::Unsupported.into())
        }
        pub fn accept_from_user(&self) -> std::io::Result<Option<Conn>> {
            Err(std::io::ErrorKind::Unsupported.into())
        }
    }
    pub fn remove(_address: &Path) {}
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// An address no other test (or test run) uses. On Unix, in a fresh
    /// private directory, as a listener's own default is.
    fn scratch_address(name: &str) -> Address {
        static N: AtomicUsize = AtomicUsize::new(0);
        let tag = format!(
            "nal-{}-{}-{name}",
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
    fn a_line_goes_both_ways_and_the_closer_ends_a_blocked_read() {
        let address = scratch_address("echo");
        let listener = Listener::bind(&address, true).unwrap();
        let server = std::thread::spawn(move || {
            let conn = listener.accept_from_user().unwrap().expect("our own user");
            let mut r = BufReader::new(conn.reader);
            let mut w = conn.writer;
            let mut line = String::new();
            r.read_line(&mut line).unwrap();
            w.write_all(line.as_bytes()).unwrap();
            w.flush().unwrap();
            // Next, a read that only the closer ends.
            let closer = conn.closer;
            let blocked = std::thread::spawn(move || {
                let mut rest = String::new();
                r.read_line(&mut rest).map(|n| n == 0)
            });
            std::thread::sleep(std::time::Duration::from_millis(50));
            if closer.works() {
                closer.close();
                assert!(blocked.join().unwrap().unwrap_or(true));
            }
        });
        assert!(may_be_listening(&address));
        let conn = connect(&address).unwrap();
        let mut w = conn.writer;
        w.write_all(b"hello\n").unwrap();
        w.flush().unwrap();
        let mut r = BufReader::new(conn.reader);
        let mut line = String::new();
        r.read_line(&mut line).unwrap();
        assert_eq!(line, "hello\n");
        server.join().unwrap();
        drop(w);
        cleanup(&address);
    }

    #[test]
    fn a_second_listener_at_the_same_address_is_refused() {
        let address = scratch_address("twice");
        let _first = Listener::bind(&address, true).unwrap();
        let e = Listener::bind(&address, true).unwrap_err();
        assert!(e.contains("already listening"), "{e}");
        cleanup(&address);
    }

    #[test]
    fn nothing_listening_is_no_server() {
        let address = scratch_address("absent");
        assert!(matches!(connect(&address), Err(ConnectError::NoServer)));
        cleanup(&address);
    }

    #[cfg(unix)]
    #[test]
    fn only_a_private_directorys_socket_is_trusted() {
        use std::os::unix::fs::PermissionsExt;
        let d = std::env::temp_dir().join(format!("naltrust-{}", std::process::id()));
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
        assert!(matches!(connect(&sock), Err(ConnectError::Untrusted(_))));
        // Sticky (like /tmp): no one else can replace a socket that is ours.
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o1777)).unwrap();
        assert!(trusted(&sock).is_ok());
        // Not a socket at all.
        std::fs::write(d.join("f"), "x").unwrap();
        assert!(trusted(&d.join("f")).unwrap_err().contains("not a socket"));
        std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).unwrap();
        let _ = std::fs::remove_dir_all(&d);
    }

    #[cfg(windows)]
    #[test]
    fn the_pipe_is_owned_by_this_user() {
        let address = scratch_address("owner");
        let _listener = Listener::bind(&address, true).unwrap();
        let pipe = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&address)
            .unwrap();
        assert!(win::owned_by_current_user(&pipe).unwrap());
    }
}
