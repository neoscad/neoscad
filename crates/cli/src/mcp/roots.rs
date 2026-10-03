//! What `neoscad mcp` may touch: the allowed roots (the working directory
//! and each `--root`) for reading and writing, plus the library path and
//! font directories for reading only.
//!
//! Two layers enforce it. Tool arguments that name files (`path`,
//! `base_dir`, `export`, `output`, `diff_against`) are checked before a
//! request starts, so an agent gets a clear refusal. And the session's
//! file system is a [`RootedFs`], which refuses every read outside the
//! allowed directories: a model's `include`, `use`, `import()` or
//! `surface()` names files too, and without this layer inline source
//! could read any file on the machine (`import("/etc/passwd")` fails to
//! parse, but its error text may quote the file).
//!
//! A connected NeoSCAD app adds one readable directory per open document:
//! the document's own, so the model tools run it under its real path and
//! its includes resolve (docs/agent-bridge.md, "Desktop apps"). The user
//! chose those directories by opening the documents; writes stay in the
//! roots.

use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, PoisonError, RwLock};

use lang::loader::{FileSystem, Metadata};

/// The directories a client may use.
#[derive(Debug, Clone)]
pub struct Roots {
    /// Read and write, canonical.
    write: Vec<PathBuf>,
    /// Where relative paths resolve: the working directory, root or not.
    base: PathBuf,
    /// Why the working directory is not a root ([`unsafe_cwd`]), for the
    /// refusals.
    cwd_refused: Option<&'static str>,
    /// Read only (libraries, fonts), canonical.
    read: Vec<PathBuf>,
    /// Read only: the directories of a connected app's open documents,
    /// canonical. Shared by every clone (the tools' and the file
    /// system's), and replaced whenever the app's documents change.
    documents: Arc<RwLock<Vec<PathBuf>>>,
}

impl Roots {
    /// `write` are the working directory and `--root`s; `read` adds the
    /// library path and font directories. Directories that do not exist
    /// are dropped (they cannot contain anything).
    pub fn new(write: &[PathBuf], read: &[PathBuf]) -> Roots {
        let canon = |v: &[PathBuf]| -> Vec<PathBuf> {
            // Plain (no `\\?\` on Windows): the paths judged against a root
            // are built by joining onto it or come from a client, and a
            // verbatim root would contain none of them (lang::paths).
            let mut out: Vec<PathBuf> = v
                .iter()
                .filter_map(|p| p.canonicalize().ok().map(lang::paths::plain))
                .collect();
            out.dedup();
            out
        };
        let write = canon(write);
        Roots {
            base: write.first().cloned().unwrap_or_else(|| PathBuf::from("/")),
            write,
            read: canon(read),
            cwd_refused: None,
            documents: Arc::default(),
        }
    }

    /// Relative paths resolve against `cwd`, which is not a root because
    /// of `why` ([`unsafe_cwd`]); the refusals say so.
    pub fn with_unsafe_cwd(mut self, cwd: &Path, why: &'static str) -> Roots {
        self.base = cwd.to_path_buf();
        self.cwd_refused = Some(why);
        self
    }

    /// The directories of the connected apps' open documents, readable
    /// from now on in place of the ones before. Directories that do not
    /// exist are dropped.
    pub fn set_document_dirs(&self, dirs: &[PathBuf]) {
        let mut out: Vec<PathBuf> = dirs
            .iter()
            .filter_map(|p| p.canonicalize().ok().map(lang::paths::plain))
            .collect();
        out.sort();
        out.dedup();
        *self
            .documents
            .write()
            .unwrap_or_else(PoisonError::into_inner) = out;
    }

    /// The directory relative paths resolve against when no `base_dir`
    /// is given: the working directory (the first root, unless it is not
    /// one).
    pub fn home(&self) -> &Path {
        &self.base
    }

    pub fn writable(&self) -> &[PathBuf] {
        &self.write
    }

    /// May `p` (absolute) be read?
    pub fn can_read(&self, p: &Path) -> bool {
        let r = resolve(p);
        if r.as_os_str().is_empty() {
            return false;
        }
        self.write
            .iter()
            .chain(&self.read)
            .any(|d| r.starts_with(d))
            || self
                .documents
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .iter()
                .any(|d| r.starts_with(d))
    }

    /// May `p` (absolute) be written? Judged by where it resolves
    /// ([`resolve`]), dangling links included.
    pub fn can_write(&self, p: &Path) -> bool {
        let r = resolve(p);
        !r.as_os_str().is_empty() && self.write.iter().any(|d| r.starts_with(d))
    }

    /// Why `p` was refused, for the agent: which roots there are and how
    /// to add one.
    pub fn refusal(&self, what: &str, p: &Path) -> String {
        let roots: Vec<String> = self.write.iter().map(|d| d.display().to_string()).collect();
        match self.cwd_refused {
            Some(why) if roots.is_empty() => format!(
                "{what} '{}' is outside the allowed roots: there are none. neoscad mcp started in {} ({why}), which is never a root (some clients, Claude Desktop among them, start servers in `/`). Add `--root DIR` to the server's arguments (\"args\": [\"mcp\", \"--root\", \"/path/to/models\"]); inline `source` works without one",
                p.display(),
                self.base.display(),
            ),
            Some(why) => format!(
                "{what} '{}' is outside the allowed roots ({}; not the working directory {}, {why}); start `neoscad mcp` with `--root DIR` to allow another directory",
                p.display(),
                roots.join(", "),
                self.base.display(),
            ),
            None => format!(
                "{what} '{}' is outside the allowed roots ({}); start `neoscad mcp` with `--root DIR` to allow another directory",
                p.display(),
                roots.join(", ")
            ),
        }
    }
}

/// Why the working directory `cwd` must not be a root, or `None` for a
/// project-like folder that may be. `home` is the user's home directory,
/// `system` the system's folders ([`SystemDirs::here`]).
///
/// An MCP client may start the server anywhere: Claude Desktop starts it
/// with an undefined working directory, "like `/` on macOS"
/// (modelcontextprotocol.io, "Debugging"). Made a root, `/` or the home
/// folder would let an agent write anywhere the user can (a LaunchAgent,
/// a shell profile, `~/.ssh`). So the working directory is a root only
/// when it is none of:
/// - a file system's root (`/`, `C:\`);
/// - the home folder itself, or a folder that contains it (`/Users`);
/// - a folder under home whose first component hides settings, keys or
///   code that runs on its own: a dot folder (`~/.ssh`, `~/.config`),
///   `~/Library` (macOS: LaunchAgents) or `~/AppData` (Windows: Startup);
/// - a system tree (`/usr`, `/etc`, `/System`, `C:\Windows`, ...), or a
///   folder the whole system shares (`/tmp`, `/var`, `/opt`), though not
///   the folders under those.
///
/// The user's temp directory ([`SystemDirs::temp`]) and its subfolders
/// are excepted from the settings-folder rule only. On Windows it is
/// `%LOCALAPPDATA%\Temp`, under `~\AppData`, and refusing it refused every
/// scratch folder a user, a script or a test makes there (Windows CI's
/// MCP tests all failed so); macOS's (`/var/folders/...`) and Linux's
/// (`/tmp`) are outside home and already roots below their shared
/// folders. The exception is ignored when the temp directory is itself
/// refused for any other reason, or is a settings folder's top
/// (`TEMP=%APPDATA%` must not make Claude Desktop's folder a root).
///
/// A deny list rather than a test for a project (a `.git`, a `.scad`):
/// models live in plain folders, and every folder the list leaves out is
/// one the user chose to start an agent in.
pub fn unsafe_cwd(cwd: &Path, home: Option<&Path>, system: &SystemDirs) -> Option<&'static str> {
    let temp = system.temp.as_deref().filter(|t| {
        let bare = SystemDirs {
            temp: None,
            ..system.clone()
        };
        match judge(t, home, &bare) {
            Verdict::Fine => true,
            Verdict::Settings { depth, .. } => depth >= 2,
            Verdict::Refused(_) => false,
        }
    });
    match judge(cwd, home, system) {
        Verdict::Fine => None,
        Verdict::Settings { why, .. } => match temp {
            Some(t) if is_under(cwd, t) => None,
            _ => Some(why),
        },
        Verdict::Refused(why) => Some(why),
    }
}

/// [`unsafe_cwd`]'s verdict before the temp directory's exception.
enum Verdict {
    Fine,
    /// Under a settings folder in home, `depth` components below home
    /// (`~/AppData` is 1, `~/AppData/Local/Temp` 3).
    Settings {
        why: &'static str,
        depth: usize,
    },
    Refused(&'static str),
}

/// `p` is `dir` or inside it, compared as this platform compares paths
/// (case-insensitively on Windows).
fn is_under(p: &Path, dir: &Path) -> bool {
    if cfg!(windows) {
        let (p, d) = (
            p.to_string_lossy().to_ascii_lowercase(),
            dir.to_string_lossy().to_ascii_lowercase(),
        );
        Path::new(&p).starts_with(Path::new(&d))
    } else {
        p.starts_with(dir)
    }
}

fn judge(cwd: &Path, home: Option<&Path>, system: &SystemDirs) -> Verdict {
    let same = |a: &Path, b: &Path| {
        if cfg!(windows) {
            a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
        } else {
            a == b
        }
    };
    let under = is_under;
    if cwd.parent().is_none() {
        return Verdict::Refused("the file system's root");
    }
    // Found here but reported last: the temp exception lifts only this,
    // so a system folder must still be refused after it.
    let mut settings = None;
    if let Some(home) = home.filter(|h| h.parent().is_some()) {
        if same(cwd, home) {
            return Verdict::Refused("the home folder");
        }
        if under(home, cwd) {
            return Verdict::Refused("a folder that contains the home folder");
        }
        if under(cwd, home) {
            let rel = if cfg!(windows) {
                PathBuf::from(&cwd.to_string_lossy()[home.as_os_str().len()..])
            } else {
                cwd.strip_prefix(home)
                    .map(Path::to_path_buf)
                    .unwrap_or_default()
            };
            let names: Vec<String> = rel
                .components()
                .filter_map(|c| match c {
                    Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
                    _ => None,
                })
                .collect();
            let first = names.first().cloned().unwrap_or_default();
            let why = if first.starts_with('.') {
                Some("a hidden settings folder in the home folder")
            } else if ["library", "appdata"].contains(&first.to_lowercase().as_str()) {
                Some("an application settings folder in the home folder")
            } else {
                None
            };
            settings = why.map(|why| Verdict::Settings {
                why,
                depth: names.len(),
            });
        }
    }
    if system.trees.iter().any(|d| under(cwd, d)) {
        return Verdict::Refused("a system folder");
    }
    if system.shared.iter().any(|d| same(cwd, d)) {
        return Verdict::Refused("a folder the whole system shares");
    }
    settings.unwrap_or(Verdict::Fine)
}

/// The system's folders as [`unsafe_cwd`] judges them.
#[derive(Debug, Clone)]
pub struct SystemDirs {
    /// Never a root, nor anything under them.
    pub trees: Vec<PathBuf>,
    /// Never a root themselves; their subfolders may be (the temp
    /// directories, where scratch work and tests run, are under `/var` and
    /// `/tmp`).
    pub shared: Vec<PathBuf>,
    /// The user's temp directory, resolved (`std::env::temp_dir`): it and
    /// its subfolders may be roots though under a settings folder in home
    /// (Windows' `~\AppData\Local\Temp`); see [`unsafe_cwd`].
    pub temp: Option<PathBuf>,
}

impl SystemDirs {
    /// This platform's.
    pub fn here() -> SystemDirs {
        let paths = |v: &[&str]| v.iter().map(PathBuf::from).collect();
        // Resolved like the working directory it is compared with: on
        // Windows TEMP may hold an 8.3 short name (`RUNNER~1`) that only
        // canonicalizing spells out, and on macOS `/var` is a link.
        let t = std::env::temp_dir();
        let temp = Some(lang::paths::plain(t.canonicalize().unwrap_or(t)));
        if cfg!(windows) {
            let var =
                |k: &str, d: &str| PathBuf::from(std::env::var_os(k).unwrap_or_else(|| d.into()));
            SystemDirs {
                trees: vec![
                    var("SystemRoot", r"C:\Windows"),
                    var("ProgramFiles", r"C:\Program Files"),
                    var("ProgramFiles(x86)", r"C:\Program Files (x86)"),
                    var("ProgramData", r"C:\ProgramData"),
                ],
                shared: paths(&[r"C:\Users"]),
                temp,
            }
        } else {
            SystemDirs {
                trees: paths(&[
                    "/bin",
                    "/sbin",
                    "/usr",
                    "/etc",
                    "/dev",
                    "/proc",
                    "/sys",
                    "/boot",
                    "/lib",
                    "/lib64",
                    "/System",
                    "/Library",
                    "/Applications",
                    "/private/etc",
                    "/private/var/db",
                    "/private/var/root",
                    "/var/db",
                    "/var/lib",
                    "/var/log",
                    "/var/root",
                ]),
                shared: paths(&[
                    "/tmp",
                    "/private",
                    "/private/tmp",
                    "/private/var",
                    "/var",
                    "/var/tmp",
                    "/opt",
                    "/home",
                    "/Users",
                ]),
                temp,
            }
        }
    }
}

/// Symlinks followed before a path is judged unresolvable (Linux's
/// `MAXSYMLINKS`); a loop is refused like a path outside the roots.
const MAX_LINKS: u32 = 40;

/// Where `p` really leads: `.`/`..` folded lexically (the tools use the
/// folded path), then every symlink along it followed, the last one
/// included **even when it dangles**. A path that does not exist yet (an
/// output file) keeps its missing tail as written.
///
/// Following a dangling link matters for writes: `canonicalize` fails on
/// one, and judging it by its parent (inside the root) let
/// `out.stl -> ~/Library/LaunchAgents/x.plist` pass while the kernel
/// followed the link out of the root on write (the agent-surface audit's
/// finding 2). A link loop resolves to an empty path, which no root
/// contains.
pub fn resolve(p: &Path) -> PathBuf {
    // Roots are plain (see `Roots::new`), and a verbatim `\\?\C:\...`
    // spelling of a path inside one shares none of its components, so it
    // would be refused. `plain` only rewrites a verbatim path whose plain
    // spelling names the same file; any other stays verbatim and outside.
    let p = session::normal(&lang::paths::plain(p.to_path_buf()));
    let mut todo: std::collections::VecDeque<std::ffi::OsString> =
        std::collections::VecDeque::new();
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Prefix(_) | Component::RootDir => out.push(c.as_os_str()),
            _ => todo.push_back(c.as_os_str().to_os_string()),
        }
    }
    let mut hops = 0;
    while let Some(name) = todo.pop_front() {
        if name == "." {
            continue;
        }
        if name == ".." {
            out.pop();
            continue;
        }
        let next = out.join(&name);
        match std::fs::symlink_metadata(&next) {
            Ok(m) if m.file_type().is_symlink() => {
                hops += 1;
                let Ok(target) = std::fs::read_link(&next) else {
                    return PathBuf::new();
                };
                // Windows' `read_link` gives an absolute target in the
                // verbatim form (it turns the reparse point's `\??\` into
                // `\\?\`), so a link to elsewhere inside a root would
                // resolve outside it.
                let target = lang::paths::plain(target);
                if hops > MAX_LINKS {
                    return PathBuf::new();
                }
                // The target's components replace the link's; a relative
                // target is relative to the link's directory (`out`).
                if target.is_absolute() {
                    out = PathBuf::new();
                }
                for c in target.components().rev() {
                    match c {
                        Component::Prefix(_) | Component::RootDir => {}
                        _ => todo.push_front(c.as_os_str().to_os_string()),
                    }
                }
                if target.is_absolute() {
                    for c in target.components() {
                        match c {
                            Component::Prefix(_) | Component::RootDir => out.push(c.as_os_str()),
                            _ => break,
                        }
                    }
                }
            }
            _ => out = next,
        }
    }
    out
}

/// A file system that refuses everything outside the roots' readable
/// directories: reads fail with "permission denied", and outside files
/// do not exist, so the evaluator reports them as it reports any file it
/// cannot open.
pub struct RootedFs {
    base: Arc<dyn FileSystem + Send + Sync>,
    roots: Roots,
}

impl std::fmt::Debug for RootedFs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RootedFs")
            .field("roots", &self.roots)
            .finish()
    }
}

impl RootedFs {
    pub fn new(base: Arc<dyn FileSystem + Send + Sync>, roots: Roots) -> RootedFs {
        RootedFs { base, roots }
    }

    fn ok(&self, p: &Path) -> bool {
        // Relative paths are the working directory's, which is a root.
        if p.is_relative() {
            return self.roots.can_read(&self.roots.home().join(p));
        }
        self.roots.can_read(p)
    }
}

fn denied(p: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!("'{}' is outside the allowed roots", p.display()),
    )
}

impl FileSystem for RootedFs {
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        if !self.ok(path) {
            return Err(denied(path));
        }
        self.base.read(path)
    }
    fn exists(&self, path: &Path) -> bool {
        self.ok(path) && self.base.exists(path)
    }
    fn is_dir(&self, path: &Path) -> bool {
        self.ok(path) && self.base.is_dir(path)
    }
    fn canonicalize(&self, path: &Path) -> Option<PathBuf> {
        if !self.ok(path) {
            return None;
        }
        self.base.canonicalize(path)
    }
    fn metadata(&self, path: &Path) -> Option<Metadata> {
        if !self.ok(path) {
            return None;
        }
        self.base.metadata(path)
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        if !self.ok(path) {
            return Err(denied(path));
        }
        self.base.read_dir(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roots_refuse_escapes() {
        let tmp = std::env::temp_dir().join(format!("nsroots-{}", std::process::id()));
        let inside = tmp.join("inside");
        let outside = tmp.join("outside");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.scad"), "cube(1);").unwrap();
        let roots = Roots::new(std::slice::from_ref(&inside), &[]);
        // Plain, as the roots are and as paths joined onto the working
        // directory or sent by a client are. On Windows `canonicalize`
        // answers `\\?\C:\...`, where `/` is no separator: joined onto
        // that, `new/deep/model.stl` would be one (invalid) file name.
        let verbatim = inside.canonicalize().unwrap();
        let inside = lang::paths::plain(verbatim.clone());
        assert!(roots.can_write(&inside.join("new/deep/model.stl")));
        // A verbatim spelling of a path inside is the same file, and is
        // judged as its plain one; `..` inside one is not folded by
        // Windows, so that stays outside.
        assert!(roots.can_write(&verbatim.join("new").join("model.stl")));
        assert!(!roots.can_read(&verbatim.join("..").join("outside").join("secret.scad")));
        assert!(!roots.can_read(&inside.join("../outside/secret.scad")));
        assert!(!roots.can_write(Path::new("/etc/x")));
        // A symlink inside a root that points out of it is outside.
        #[cfg(unix)]
        {
            let link = inside.join("link");
            let _ = std::fs::remove_file(&link);
            std::os::unix::fs::symlink(&outside, &link).unwrap();
            assert!(!roots.can_read(&link.join("secret.scad")));
            let fs = RootedFs::new(Arc::new(lang::loader::StdFs), roots.clone());
            assert!(fs.read(&link.join("secret.scad")).is_err());
            assert!(!fs.exists(&link.join("secret.scad")));
            // A dangling link out of the root: its parent is inside, but a
            // write would follow it (the audit's repro).
            let dangling = inside.join("dangling.stl");
            let _ = std::fs::remove_file(&dangling);
            std::os::unix::fs::symlink(outside.join("newfile.stl"), &dangling).unwrap();
            assert!(!roots.can_write(&dangling));
            assert!(!roots.can_read(&dangling));
            // A relative dangling link that climbs out, and one through a
            // link to a directory.
            let rel = inside.join("rel.png");
            let _ = std::fs::remove_file(&rel);
            std::os::unix::fs::symlink("../outside/dangle.png", &rel).unwrap();
            assert!(!roots.can_write(&rel));
            // A dangling link that stays inside is fine, and resolves to
            // its target.
            let ok = inside.join("ok.stl");
            let _ = std::fs::remove_file(&ok);
            std::os::unix::fs::symlink("new/target.stl", &ok).unwrap();
            assert!(roots.can_write(&ok));
            assert_eq!(resolve(&ok), inside.join("new/target.stl"));
            // A loop is refused.
            let (a, b) = (inside.join("loop-a"), inside.join("loop-b"));
            let _ = std::fs::remove_file(&a);
            let _ = std::fs::remove_file(&b);
            std::os::unix::fs::symlink(&b, &a).unwrap();
            std::os::unix::fs::symlink(&a, &b).unwrap();
            assert!(!roots.can_write(&a.join("x.stl")));
            assert!(!outside.join("newfile.stl").exists());
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn only_a_project_like_working_directory_is_a_root() {
        let sys = SystemDirs::here();
        let home = Some(Path::new("/Users/ada"));
        let why = |p: &str| unsafe_cwd(Path::new(p), home, &sys);
        // Claude Desktop's `/`, the home folder and what contains it.
        assert_eq!(why("/"), Some("the file system's root"));
        assert_eq!(why("/Users/ada"), Some("the home folder"));
        assert_eq!(
            why("/Users"),
            Some("a folder that contains the home folder")
        );
        // Where settings, keys and launch agents live.
        assert!(why("/Users/ada/.ssh").is_some());
        assert!(why("/Users/ada/.config/claude").is_some());
        assert!(why("/Users/ada/Library/LaunchAgents").is_some());
        assert!(why("/Users/ada/Library/Application Support/Claude").is_some());
        // The system's trees, and the shared folders themselves.
        assert_eq!(why("/usr/local/bin"), Some("a system folder"));
        assert_eq!(why("/etc"), Some("a system folder"));
        assert_eq!(why("/Applications/Claude.app"), Some("a system folder"));
        assert_eq!(why("/tmp"), Some("a folder the whole system shares"));
        assert_eq!(
            why("/private/var"),
            Some("a folder the whole system shares")
        );
        // Projects: in the home folder, on another disk, in a temp folder.
        assert_eq!(why("/Users/ada/models"), None);
        assert_eq!(why("/Users/ada/Documents/gearbox"), None);
        assert_eq!(why("/Users/ada/my.models"), None);
        assert_eq!(why("/Volumes/Work/cad"), None);
        assert_eq!(why("/tmp/scratch"), None);
        assert_eq!(why("/private/var/folders/xy/T/test"), None);
        // No home known: the rest still holds.
        assert_eq!(
            unsafe_cwd(Path::new("/"), None, &sys),
            Some("the file system's root")
        );
        assert_eq!(unsafe_cwd(Path::new("/srv/cad"), None, &sys), None);
    }

    /// Windows' temp directory is `~\AppData\Local\Temp`; the same layout
    /// in Unix spelling checks the exception on every platform's CI.
    #[cfg(unix)]
    #[test]
    fn the_temp_directory_is_a_root_though_under_a_settings_folder() {
        let home = Some(Path::new("/Users/ada"));
        let with_temp = |t: &str| SystemDirs {
            temp: Some(PathBuf::from(t)),
            ..SystemDirs::here()
        };
        let sys = with_temp("/Users/ada/AppData/Local/Temp");
        let why = |p: &str| unsafe_cwd(Path::new(p), home, &sys);
        assert_eq!(why("/Users/ada/AppData/Local/Temp/nsmcp-1-tools"), None);
        assert_eq!(why("/Users/ada/AppData/Local/Temp"), None);
        // The rest of the settings folder is still refused.
        assert!(why("/Users/ada/AppData/Roaming/Claude").is_some());
        assert!(why("/Users/ada/AppData/Local").is_some());
        // A temp directory that is itself refused, or a settings folder's
        // top, excepts nothing.
        for bad in ["/", "/Users", "/Users/ada", "/Users/ada/AppData", "/usr"] {
            let sys = with_temp(bad);
            assert!(
                unsafe_cwd(Path::new("/Users/ada/AppData/Roaming/Claude"), home, &sys).is_some(),
                "TEMP={bad}"
            );
        }
        // Nor does it lift a system folder's refusal.
        let sys = with_temp("/usr/tmp");
        assert_eq!(
            unsafe_cwd(Path::new("/usr/tmp/x"), home, &sys),
            Some("a system folder")
        );
        // The real temp directory's scratch folders are roots here too.
        let real = SystemDirs::here();
        let t = real.temp.clone().unwrap().join("nsmcp-scratch");
        assert_eq!(unsafe_cwd(&t, home, &real), None);
    }

    #[cfg(windows)]
    #[test]
    fn only_a_project_like_working_directory_is_a_root() {
        let sys = SystemDirs::here();
        let home = Some(Path::new(r"C:\Users\ada"));
        let why = |p: &str| unsafe_cwd(Path::new(p), home, &sys);
        assert!(why(r"C:\").is_some());
        assert!(why(r"C:\Users\Ada").is_some());
        assert!(why(r"C:\Users").is_some());
        assert!(why(r"C:\Users\ada\AppData\Roaming\Claude").is_some());
        assert!(why(r"C:\Windows\System32").is_some());
        assert!(why(r"c:\program files\NeoSCAD").is_some());
        assert_eq!(why(r"C:\Users\ada\models"), None);
        assert_eq!(why(r"D:\cad"), None);
        // %TEMP% and its scratch folders, where tests and scripts run.
        let sys = SystemDirs {
            temp: Some(PathBuf::from(r"C:\Users\ada\AppData\Local\Temp")),
            ..SystemDirs::here()
        };
        let why = |p: &str| unsafe_cwd(Path::new(p), home, &sys);
        assert_eq!(why(r"C:\Users\ada\AppData\Local\Temp\nsmcp-1-tools"), None);
        assert_eq!(why(r"c:\users\ada\appdata\local\temp\x"), None);
        assert!(why(r"C:\Users\ada\AppData\Roaming\Claude").is_some());
        // The real one, under whatever home this runner has.
        let real = SystemDirs::here();
        let home = std::env::var_os("USERPROFILE").map(PathBuf::from);
        let home = home.map(|h| lang::paths::plain(h.canonicalize().unwrap_or(h)));
        let t = real.temp.clone().unwrap().join("nsmcp-scratch");
        assert_eq!(unsafe_cwd(&t, home.as_deref(), &real), None);
    }

    #[test]
    fn with_no_roots_the_refusal_says_how_to_add_one() {
        let roots = Roots::new(&[], &[]).with_unsafe_cwd(Path::new("/"), "the file system's root");
        assert!(roots.writable().is_empty());
        assert_eq!(roots.home(), Path::new("/"));
        assert!(!roots.can_write(Path::new("/x.stl")));
        let r = roots.refusal("export", Path::new("/x.stl"));
        assert!(r.contains("there are none"), "{r}");
        assert!(r.contains("--root"), "{r}");
        assert!(r.contains("inline `source` works"), "{r}");
    }

    #[test]
    fn an_app_documents_directory_is_readable_not_writable() {
        let tmp = std::env::temp_dir().join(format!("nsroots-doc-{}", std::process::id()));
        let (home, doc) = (tmp.join("home"), tmp.join("models"));
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&doc).unwrap();
        let roots = Roots::new(std::slice::from_ref(&home), &[]);
        let fs = RootedFs::new(Arc::new(lang::loader::StdFs), roots.clone());
        let doc = lang::paths::plain(doc.canonicalize().unwrap());
        assert!(!fs.exists(&doc));
        // A clone (the file system's) sees the change too.
        roots.set_document_dirs(std::slice::from_ref(&doc));
        assert!(fs.exists(&doc));
        assert!(roots.can_read(&doc.join("gear.scad")));
        assert!(!roots.can_write(&doc.join("gear.stl")));
        assert!(!roots.can_read(&tmp.join("elsewhere.scad")));
        // Replaced, not added to: a closed document's directory goes.
        roots.set_document_dirs(&[]);
        assert!(!roots.can_read(&doc.join("gear.scad")));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
