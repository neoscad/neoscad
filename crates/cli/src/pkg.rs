//! Project dependencies (`docs/dependencies.md`): `neoscad add`,
//! `remove`, `fetch`, `update`, `tree`, `vendor` and `store`, over the
//! `project` crate. A project is the nearest directory holding a
//! `scad.toml`; its locked dependencies are linked into
//! `.scad/libraries`, which every host puts on the library path of the
//! project's files, after the including file's directory and before
//! `OPENSCADPATH`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use clap::Parser;
use project::manifest::GitRef;
use project::{FetchOptions, FetchReport, Update};
use serde_json::{Value, json};

use crate::outcome::Outcome;

const EXIT_ERROR: u8 = 1;

/// Flags every command that fetches shares.
#[derive(clap::Args, Debug, Clone, Default)]
struct Common {
    /// The project: this directory or the nearest one above it with a
    /// scad.toml (default: the working directory).
    #[arg(long, value_name = "DIR")]
    project: Option<PathBuf>,

    /// The store of shared mirrors and checkouts (default: NEOSCAD_STORE,
    /// else the platform's cache directory; `neoscad store path`).
    #[arg(long, value_name = "DIR")]
    store: Option<PathBuf>,

    /// Never use the network: only mirrors and checkouts already in the
    /// store.
    #[arg(long)]
    offline: bool,

    /// Fail instead of changing scad.lock (for CI).
    #[arg(long)]
    locked: bool,

    /// `json`: one JSON object describing the run.
    #[arg(long, value_name = "FORMAT")]
    format: Option<String>,
}

#[derive(Parser, Debug)]
#[command(
    name = "neoscad fetch",
    about = "Fetch the dependencies scad.toml declares and link them into .scad/libraries",
    long_about = "Fetch the dependencies scad.toml declares and link them into .scad/libraries.\n\n\
        Resolves what scad.lock does not pin yet, writes scad.lock, checks each revision out once \
        into the shared store and links it into the project. With a current scad.lock and every \
        checkout in the store it runs no git at all.",
    version
)]
pub(crate) struct FetchArgs {
    #[command(flatten)]
    common: Common,

    /// After fetching, print the project's library directories for
    /// OPENSCADPATH on standard output.
    #[arg(long = "print-path")]
    print_path: bool,
}

#[derive(Parser, Debug)]
#[command(
    name = "neoscad update",
    about = "Resolve dependencies again from their remotes (all, or the NAMEs), then fetch",
    version
)]
pub(crate) struct UpdateArgs {
    /// Dependencies to update; none for all.
    names: Vec<String>,

    #[command(flatten)]
    common: Common,
}

#[derive(Parser, Debug)]
#[command(
    name = "neoscad add",
    about = "Add a dependency to scad.toml (creating it if needed), then fetch",
    after_help = "Examples:\n  neoscad add BOSL2 --git https://github.com/BelfrySCAD/BOSL2 --tag v2.0.716\n  neoscad add threads --git https://github.com/rcolyer/threads-scad --rev 6e3d2b4\n  neoscad add parts --path ../shared-parts",
    version
)]
pub(crate) struct AddArgs {
    /// The dependency's name: the folder `include <NAME/...>` uses,
    /// unless --name gives another.
    name: String,

    /// A git repository URL.
    #[arg(
        long,
        value_name = "URL",
        conflicts_with = "path",
        required_unless_present = "path"
    )]
    git: Option<String>,

    /// A git tag (versions like v1.2.3 are reconciled across
    /// dependencies).
    #[arg(long, value_name = "TAG", conflicts_with_all = ["rev", "branch", "path"])]
    tag: Option<String>,

    /// A commit id.
    #[arg(long, value_name = "COMMIT", conflicts_with_all = ["branch", "path"])]
    rev: Option<String>,

    /// A branch (pinned to its commit in scad.lock until `neoscad update`).
    #[arg(long, value_name = "BRANCH", conflicts_with = "path")]
    branch: Option<String>,

    /// A local directory, linked as it is (relative to the project).
    #[arg(long, value_name = "DIR")]
    path: Option<String>,

    /// The folder of the repository that holds the library's files.
    #[arg(long = "include-root", value_name = "DIR")]
    include_root: Option<String>,

    /// The folder name in .scad/libraries, if not NAME.
    #[arg(long = "name", value_name = "FOLDER")]
    folder: Option<String>,

    /// Only edit scad.toml.
    #[arg(long = "no-fetch")]
    no_fetch: bool,

    #[command(flatten)]
    common: Common,
}

#[derive(Parser, Debug)]
#[command(
    name = "neoscad remove",
    about = "Remove a dependency from scad.toml, then fetch (relinking)",
    version
)]
pub(crate) struct RemoveArgs {
    name: String,

    /// Only edit scad.toml.
    #[arg(long = "no-fetch")]
    no_fetch: bool,

    #[command(flatten)]
    common: Common,
}

#[derive(Parser, Debug)]
#[command(
    name = "neoscad tree",
    about = "Print the locked dependency graph",
    version
)]
pub(crate) struct TreeArgs {
    /// The project directory (default: the working directory).
    #[arg(long, value_name = "DIR")]
    project: Option<PathBuf>,

    /// `json`: the locked packages as one JSON object.
    #[arg(long, value_name = "FORMAT")]
    format: Option<String>,
}

#[derive(Parser, Debug)]
#[command(
    name = "neoscad vendor",
    about = "Copy the locked dependencies into a plain folder for stock OpenSCAD",
    long_about = "Copy the locked dependencies into a plain folder for stock OpenSCAD (default \
        <project>/libraries), fetching first, and print the OPENSCADPATH line that makes \
        OpenSCAD find them.",
    version
)]
pub(crate) struct VendorArgs {
    /// Where to copy them (default: <project>/libraries).
    dir: Option<PathBuf>,

    #[command(flatten)]
    common: Common,
}

#[derive(Parser, Debug)]
#[command(
    name = "neoscad store",
    about = "The shared store of git mirrors and checkouts: list, gc, path",
    version,
    disable_help_subcommand = true
)]
pub(crate) struct StoreArgs {
    #[command(subcommand)]
    what: StoreCmd,

    /// The store (default: NEOSCAD_STORE, else the platform's cache
    /// directory).
    #[arg(long, value_name = "DIR", global = true)]
    store: Option<PathBuf>,

    /// `json`: one JSON object.
    #[arg(long, value_name = "FORMAT", global = true)]
    format: Option<String>,
}

#[derive(clap::Subcommand, Debug)]
enum StoreCmd {
    /// Every mirror and checkout, its size and the projects that use it.
    List,
    /// Remove the mirrors and checkouts no project uses any more.
    Gc {
        /// Only list what would be removed.
        #[arg(long = "dry-run")]
        dry_run: bool,
    },
    /// Print the store's directory.
    Path,
}

fn parse<A: Parser>(name: &str, args: Vec<OsString>) -> Result<A, u8> {
    let argv = std::iter::once(OsString::from(name)).chain(args);
    A::try_parse_from(argv).map_err(|e| {
        let _ = e.print();
        if e.use_stderr() { EXIT_ERROR } else { 0 }
    })
}

fn json_flag(cmd: &str, f: &Option<String>) -> Result<bool, u8> {
    match f.as_deref() {
        None => Ok(false),
        Some("json") => Ok(true),
        Some(f) => Err(Outcome::fail(
            EXIT_ERROR,
            format!("neoscad {cmd}: unknown --format '{f}' (only json)"),
        )
        .emit()),
    }
}

/// The error, as text or JSON.
fn fail(cmd: &str, json: bool, e: impl std::fmt::Display) -> u8 {
    if json {
        Outcome {
            exit_code: EXIT_ERROR,
            stdout: format!(
                "{}\n",
                json!({"schema": 1, "exit_code": EXIT_ERROR, "error": e.to_string()})
            )
            .into_bytes(),
            stderr: Vec::new(),
        }
        .emit()
    } else {
        Outcome::fail(EXIT_ERROR, format!("neoscad {cmd}: {e}")).emit()
    }
}

fn start_dir(p: &Option<PathBuf>) -> PathBuf {
    let cwd = std::env::current_dir().unwrap_or_default();
    match p {
        Some(p) => cwd.join(p),
        None => cwd,
    }
}

/// The project root at or above `start`.
fn root_of(start: &Path) -> Result<PathBuf, String> {
    project::find_root(start).ok_or_else(|| {
        format!(
            "no scad.toml in {} or above; create one with `neoscad add NAME --git URL --tag TAG`",
            start.display()
        )
    })
}

fn options(c: &Common, update: Update) -> FetchOptions {
    FetchOptions {
        store: c.store.clone(),
        offline: c.offline,
        locked: c.locked,
        update,
    }
}

/// Fetch with progress on stderr (text mode).
fn run_fetch(root: &Path, opts: &FetchOptions, json: bool) -> project::Result<FetchReport> {
    let mut progress = |line: &str| {
        if !json {
            eprintln!("neoscad: {line}");
        }
    };
    project::fetch(root, opts, &mut progress)
}

/// The text summary of a fetch.
fn summary(r: &FetchReport) -> String {
    let n = r.packages.len();
    let deps = if n == 1 { "dependency" } else { "dependencies" };
    let mut s = String::new();
    let mut work = Vec::new();
    for (count, what) in [
        (r.cloned, "cloned"),
        (r.fetched, "fetched"),
        (r.checked_out, "checked out"),
    ] {
        if count > 0 {
            work.push(format!("{count} {what}"));
        }
    }
    let work = if work.is_empty() {
        String::new()
    } else {
        format!("; {}", work.join(", "))
    };
    if r.lock_changed {
        s.push_str(&format!(
            "neoscad: {n} {deps} locked in scad.lock and linked into .scad/libraries{work}\n"
        ));
        for p in &r.packages {
            let at = match &p.commit {
                Some(c) => format!(" ({})", &c[..c.len().min(7)]),
                None => String::new(),
            };
            let folder = if p.folder != p.name {
                format!(" as {}", p.folder)
            } else {
                String::new()
            };
            s.push_str(&format!("  {}{folder}: {}{at}\n", p.name, p.requested));
        }
    } else {
        s.push_str(&format!(
            "neoscad: {n} {deps} up to date in .scad/libraries{work}\n"
        ));
    }
    if !r.unlinked.is_empty() {
        s.push_str(&format!(
            "neoscad: unlinked {} (no longer a dependency)\n",
            r.unlinked.join(", ")
        ));
    }
    s
}

fn report_json(r: &FetchReport) -> Value {
    let packages: Vec<Value> = r.packages.iter().map(package_json).collect();
    json!({
        "schema": 1,
        "exit_code": 0,
        "root": r.root.to_string_lossy(),
        "store": r.store.to_string_lossy(),
        "library_dir": project::managed_dir(&r.root).to_string_lossy(),
        "resolved": r.resolved,
        "lock_changed": r.lock_changed,
        "cloned": r.cloned,
        "fetched": r.fetched,
        "checked_out": r.checked_out,
        "linked": r.linked,
        "unlinked": r.unlinked,
        "packages": packages,
    })
}

fn package_json(p: &project::Package) -> Value {
    json!({
        "name": p.name,
        "folder": p.folder,
        "source": p.source,
        "requested": p.requested,
        "commit": p.commit,
        "hash": p.hash,
        "target": p.target.to_string_lossy(),
    })
}

/// `OPENSCADPATH`'s separator.
const PATH_SEP: &str = if cfg!(windows) { ";" } else { ":" };

/// Print a successful fetch: the summary on stderr (or JSON on stdout).
fn emit_fetch(r: &FetchReport, json: bool, stdout_extra: String) -> u8 {
    let (stdout, stderr) = if json {
        (format!("{}\n", report_json(r)), String::new())
    } else {
        (stdout_extra, summary(r))
    };
    Outcome {
        exit_code: 0,
        stdout: stdout.into_bytes(),
        stderr: stderr.into_bytes(),
    }
    .emit()
}

/// Run `neoscad fetch`.
pub fn fetch_main(args: Vec<OsString>) -> u8 {
    let a: FetchArgs = match parse("neoscad fetch", args) {
        Ok(a) => a,
        Err(c) => return c,
    };
    let json = match json_flag("fetch", &a.common.format) {
        Ok(j) => j,
        Err(c) => return c,
    };
    let root = match root_of(&start_dir(&a.common.project)) {
        Ok(r) => r,
        Err(e) => return fail("fetch", json, e),
    };
    match run_fetch(&root, &options(&a.common, Update::Nothing), json) {
        Ok(r) => {
            let extra = if a.print_path {
                let dirs = project::search::project_for(&root.join(project::manifest::MANIFEST))
                    .map(|p| p.dirs)
                    .unwrap_or_default();
                let dirs: Vec<String> = dirs
                    .iter()
                    .map(|d| d.to_string_lossy().into_owned())
                    .collect();
                format!("{}\n", dirs.join(PATH_SEP))
            } else {
                String::new()
            };
            emit_fetch(&r, json, extra)
        }
        Err(e) => fail("fetch", json, e),
    }
}

/// Run `neoscad update`.
pub fn update_main(args: Vec<OsString>) -> u8 {
    let a: UpdateArgs = match parse("neoscad update", args) {
        Ok(a) => a,
        Err(c) => return c,
    };
    let json = match json_flag("update", &a.common.format) {
        Ok(j) => j,
        Err(c) => return c,
    };
    let root = match root_of(&start_dir(&a.common.project)) {
        Ok(r) => r,
        Err(e) => return fail("update", json, e),
    };
    let update = if a.names.is_empty() {
        Update::All
    } else {
        Update::Names(a.names.clone())
    };
    match run_fetch(&root, &options(&a.common, update), json) {
        Ok(r) => emit_fetch(&r, json, String::new()),
        Err(e) => fail("update", json, e),
    }
}

/// Run `neoscad add`.
pub fn add_main(args: Vec<OsString>) -> u8 {
    let a: AddArgs = match parse("neoscad add", args) {
        Ok(a) => a,
        Err(c) => return c,
    };
    let json = match json_flag("add", &a.common.format) {
        Ok(j) => j,
        Err(c) => return c,
    };
    let spec = match (&a.git, &a.path) {
        (Some(url), None) => project::AddSpec::Git {
            url: url.clone(),
            reference: match (&a.tag, &a.rev, &a.branch) {
                (Some(t), _, _) => GitRef::Tag(t.clone()),
                (_, Some(r), _) => GitRef::Rev(r.to_ascii_lowercase()),
                (_, _, Some(b)) => GitRef::Branch(b.clone()),
                _ => GitRef::Default,
            },
        },
        (None, Some(p)) => project::AddSpec::Path(p.clone()),
        _ => return fail("add", json, "give --git URL or --path DIR"),
    };
    let start = start_dir(&a.common.project);
    let before = project::find_root(&start).map(|r| {
        let p = r.join(project::manifest::MANIFEST);
        (p.clone(), std::fs::read(&p).ok())
    });
    let root = match project::add(
        &start,
        &a.name,
        spec,
        a.include_root.clone(),
        a.folder.clone(),
    ) {
        Ok(r) => r,
        Err(e) => return fail("add", json, e),
    };
    if a.no_fetch {
        return Outcome {
            exit_code: 0,
            stdout: if json {
                format!(
                    "{}\n",
                    json!({"schema": 1, "exit_code": 0, "root": root.to_string_lossy()})
                )
                .into_bytes()
            } else {
                Vec::new()
            },
            stderr: if json {
                Vec::new()
            } else {
                format!("neoscad: added `{}` to scad.toml\n", a.name).into_bytes()
            },
        }
        .emit();
    }
    match run_fetch(&root, &options(&a.common, Update::Nothing), json) {
        Ok(r) => emit_fetch(&r, json, String::new()),
        Err(e) => {
            // Leave the manifest as it was: a dependency that cannot be
            // fetched is a typo more often than not.
            match before {
                Some((p, Some(text))) => {
                    let _ = std::fs::write(&p, text);
                }
                _ => {
                    let _ = std::fs::remove_file(root.join(project::manifest::MANIFEST));
                }
            }
            fail("add", json, format!("{e} (scad.toml left unchanged)"))
        }
    }
}

/// Run `neoscad remove`.
pub fn remove_main(args: Vec<OsString>) -> u8 {
    let a: RemoveArgs = match parse("neoscad remove", args) {
        Ok(a) => a,
        Err(c) => return c,
    };
    let json = match json_flag("remove", &a.common.format) {
        Ok(j) => j,
        Err(c) => return c,
    };
    let root = match root_of(&start_dir(&a.common.project)) {
        Ok(r) => r,
        Err(e) => return fail("remove", json, e),
    };
    if let Err(e) = project::remove(&root, &a.name) {
        return fail("remove", json, e);
    }
    if a.no_fetch {
        return Outcome::fail(0, format!("neoscad: removed `{}` from scad.toml", a.name)).emit();
    }
    match run_fetch(&root, &options(&a.common, Update::Nothing), json) {
        Ok(r) => emit_fetch(&r, json, String::new()),
        Err(e) => fail("remove", json, e),
    }
}

/// Run `neoscad tree`.
pub fn tree_main(args: Vec<OsString>) -> u8 {
    let a: TreeArgs = match parse("neoscad tree", args) {
        Ok(a) => a,
        Err(c) => return c,
    };
    let json = match json_flag("tree", &a.format) {
        Ok(j) => j,
        Err(c) => return c,
    };
    let root = match root_of(&start_dir(&a.project)) {
        Ok(r) => r,
        Err(e) => return fail("tree", json, e),
    };
    match project::tree(&root) {
        Ok((text, packages, notes)) => {
            let (stdout, stderr) = if json {
                let p: Vec<Value> = packages.iter().map(package_json).collect();
                (
                    format!(
                        "{}\n",
                        json!({"schema": 1, "exit_code": 0, "root": root.to_string_lossy(), "packages": p, "notes": notes})
                    ),
                    String::new(),
                )
            } else {
                let n: String = notes.iter().map(|n| format!("neoscad: {n}\n")).collect();
                (text, n)
            };
            Outcome {
                exit_code: 0,
                stdout: stdout.into_bytes(),
                stderr: stderr.into_bytes(),
            }
            .emit()
        }
        Err(e) => fail("tree", json, e),
    }
}

/// Run `neoscad vendor`.
pub fn vendor_main(args: Vec<OsString>) -> u8 {
    let a: VendorArgs = match parse("neoscad vendor", args) {
        Ok(a) => a,
        Err(c) => return c,
    };
    let json = match json_flag("vendor", &a.common.format) {
        Ok(j) => j,
        Err(c) => return c,
    };
    let root = match root_of(&start_dir(&a.common.project)) {
        Ok(r) => r,
        Err(e) => return fail("vendor", json, e),
    };
    let r = match run_fetch(&root, &options(&a.common, Update::Nothing), json) {
        Ok(r) => r,
        Err(e) => return fail("vendor", json, e),
    };
    match project::vendor(&root, &r, a.dir.as_deref()) {
        Ok(v) => {
            let line = format!("OPENSCADPATH={}", v.dir.display());
            let (stdout, stderr) = if json {
                (
                    format!(
                        "{}\n",
                        json!({"schema": 1, "exit_code": 0, "dir": v.dir.to_string_lossy(), "folders": v.folders, "openscadpath": v.dir.to_string_lossy()})
                    ),
                    String::new(),
                )
            } else {
                (
                    format!("{line}\n"),
                    format!(
                        "neoscad: copied {} into {}; stock OpenSCAD finds them with\n",
                        if v.folders.is_empty() {
                            "no dependencies".to_string()
                        } else {
                            v.folders.join(", ")
                        },
                        v.dir.display()
                    ),
                )
            };
            Outcome {
                exit_code: 0,
                stdout: stdout.into_bytes(),
                stderr: stderr.into_bytes(),
            }
            .emit()
        }
        Err(e) => fail("vendor", json, e),
    }
}

/// A byte count for people.
fn size(b: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut v = b as f64;
    let mut u = 0;
    while v >= 1024.0 && u + 1 < UNITS.len() {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{b} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// Run `neoscad store`.
pub fn store_main(args: Vec<OsString>) -> u8 {
    let a: StoreArgs = match parse("neoscad store", args) {
        Ok(a) => a,
        Err(c) => return c,
    };
    let json = match json_flag("store", &a.format) {
        Ok(j) => j,
        Err(c) => return c,
    };
    if let StoreCmd::Path = a.what {
        return match project::store::locate(a.store.as_deref()) {
            Ok(p) if json => Outcome {
                exit_code: 0,
                stdout: format!(
                    "{}\n",
                    json!({"schema": 1, "exit_code": 0, "store": p.to_string_lossy()})
                )
                .into_bytes(),
                stderr: Vec::new(),
            }
            .emit(),
            Ok(p) => Outcome {
                exit_code: 0,
                stdout: format!("{}\n", p.display()).into_bytes(),
                stderr: Vec::new(),
            }
            .emit(),
            Err(e) => fail("store", json, e),
        };
    }
    let store = match project::store::Store::open(a.store.as_deref()) {
        Ok(s) => s,
        Err(e) => return fail("store", json, e),
    };
    let (stdout, stderr) = match a.what {
        StoreCmd::Path => unreachable!("answered above"),
        StoreCmd::List => {
            let l = store.list();
            if json {
                let entries: Vec<Value> = l
                    .entries
                    .iter()
                    .map(|e| {
                        json!({
                            "kind": e.kind,
                            "name": e.name,
                            "path": e.path.to_string_lossy(),
                            "bytes": e.bytes,
                            "used_by": e.used_by.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>(),
                        })
                    })
                    .collect();
                (
                    json!({"schema": 1, "exit_code": 0, "store": l.root.to_string_lossy(), "entries": entries, "projects": l.projects.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>()}).to_string() + "\n",
                    String::new(),
                )
            } else {
                let mut s = format!("{}\n", l.root.display());
                let total: u64 = l.entries.iter().map(|e| e.bytes).sum();
                for e in &l.entries {
                    let users = if e.used_by.is_empty() {
                        "unused".to_string()
                    } else {
                        let n = e.used_by.len();
                        format!("{n} project{}", if n == 1 { "" } else { "s" })
                    };
                    s.push_str(&format!(
                        "  {:<8} {:>9}  {:<10} {}\n",
                        e.kind,
                        size(e.bytes),
                        users,
                        e.name
                    ));
                }
                s.push_str(&format!(
                    "{} in {} entries, used by {} project{}\n",
                    size(total),
                    l.entries.len(),
                    l.projects.len(),
                    if l.projects.len() == 1 { "" } else { "s" }
                ));
                (s, String::new())
            }
        }
        StoreCmd::Gc { dry_run } => match store.gc(dry_run) {
            Ok(r) => {
                if json {
                    (
                        json!({"schema": 1, "exit_code": 0, "dry_run": dry_run, "removed": r.removed.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>(), "bytes": r.bytes, "forgotten": r.forgotten.iter().map(|p| p.to_string_lossy()).collect::<Vec<_>>()}).to_string() + "\n",
                        String::new(),
                    )
                } else {
                    let verb = if dry_run { "would remove" } else { "removed" };
                    let mut s = String::new();
                    for p in &r.removed {
                        s.push_str(&format!("{verb} {}\n", p.display()));
                    }
                    s.push_str(&format!(
                        "neoscad store gc: {verb} {} entries ({})\n",
                        r.removed.len(),
                        size(r.bytes)
                    ));
                    (String::new(), s)
                }
            }
            Err(e) => return fail("store", json, e),
        },
    };
    Outcome {
        exit_code: 0,
        stdout: stdout.into_bytes(),
        stderr: stderr.into_bytes(),
    }
    .emit()
}
