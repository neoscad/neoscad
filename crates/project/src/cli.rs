//! The command line (feature `cli`): `add`, `remove`, `fetch`, `update`,
//! `tree`, `vendor` and `store`, as the `scad-deps` binary runs them and
//! as `neoscad` runs them under its own name. Each command is a function
//! of a [`Tool`] (the program's name and version, for usage lines and the
//! commands messages suggest) and the arguments after the command's word.
//!
//! Progress and summaries go to stderr; `--format json` prints one JSON
//! object (`schema: 1`) on stdout, and `fetch --print-path` and `vendor`
//! print their one line there.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{CommandFactory, FromArgMatches};
use serde_json::{Value, json};

use crate::manifest::GitRef;
use crate::{FetchOptions, FetchReport, Update};

const EXIT_ERROR: u8 = 1;

/// The program the commands run as: its name starts usage lines and the
/// commands messages suggest (`run \`scad-deps fetch\``), and `--version`
/// prints its version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tool {
    pub name: &'static str,
    pub version: &'static str,
}

impl Tool {
    /// The standalone `scad-deps` binary.
    pub const SCAD_DEPS: Tool = Tool {
        name: "scad-deps",
        version: env!("CARGO_PKG_VERSION"),
    };
}

/// One command: its word, the function that runs it on the arguments
/// after that word, and its clap definition.
#[derive(Debug, Clone, Copy)]
pub struct Command {
    pub name: &'static str,
    pub run: fn(Tool, Vec<OsString>) -> u8,
    pub command: fn(Tool) -> clap::Command,
}

/// Every command, in the order help lists them.
pub const COMMANDS: &[Command] = &[
    Command {
        name: "add",
        run: add_main,
        command: add_command,
    },
    Command {
        name: "remove",
        run: remove_main,
        command: remove_command,
    },
    Command {
        name: "fetch",
        run: fetch_main,
        command: fetch_command,
    },
    Command {
        name: "update",
        run: update_main,
        command: update_command,
    },
    Command {
        name: "tree",
        run: tree_main,
        command: tree_command,
    },
    Command {
        name: "vendor",
        run: vendor_main,
        command: vendor_command,
    },
    Command {
        name: "store",
        run: store_main,
        command: store_command,
    },
];

/// Flags every command that fetches shares.
#[derive(clap::Args, Debug, Clone, Default)]
struct Common {
    /// The project: this directory or the nearest one above it with a
    /// scad.toml (default: the working directory).
    #[arg(long, value_name = "DIR")]
    project: Option<PathBuf>,

    /// The store of shared mirrors and checkouts (default: NEOSCAD_STORE,
    /// else the platform's cache directory; `store path` prints it).
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

#[derive(clap::Parser, Debug)]
#[command(
    about = "Fetch the dependencies scad.toml declares and link them into .scad/libraries",
    long_about = "Fetch the dependencies scad.toml declares and link them into .scad/libraries.\n\n\
        Resolves what scad.lock does not pin yet, writes scad.lock, checks each revision out once \
        into the shared store and links it into the project. With a current scad.lock and every \
        checkout in the store it runs no git at all."
)]
struct FetchArgs {
    #[command(flatten)]
    common: Common,

    /// After fetching, print the project's library directories for
    /// OPENSCADPATH on standard output.
    #[arg(long = "print-path")]
    print_path: bool,
}

#[derive(clap::Parser, Debug)]
#[command(about = "Resolve dependencies again from their remotes (all, or the NAMEs), then fetch")]
struct UpdateArgs {
    /// Dependencies to update; none for all.
    names: Vec<String>,

    #[command(flatten)]
    common: Common,
}

#[derive(clap::Parser, Debug)]
#[command(about = "Add a dependency to scad.toml (creating it if needed), then fetch")]
struct AddArgs {
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

    /// A branch (pinned to its commit in scad.lock until `update`).
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

#[derive(clap::Parser, Debug)]
#[command(about = "Remove a dependency from scad.toml, then fetch (relinking)")]
struct RemoveArgs {
    name: String,

    /// Only edit scad.toml.
    #[arg(long = "no-fetch")]
    no_fetch: bool,

    #[command(flatten)]
    common: Common,
}

#[derive(clap::Parser, Debug)]
#[command(about = "Print the locked dependency graph")]
struct TreeArgs {
    /// The project directory (default: the working directory).
    #[arg(long, value_name = "DIR")]
    project: Option<PathBuf>,

    /// `json`: the locked packages as one JSON object.
    #[arg(long, value_name = "FORMAT")]
    format: Option<String>,
}

#[derive(clap::Parser, Debug)]
#[command(
    about = "Copy the locked dependencies into a plain folder for stock OpenSCAD",
    long_about = "Copy the locked dependencies into a plain folder for stock OpenSCAD (default \
        <project>/libraries), fetching first, and print the OPENSCADPATH line that makes \
        OpenSCAD find them."
)]
struct VendorArgs {
    /// Where to copy them (default: <project>/libraries).
    dir: Option<PathBuf>,

    #[command(flatten)]
    common: Common,
}

#[derive(clap::Parser, Debug)]
#[command(
    about = "The shared store of git mirrors and checkouts: list, gc, path",
    disable_help_subcommand = true
)]
struct StoreArgs {
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

/// `A`'s definition as `<tool> <word>`.
fn command_of<A: CommandFactory>(tool: Tool, word: &str) -> clap::Command {
    A::command()
        .name(format!("{} {word}", tool.name))
        .version(tool.version)
}

/// The clap definition of `add`.
pub fn add_command(tool: Tool) -> clap::Command {
    let t = tool.name;
    command_of::<AddArgs>(tool, "add").after_help(format!(
        "Examples:\n  {t} add BOSL2 --git https://github.com/BelfrySCAD/BOSL2 --tag v2.0.716\n  {t} add threads --git https://github.com/rcolyer/threads-scad --rev 6e3d2b4\n  {t} add parts --path ../shared-parts"
    ))
}

/// The clap definition of `remove`.
pub fn remove_command(tool: Tool) -> clap::Command {
    command_of::<RemoveArgs>(tool, "remove")
}

/// The clap definition of `fetch`.
pub fn fetch_command(tool: Tool) -> clap::Command {
    command_of::<FetchArgs>(tool, "fetch")
}

/// The clap definition of `update`.
pub fn update_command(tool: Tool) -> clap::Command {
    command_of::<UpdateArgs>(tool, "update")
}

/// The clap definition of `tree`.
pub fn tree_command(tool: Tool) -> clap::Command {
    command_of::<TreeArgs>(tool, "tree")
}

/// The clap definition of `vendor`.
pub fn vendor_command(tool: Tool) -> clap::Command {
    command_of::<VendorArgs>(tool, "vendor")
}

/// The clap definition of `store`.
pub fn store_command(tool: Tool) -> clap::Command {
    command_of::<StoreArgs>(tool, "store")
}

/// Parse `args` (after the command's word) with `cmd`; help, a version
/// request or an error is printed, and its exit code returned.
fn parse<A: FromArgMatches>(mut cmd: clap::Command, args: Vec<OsString>) -> Result<A, u8> {
    let argv = std::iter::once(OsString::from(cmd.get_name().to_string())).chain(args);
    cmd.try_get_matches_from_mut(argv)
        .and_then(|m| A::from_arg_matches(&m))
        .map_err(|e| {
            let e = e.format(&mut cmd);
            let _ = e.print();
            if e.use_stderr() { EXIT_ERROR } else { 0 }
        })
}

/// Print `stderr`, then `stdout`, and return `code`.
fn emit(code: u8, stdout: impl AsRef<[u8]>, stderr: impl AsRef<[u8]>) -> u8 {
    let mut e = std::io::stderr().lock();
    let _ = e.write_all(stderr.as_ref());
    let _ = e.flush();
    let mut o = std::io::stdout().lock();
    let _ = o.write_all(stdout.as_ref());
    let _ = o.flush();
    code
}

/// One line on stderr, and `code`.
fn say(code: u8, line: impl std::fmt::Display) -> u8 {
    emit(code, "", format!("{line}\n"))
}

fn json_flag(tool: Tool, cmd: &str, f: &Option<String>) -> Result<bool, u8> {
    match f.as_deref() {
        None => Ok(false),
        Some("json") => Ok(true),
        Some(f) => Err(say(
            EXIT_ERROR,
            format!("{} {cmd}: unknown --format '{f}' (only json)", tool.name),
        )),
    }
}

/// The error, as text or JSON.
fn fail(tool: Tool, cmd: &str, json: bool, e: impl std::fmt::Display) -> u8 {
    if json {
        emit(
            EXIT_ERROR,
            format!(
                "{}\n",
                json!({"schema": 1, "exit_code": EXIT_ERROR, "error": e.to_string()})
            ),
            "",
        )
    } else {
        say(EXIT_ERROR, format!("{} {cmd}: {e}", tool.name))
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
fn root_of(tool: Tool, start: &Path) -> Result<PathBuf, String> {
    crate::find_root(start).ok_or_else(|| {
        format!(
            "no scad.toml in {} or above; create one with `{} add NAME --git URL --tag TAG`",
            start.display(),
            tool.name
        )
    })
}

fn options(tool: Tool, c: &Common, update: Update) -> FetchOptions {
    FetchOptions {
        store: c.store.clone(),
        offline: c.offline,
        locked: c.locked,
        update,
        program: tool.name.to_string(),
    }
}

/// Fetch with progress on stderr (text mode).
fn run_fetch(
    tool: Tool,
    root: &Path,
    opts: &FetchOptions,
    json: bool,
) -> crate::Result<FetchReport> {
    let mut progress = |line: &str| {
        if !json {
            eprintln!("{}: {line}", tool.name);
        }
    };
    crate::fetch(root, opts, &mut progress)
}

/// The text summary of a fetch.
fn summary(tool: Tool, r: &FetchReport) -> String {
    let t = tool.name;
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
            "{t}: {n} {deps} locked in scad.lock and linked into .scad/libraries{work}\n"
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
            "{t}: {n} {deps} up to date in .scad/libraries{work}\n"
        ));
    }
    if !r.unlinked.is_empty() {
        s.push_str(&format!(
            "{t}: unlinked {} (no longer a dependency)\n",
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
        "library_dir": crate::managed_dir(&r.root).to_string_lossy(),
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

fn package_json(p: &crate::Package) -> Value {
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
fn emit_fetch(tool: Tool, r: &FetchReport, json: bool, stdout_extra: String) -> u8 {
    let (stdout, stderr) = if json {
        (format!("{}\n", report_json(r)), String::new())
    } else {
        (stdout_extra, summary(tool, r))
    };
    emit(0, stdout, stderr)
}

/// Run `fetch`.
pub fn fetch_main(tool: Tool, args: Vec<OsString>) -> u8 {
    let a: FetchArgs = match parse(fetch_command(tool), args) {
        Ok(a) => a,
        Err(c) => return c,
    };
    let json = match json_flag(tool, "fetch", &a.common.format) {
        Ok(j) => j,
        Err(c) => return c,
    };
    let root = match root_of(tool, &start_dir(&a.common.project)) {
        Ok(r) => r,
        Err(e) => return fail(tool, "fetch", json, e),
    };
    match run_fetch(
        tool,
        &root,
        &options(tool, &a.common, Update::Nothing),
        json,
    ) {
        Ok(r) => {
            let extra = if a.print_path {
                let dirs = crate::search::project_for(&root.join(crate::manifest::MANIFEST))
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
            emit_fetch(tool, &r, json, extra)
        }
        Err(e) => fail(tool, "fetch", json, e),
    }
}

/// Run `update`.
pub fn update_main(tool: Tool, args: Vec<OsString>) -> u8 {
    let a: UpdateArgs = match parse(update_command(tool), args) {
        Ok(a) => a,
        Err(c) => return c,
    };
    let json = match json_flag(tool, "update", &a.common.format) {
        Ok(j) => j,
        Err(c) => return c,
    };
    let root = match root_of(tool, &start_dir(&a.common.project)) {
        Ok(r) => r,
        Err(e) => return fail(tool, "update", json, e),
    };
    let update = if a.names.is_empty() {
        Update::All
    } else {
        Update::Names(a.names.clone())
    };
    match run_fetch(tool, &root, &options(tool, &a.common, update), json) {
        Ok(r) => emit_fetch(tool, &r, json, String::new()),
        Err(e) => fail(tool, "update", json, e),
    }
}

/// Run `add`.
pub fn add_main(tool: Tool, args: Vec<OsString>) -> u8 {
    let a: AddArgs = match parse(add_command(tool), args) {
        Ok(a) => a,
        Err(c) => return c,
    };
    let json = match json_flag(tool, "add", &a.common.format) {
        Ok(j) => j,
        Err(c) => return c,
    };
    let spec = match (&a.git, &a.path) {
        (Some(url), None) => crate::AddSpec::Git {
            url: url.clone(),
            reference: match (&a.tag, &a.rev, &a.branch) {
                (Some(t), _, _) => GitRef::Tag(t.clone()),
                (_, Some(r), _) => GitRef::Rev(r.to_ascii_lowercase()),
                (_, _, Some(b)) => GitRef::Branch(b.clone()),
                _ => GitRef::Default,
            },
        },
        (None, Some(p)) => crate::AddSpec::Path(p.clone()),
        _ => return fail(tool, "add", json, "give --git URL or --path DIR"),
    };
    let start = start_dir(&a.common.project);
    let before = crate::find_root(&start).map(|r| {
        let p = r.join(crate::manifest::MANIFEST);
        (p.clone(), std::fs::read(&p).ok())
    });
    let root = match crate::add(
        &start,
        &a.name,
        spec,
        a.include_root.clone(),
        a.folder.clone(),
    ) {
        Ok(r) => r,
        Err(e) => return fail(tool, "add", json, e),
    };
    if a.no_fetch {
        return emit(
            0,
            if json {
                format!(
                    "{}\n",
                    json!({"schema": 1, "exit_code": 0, "root": root.to_string_lossy()})
                )
                .into_bytes()
            } else {
                Vec::new()
            },
            if json {
                Vec::new()
            } else {
                format!("{}: added `{}` to scad.toml\n", tool.name, a.name).into_bytes()
            },
        );
    }
    match run_fetch(
        tool,
        &root,
        &options(tool, &a.common, Update::Nothing),
        json,
    ) {
        Ok(r) => emit_fetch(tool, &r, json, String::new()),
        Err(e) => {
            // Leave the manifest as it was: a dependency that cannot be
            // fetched is a typo more often than not.
            match before {
                Some((p, Some(text))) => {
                    let _ = std::fs::write(&p, text);
                }
                _ => {
                    let _ = std::fs::remove_file(root.join(crate::manifest::MANIFEST));
                }
            }
            fail(tool, "add", json, format!("{e} (scad.toml left unchanged)"))
        }
    }
}

/// Run `remove`.
pub fn remove_main(tool: Tool, args: Vec<OsString>) -> u8 {
    let a: RemoveArgs = match parse(remove_command(tool), args) {
        Ok(a) => a,
        Err(c) => return c,
    };
    let json = match json_flag(tool, "remove", &a.common.format) {
        Ok(j) => j,
        Err(c) => return c,
    };
    let root = match root_of(tool, &start_dir(&a.common.project)) {
        Ok(r) => r,
        Err(e) => return fail(tool, "remove", json, e),
    };
    if let Err(e) = crate::remove(&root, &a.name) {
        return fail(tool, "remove", json, e);
    }
    if a.no_fetch {
        return say(
            0,
            format!("{}: removed `{}` from scad.toml", tool.name, a.name),
        );
    }
    match run_fetch(
        tool,
        &root,
        &options(tool, &a.common, Update::Nothing),
        json,
    ) {
        Ok(r) => emit_fetch(tool, &r, json, String::new()),
        Err(e) => fail(tool, "remove", json, e),
    }
}

/// Run `tree`.
pub fn tree_main(tool: Tool, args: Vec<OsString>) -> u8 {
    let a: TreeArgs = match parse(tree_command(tool), args) {
        Ok(a) => a,
        Err(c) => return c,
    };
    let json = match json_flag(tool, "tree", &a.format) {
        Ok(j) => j,
        Err(c) => return c,
    };
    let root = match root_of(tool, &start_dir(&a.project)) {
        Ok(r) => r,
        Err(e) => return fail(tool, "tree", json, e),
    };
    match crate::tree(&root, tool.name) {
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
                let n: String = notes
                    .iter()
                    .map(|n| format!("{}: {n}\n", tool.name))
                    .collect();
                (text, n)
            };
            emit(0, stdout.into_bytes(), stderr.into_bytes())
        }
        Err(e) => fail(tool, "tree", json, e),
    }
}

/// Run `vendor`.
pub fn vendor_main(tool: Tool, args: Vec<OsString>) -> u8 {
    let a: VendorArgs = match parse(vendor_command(tool), args) {
        Ok(a) => a,
        Err(c) => return c,
    };
    let json = match json_flag(tool, "vendor", &a.common.format) {
        Ok(j) => j,
        Err(c) => return c,
    };
    let root = match root_of(tool, &start_dir(&a.common.project)) {
        Ok(r) => r,
        Err(e) => return fail(tool, "vendor", json, e),
    };
    let r = match run_fetch(
        tool,
        &root,
        &options(tool, &a.common, Update::Nothing),
        json,
    ) {
        Ok(r) => r,
        Err(e) => return fail(tool, "vendor", json, e),
    };
    match crate::vendor(&root, &r, a.dir.as_deref()) {
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
                        "{}: copied {} into {}; stock OpenSCAD finds them with\n",
                        tool.name,
                        if v.folders.is_empty() {
                            "no dependencies".to_string()
                        } else {
                            v.folders.join(", ")
                        },
                        v.dir.display()
                    ),
                )
            };
            emit(0, stdout.into_bytes(), stderr.into_bytes())
        }
        Err(e) => fail(tool, "vendor", json, e),
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

/// Run `store`.
pub fn store_main(tool: Tool, args: Vec<OsString>) -> u8 {
    let a: StoreArgs = match parse(store_command(tool), args) {
        Ok(a) => a,
        Err(c) => return c,
    };
    let json = match json_flag(tool, "store", &a.format) {
        Ok(j) => j,
        Err(c) => return c,
    };
    if let StoreCmd::Path = a.what {
        return match crate::store::locate(a.store.as_deref()) {
            Ok(p) if json => emit(
                0,
                format!(
                    "{}\n",
                    json!({"schema": 1, "exit_code": 0, "store": p.to_string_lossy()})
                )
                .into_bytes(),
                Vec::new(),
            ),
            Ok(p) => emit(0, format!("{}\n", p.display()).into_bytes(), Vec::new()),
            Err(e) => fail(tool, "store", json, e),
        };
    }
    let store = match crate::store::Store::open(a.store.as_deref()) {
        Ok(s) => s,
        Err(e) => return fail(tool, "store", json, e),
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
                        "{} store gc: {verb} {} entries ({})\n",
                        tool.name,
                        r.removed.len(),
                        size(r.bytes)
                    ));
                    (String::new(), s)
                }
            }
            Err(e) => return fail(tool, "store", json, e),
        },
    };
    emit(0, stdout.into_bytes(), stderr.into_bytes())
}
