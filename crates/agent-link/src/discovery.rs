//! Where a running app listens, and where `neoscad mcp` looks for it.
//!
//! Each app process with agents allowed listens at an address of its own
//! (so several processes coexist: the Windows app is one per window), in a
//! place both sides compute the same way from the user alone:
//!
//! - **macOS:** `~/Library/Application Support/NeoSCAD/run/app-<random>.sock`.
//!   Not the temp directory: Rust's `temp_dir()` falls back to `/tmp` when
//!   `TMPDIR` is unset, and an MCP client may start `neoscad mcp` with a
//!   trimmed environment, so the app and the command line could look in
//!   different places. A socket path is limited to 104 bytes; with a home
//!   directory so long that the path would not fit, the app listens in
//!   `/tmp/neoscad-<uid>/` instead, which the command line also searches.
//! - **Linux:** `$XDG_RUNTIME_DIR/neoscad/`; inside the Flatpak,
//!   `$XDG_RUNTIME_DIR/app/org.neoscad.NeoSCAD/`, the per-app directory
//!   Flatpak makes writable in the sandbox and which a host `neoscad mcp`
//!   searches too (see docs/agent-bridge.md, "Desktop apps", for what is
//!   verified about it). Without `XDG_RUNTIME_DIR`, `/tmp/neoscad-<uid>/`.
//!   Never an abstract socket: those have no permissions at all, and the
//!   Flatpak's `--share=network` reaches every one on the host.
//! - **Windows:** a pipe `\\.\pipe\neoscad-<SID>-app-<pid>-<random>`; the
//!   command line lists the pipe namespace for its user's prefix.
//!
//! `NEOSCAD_AGENT_DIR` names one directory instead, for tests and for
//! running two setups side by side. On Windows, where pipes share one
//! namespace, a hash of the directory's whole path tags the pipe names
//! ([`dir_tag`]), so two directories never see each other's apps.
//!
//! Finding a socket proves nothing: the command line checks its owner and
//! directory before it sends anything ([`crate::transport::connect`]).

use std::path::{Path, PathBuf};

use crate::transport::Address;

/// Environment variable naming the one directory apps listen in and the
/// command line searches (a test's scratch directory).
pub const DIR_ENV: &str = "NEOSCAD_AGENT_DIR";

/// The Flatpak's application id, whose runtime directory the app listens
/// in when sandboxed and the command line searches always.
pub const FLATPAK_ID: &str = "org.neoscad.NeoSCAD";

/// What a listening app's socket (or pipe) name starts with.
const PREFIX: &str = "app-";

/// Where apps listen and the command line looks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendezvous {
    /// Unix: the directory this process listens in. Windows: the pipe
    /// names' prefix (`neoscad-app-<SID>-`).
    listen: PathBuf,
    /// Unix: the directories searched, in order (`listen` among them).
    search: Vec<PathBuf>,
}

impl Rendezvous {
    /// This user's places, or `NEOSCAD_AGENT_DIR`'s one.
    pub fn from_env() -> Rendezvous {
        match std::env::var_os(DIR_ENV).filter(|d| !d.is_empty()) {
            Some(d) => Rendezvous::at(Path::new(&d)),
            None => imp::default(),
        }
    }

    /// One place only: a directory on Unix; on Windows a hash of its path
    /// becomes a tag in the pipe names ([`dir_tag`]).
    pub fn at(dir: &Path) -> Rendezvous {
        imp::at(dir)
    }

    /// The directory an app listens in (Unix), or the pipe names' prefix
    /// (Windows).
    pub fn listen_dir(&self) -> &Path {
        &self.listen
    }

    /// The directories searched (Unix), or the prefix (Windows).
    pub fn search_dirs(&self) -> &[PathBuf] {
        &self.search
    }

    /// A fresh address for a listening app: random, so two app processes
    /// never collide (two Flatpak sandboxes can both see their app as
    /// process 2).
    pub fn new_address(&self) -> Address {
        let mut bytes = [0u8; 8];
        // Without the OS's generator (it does not fail in practice), the
        // process id and the time still separate two apps.
        if getrandom::fill(&mut bytes).is_err() {
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            bytes = (t ^ u64::from(std::process::id()) << 32).to_le_bytes();
        }
        let tag: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        imp::address(self, &tag)
    }

    /// The addresses apps listen at now, sorted. Cheap: a directory
    /// listing per place (the pipe namespace's on Windows). Each still has
    /// to pass the owner check when connected to.
    pub fn scan(&self) -> Vec<Address> {
        let mut out = imp::scan(self);
        out.sort();
        out.dedup();
        out
    }
}

/// The pipe-name tag for `NEOSCAD_AGENT_DIR` on Windows: `rv` and a hash
/// of the directory's absolute path, folded the way Windows compares
/// paths (case, `/` against `\`, a trailing separator).
///
/// The whole path, not its last component: tagging by the folder's name
/// alone made `C:\a\rv` and `C:\b\rv` one rendezvous, so parallel tests
/// (each with its own `...\rv`) attached to each other's apps, and two
/// setups side by side would have crossed the same way. The `rv` start
/// keeps a tagged name from reading as an untagged one: after the user's
/// prefix an untagged app's name goes straight on with `app-`.
///
/// FNV-1a rather than std's hasher, whose output may change between
/// Rust releases: the app and `neoscad mcp` can be different builds and
/// must compute the same tag.
#[cfg_attr(not(windows), allow(dead_code))]
fn dir_tag(dir: &Path) -> String {
    let abs = std::path::absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let folded = abs.to_string_lossy().replace('/', "\\").to_lowercase();
    let folded = folded.trim_end_matches('\\');
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in folded.bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("rv{h:016x}")
}

/// The pipe names' prefix for `user` (a SID), tagged for one
/// `NEOSCAD_AGENT_DIR` or not.
#[cfg_attr(not(windows), allow(dead_code))]
fn pipe_prefix(user: &str, dir: Option<&Path>) -> String {
    match dir {
        Some(d) => format!("neoscad-{user}-{}-", dir_tag(d)),
        None => format!("neoscad-{user}-"),
    }
}

/// `name` is a listening app's socket or pipe name.
fn is_app(name: &str, prefix: &str) -> bool {
    name.strip_prefix(prefix)
        .and_then(|rest| rest.strip_prefix(PREFIX))
        .is_some_and(|rest| !rest.is_empty())
}

#[cfg(unix)]
mod imp {
    use super::{PREFIX, Rendezvous, is_app};
    use crate::transport::Address;
    use std::os::unix::fs::FileTypeExt;
    use std::path::{Path, PathBuf};

    /// `sun_path`'s size on macOS and the BSDs (Linux allows 108); a path
    /// must leave room for its terminating NUL.
    const MAX_SOCKET_PATH: usize = 103;
    /// The longest socket name: `app-` and 16 hex digits and `.sock`.
    const NAME_LEN: usize = 25;

    fn fallback() -> PathBuf {
        PathBuf::from(format!("/tmp/neoscad-{}", crate::transport::uid()))
    }

    fn fits(dir: &Path) -> bool {
        dir.as_os_str().len() + 1 + NAME_LEN <= MAX_SOCKET_PATH
    }

    pub fn at(dir: &Path) -> Rendezvous {
        Rendezvous {
            listen: dir.to_path_buf(),
            search: vec![dir.to_path_buf()],
        }
    }

    #[cfg(target_vendor = "apple")]
    pub fn default() -> Rendezvous {
        let home = std::env::var_os("HOME").filter(|h| !h.is_empty());
        let support = home.map(|h| {
            PathBuf::from(h)
                .join("Library")
                .join("Application Support")
                .join("NeoSCAD")
                .join("run")
        });
        let mut search: Vec<PathBuf> = support.iter().cloned().collect();
        search.push(fallback());
        let listen = support.filter(|d| fits(d)).unwrap_or_else(fallback);
        Rendezvous { listen, search }
    }

    #[cfg(not(target_vendor = "apple"))]
    pub fn default() -> Rendezvous {
        let runtime = std::env::var_os("XDG_RUNTIME_DIR")
            .filter(|d| !d.is_empty())
            .map(PathBuf::from);
        let host = runtime.as_ref().map(|r| r.join("neoscad"));
        let flatpak = runtime
            .as_ref()
            .map(|r| r.join("app").join(super::FLATPAK_ID));
        let mut search: Vec<PathBuf> = host.iter().chain(&flatpak).cloned().collect();
        search.push(fallback());
        // Inside the Flatpak the app can write only its own runtime
        // directory; the variable is Flatpak's, and `/.flatpak-info` is
        // there whatever the environment says.
        let sandboxed =
            std::env::var_os("FLATPAK_ID").is_some() || Path::new("/.flatpak-info").exists();
        let listen = if sandboxed { flatpak } else { host }
            .filter(|d| fits(d))
            .unwrap_or_else(fallback);
        Rendezvous { listen, search }
    }

    pub fn address(r: &Rendezvous, tag: &str) -> Address {
        r.listen.join(format!("{PREFIX}{tag}.sock"))
    }

    pub fn scan(r: &Rendezvous) -> Vec<Address> {
        let mut out = Vec::new();
        for dir in &r.search {
            let Ok(entries) = std::fs::read_dir(dir) else {
                continue;
            };
            for e in entries.flatten() {
                let name = e.file_name();
                let Some(name) = name.to_str() else { continue };
                if !(is_app(name, "") && name.ends_with(".sock")) {
                    continue;
                }
                // `file_type` does not follow a link: a link named like a
                // socket is not one of ours.
                if e.file_type().is_ok_and(|t| t.is_socket()) {
                    out.push(e.path());
                }
            }
        }
        out
    }
}

#[cfg(windows)]
mod imp {
    use super::{PREFIX, Rendezvous, is_app, pipe_prefix};
    use crate::transport::Address;
    use std::path::{Path, PathBuf};

    const PIPES: &str = r"\\.\pipe\";

    fn user() -> String {
        crate::transport::current_user_sid()
            .unwrap_or_else(|_| std::env::var("USERNAME").unwrap_or_else(|_| "user".to_string()))
    }

    fn with_prefix(prefix: String) -> Rendezvous {
        Rendezvous {
            listen: PathBuf::from(&prefix),
            search: vec![PathBuf::from(prefix)],
        }
    }

    pub fn default() -> Rendezvous {
        with_prefix(pipe_prefix(&user(), None))
    }

    pub fn at(dir: &Path) -> Rendezvous {
        with_prefix(pipe_prefix(&user(), Some(dir)))
    }

    pub fn address(r: &Rendezvous, tag: &str) -> Address {
        PathBuf::from(format!(
            "{PIPES}{}{PREFIX}{}-{tag}",
            r.listen.display(),
            std::process::id()
        ))
    }

    pub fn scan(r: &Rendezvous) -> Vec<Address> {
        let prefix = r.listen.to_string_lossy().into_owned();
        let Ok(entries) = std::fs::read_dir(PIPES) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                is_app(&name, &prefix).then(|| PathBuf::from(format!("{PIPES}{name}")))
            })
            .collect()
    }
}

#[cfg(not(any(unix, windows)))]
mod imp {
    use super::Rendezvous;
    use crate::transport::Address;
    use std::path::Path;

    pub fn default() -> Rendezvous {
        at(&std::env::temp_dir())
    }
    pub fn at(dir: &Path) -> Rendezvous {
        Rendezvous {
            listen: dir.to_path_buf(),
            search: vec![dir.to_path_buf()],
        }
    }
    pub fn address(r: &Rendezvous, tag: &str) -> Address {
        r.listen.join(tag)
    }
    pub fn scan(_r: &Rendezvous) -> Vec<Address> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_names_are_told_apart() {
        assert!(is_app("app-0123.sock", ""));
        assert!(!is_app("app-", ""));
        assert!(!is_app("serve.sock", ""));
        assert!(is_app("neoscad-S-1-5-app-12-ab", "neoscad-S-1-5-"));
        assert!(!is_app("neoscad-S-1-6-app-12-ab", "neoscad-S-1-5-"));
    }

    /// The Windows rendezvous filter, checked on every platform: tagging
    /// and the name filter are plain string work.
    #[test]
    fn agent_dirs_isolate_pipe_names() {
        let sid = "S-1-5-21-1";
        let a = Path::new(r"C:\Temp\nsapp1one\rv");
        let b = Path::new(r"C:\Temp\nsapp1two\rv");
        let (pa, pb) = (pipe_prefix(sid, Some(a)), pipe_prefix(sid, Some(b)));
        // Same folder name, different folders: different rendezvous (the
        // Windows CI failure was these two being one).
        assert_ne!(pa, pb);
        let app_b = format!("{pb}app-42-0123abcd");
        assert!(is_app(&app_b, &pb));
        assert!(!is_app(&app_b, &pa));
        // An untagged scan and another user's do not see a tagged app,
        // and a tagged scan does not see an untagged one.
        let plain = pipe_prefix(sid, None);
        assert!(!is_app(&app_b, &plain));
        assert!(!is_app(&app_b, &pipe_prefix("S-1-5-21-2", Some(b))));
        assert!(!is_app(&format!("{plain}app-42-0123abcd"), &pb));
        // A folder named like the untagged marker is still kept apart.
        let named_app = pipe_prefix(sid, Some(Path::new(r"C:\app")));
        assert!(!is_app(&format!("{named_app}app-42-0123abcd"), &plain));
        // The same folder spelled another way is the same rendezvous, as
        // on Windows it is the same folder; and the tag is stable.
        assert_eq!(
            dir_tag(Path::new(r"C:\Temp\NSAPP1ONE\rv\")),
            dir_tag(Path::new("c:/temp/nsapp1one/rv"))
        );
        assert_eq!(dir_tag(a), dir_tag(a));
        assert_eq!(dir_tag(a).len(), 18);
    }

    #[test]
    fn addresses_are_fresh_and_found() {
        let r = Rendezvous::at(&std::env::temp_dir().join("nal-disc"));
        let (a, b) = (r.new_address(), r.new_address());
        assert_ne!(a, b);
        if cfg!(unix) {
            assert!(a.starts_with(r.listen_dir()));
            let name = a.file_name().unwrap().to_str().unwrap();
            assert!(
                name.starts_with("app-") && name.ends_with(".sock"),
                "{name}"
            );
            assert_eq!(name.len(), 25);
        }
    }

    #[cfg(unix)]
    #[test]
    fn the_defaults_fit_a_socket_path_and_are_searched() {
        let r = imp::default();
        let a = r.new_address();
        assert!(a.as_os_str().len() <= 103, "{}", a.display());
        assert!(r.search_dirs().iter().any(|d| a.starts_with(d)));
        if cfg!(target_vendor = "apple") {
            assert!(
                r.search_dirs()[0].ends_with("Library/Application Support/NeoSCAD/run"),
                "{:?}",
                r.search_dirs()
            );
        } else {
            assert!(
                r.search_dirs()
                    .iter()
                    .any(|d| d.ends_with("app/org.neoscad.NeoSCAD"))
                    || std::env::var_os("XDG_RUNTIME_DIR").is_none()
            );
        }
    }
}
