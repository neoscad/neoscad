//! Project dependencies in NeoSCAD (`docs/dependencies.md`): the
//! scad-deps commands under `neoscad`'s name, and includes resolving
//! through `.scad/libraries` in every host's search order, over local git
//! repositories (file:// URLs, no network), each test with its own store.
//! The commands themselves (locking, the store, `--locked`, `--offline`,
//! gc, conflicts) are tested in `crates/project/tests/cli.rs`.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_neoscad");

/// A test's directory, removed (store and all) when the test passes.
struct Scratch(PathBuf);

impl std::ops::Deref for Scratch {
    type Target = PathBuf;
    fn deref(&self) -> &PathBuf {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if !std::thread::panicking() {
            writable(&self.0);
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

fn scratch(name: &str) -> Scratch {
    let d = std::env::temp_dir().join(format!("nsc-deps-{}-{name}", std::process::id()));
    if d.exists() {
        writable(&d);
        let _ = std::fs::remove_dir_all(&d);
    }
    std::fs::create_dir_all(&d).unwrap();
    Scratch(d.canonicalize().unwrap())
}

/// Undo the store's read-only checkouts, so a test can clean up.
fn writable(p: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(m) = std::fs::symlink_metadata(p) {
            if m.file_type().is_symlink() {
                return;
            }
            let mode = m.permissions().mode() | 0o200;
            let _ = std::fs::set_permissions(p, std::fs::Permissions::from_mode(mode));
            if m.is_dir() {
                for e in std::fs::read_dir(p).unwrap().flatten() {
                    writable(&e.path());
                }
            }
        }
    }
    #[cfg(not(unix))]
    let _ = p;
}

fn git(dir: &Path, args: &[&str]) -> String {
    let o = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "tag.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {}", text(&o.stderr));
    text(&o.stdout).trim().to_string()
}

/// A library repository at `dir` with `files`, committed and tagged
/// `tag`; returns its file:// URL.
fn repo(dir: &Path, files: &[(&str, &str)], tag: &str) -> String {
    std::fs::create_dir_all(dir).unwrap();
    if !dir.join(".git").exists() {
        git(dir, &["init", "-q"]);
    }
    commit(dir, files, tag);
    url(dir)
}

fn commit(dir: &Path, files: &[(&str, &str)], tag: &str) -> String {
    for (p, t) in files {
        let path = dir.join(p);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, t).unwrap();
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", tag]);
    if !tag.is_empty() {
        git(dir, &["tag", "-f", tag]);
    }
    git(dir, &["rev-parse", "HEAD"])
}

fn url(dir: &Path) -> String {
    let s = dir.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

/// The remote a dependency's own manifest names the test's repositories
/// by: a dependency may only name remote repositories, so git rewrites
/// this one to the test's directory ([`Env::with_host`]), as a user's
/// `insteadOf` would.
const HOST: &str = "https://scad-deps.invalid/";

struct Env {
    store: PathBuf,
    /// Git configuration for every run (`GIT_CONFIG_COUNT`).
    config: Vec<(String, String)>,
}

impl Env {
    fn new(base: &Path) -> Env {
        Env {
            store: base.join("store"),
            config: Vec::new(),
        }
    }

    /// [`HOST`] leads to the repositories under `base`, over git's file
    /// transport, which it allows a dependency's remote only when told to.
    fn with_host(base: &Path) -> Env {
        let mut env = Env::new(base);
        env.config = vec![
            (format!("url.{}/.insteadOf", url(base)), HOST.to_string()),
            ("protocol.file.allow".into(), "always".into()),
        ];
        env
    }

    fn run(&self, dir: &Path, args: &[&str]) -> Output {
        self.run_with(dir, args, &[])
    }

    fn run_with(&self, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(dir)
            .env("NEOSCAD_NO_SERVER", "1")
            .env("SCAD_STORE", &self.store)
            .env_remove("NEOSCAD_STORE")
            .env_remove("OPENSCADPATH")
            .env_remove("SCAD_GIT")
            .env_remove("NEOSCAD_GIT")
            .env_remove("GIT_PROTOCOL_FROM_USER")
            .stdin(Stdio::null());
        c.env("GIT_CONFIG_COUNT", self.config.len().to_string());
        for (i, (k, v)) in self.config.iter().enumerate() {
            c.env(format!("GIT_CONFIG_KEY_{i}"), k)
                .env(format!("GIT_CONFIG_VALUE_{i}"), v);
        }
        for (k, v) in env {
            c.env(k, v);
        }
        c.output().unwrap()
    }

    fn ok(&self, dir: &Path, args: &[&str]) -> Output {
        let o = self.run(dir, args);
        assert_eq!(
            o.status.code(),
            Some(0),
            "neoscad {args:?}:\n{}{}",
            text(&o.stderr),
            text(&o.stdout)
        );
        o
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

/// What a model echoes, through an `.echo` export.
fn echo(env: &Env, dir: &Path, file: &str, extra: &[(&str, &str)]) -> String {
    let o = env.run_with(dir, &[file, "-o", "out.echo"], extra);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    std::fs::read_to_string(dir.join("out.echo")).unwrap()
}

fn project(base: &Path, name: &str, main: &str) -> PathBuf {
    let d = base.join(name);
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("main.scad"), main).unwrap();
    d
}

const MAIN: &str = "include <Lib/x.scad>\nhello();\n";

fn lib_v(v: &str) -> String {
    format!("module hello() {{ echo(\"{v}\"); cube(1); }}\n")
}

#[test]
fn neoscad_add_fetch_update_and_includes() {
    let base = scratch("add");
    let env = Env::new(&base);
    let lib = base.join("lib");
    let u = repo(&lib, &[("x.scad", &lib_v("v1"))], "v1.0.0");
    let app = project(&base, "app", MAIN);
    let o = env.ok(&app, &["add", "Lib", "--git", &u, "--tag", "v1.0.0"]);
    assert!(
        text(&o.stderr).contains("neoscad: 1 dependency locked"),
        "{}",
        text(&o.stderr)
    );
    // The include resolves through the link.
    assert_eq!(echo(&env, &app, "main.scad", &[]), "ECHO: \"v1\"\n");
    // A subdirectory belongs to the project too.
    std::fs::create_dir_all(app.join("parts")).unwrap();
    std::fs::write(app.join("parts/p.scad"), MAIN).unwrap();
    assert_eq!(
        echo(&env, &app.join("parts"), "p.scad", &[]),
        "ECHO: \"v1\"\n"
    );
    // An update moves what the include finds.
    commit(&lib, &[("x.scad", &lib_v("v1-moved"))], "v1.0.0");
    env.ok(&app, &["update", "Lib"]);
    assert_eq!(echo(&env, &app, "main.scad", &[]), "ECHO: \"v1-moved\"\n");
    // Messages and usage name neoscad, and --version is neoscad's.
    let o = env.run(&app, &["update", "Nope"]);
    assert!(
        text(&o.stderr).contains("(see `neoscad tree`)"),
        "{}",
        text(&o.stderr)
    );
    let h = env.ok(&app, &["fetch", "--help"]);
    assert!(
        text(&h.stdout).contains("Usage: neoscad fetch"),
        "{}",
        text(&h.stdout)
    );
    let v = env.ok(&app, &["fetch", "--version"]);
    assert_eq!(
        text(&v.stdout),
        format!("neoscad fetch {}\n", env!("CARGO_PKG_VERSION"))
    );
    let printed = env.ok(&app, &["fetch", "--print-path"]);
    assert_eq!(
        text(&printed.stdout),
        format!("{}\n", app.join(".scad/libraries").display())
    );
}

#[test]
fn includes_reach_transitive_dependencies() {
    let base = scratch("graph");
    let env = Env::with_host(&base);
    let ub = format!("{HOST}b");
    repo(
        &base.join("b"),
        &[("b.scad", "function b() = \"b1\";\n")],
        "v1.0.0",
    );
    commit(
        &base.join("b"),
        &[("b.scad", "function b() = \"b11\";\n")],
        "v1.1.0",
    );
    let ua = format!("{HOST}a");
    repo(
        &base.join("a"),
        &[
            ("src/a.scad", "use <B/b.scad>\nmodule a() echo(b());\n"),
            (
                "scad.toml",
                &format!("[dependencies]\nB = {{ git = \"{ub}\", tag = \"v1.0.0\" }}\n"),
            ),
        ],
        "v1",
    );
    let app = project(&base, "app", "use <A/a.scad>\na();\n");
    std::fs::write(
        app.join("scad.toml"),
        format!("[dependencies]\nA = {{ git = \"{ua}\", tag = \"v1\", include_root = \"src\" }}\nB = {{ git = \"{ub}\", tag = \"v1.1.0\" }}\n"),
    )
    .unwrap();
    env.ok(&app, &["fetch"]);
    assert_eq!(echo(&env, &app, "main.scad", &[]), "ECHO: \"b11\"\n");
}

#[test]
fn search_order_and_hints() {
    let base = scratch("order");
    let env = Env::new(&base);
    let u = repo(&base.join("lib"), &[("x.scad", &lib_v("pinned"))], "v1");
    let app = project(&base, "app", MAIN);
    std::fs::write(
        app.join("scad.toml"),
        format!("[dependencies]\nLib = {{ git = \"{u}\", tag = \"v1\" }}\n"),
    )
    .unwrap();
    // OPENSCADPATH has another Lib.
    let env_lib = base.join("envlibs");
    std::fs::create_dir_all(env_lib.join("Lib")).unwrap();
    std::fs::write(env_lib.join("Lib/x.scad"), lib_v("env")).unwrap();
    let path = env_lib.to_string_lossy().into_owned();
    let with = [("OPENSCADPATH", path.as_str())];
    // Declared but not fetched: OPENSCADPATH's copy, and a hint.
    let o = env.run_with(&app, &["main.scad", "-o", "out.echo"], &[]);
    assert!(
        text(&o.stderr).contains(
            "neoscad: hint: `Lib` is declared in scad.toml but not fetched: run `neoscad fetch`"
        ),
        "{}",
        text(&o.stderr)
    );
    // -q keeps it quiet, as it does the warning it follows.
    let o = env.run_with(&app, &["main.scad", "-q", "-o", "out.echo"], &[]);
    assert!(!text(&o.stderr).contains("hint"), "{}", text(&o.stderr));
    let check = env.run(&app, &["check", "main.scad", "--format", "json"]);
    let v: Value = serde_json::from_slice(&check.stdout).unwrap();
    let hints = v["diagnostics"]["items"][0]["hints"][0]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(hints.contains("neoscad fetch"), "{v}");
    assert_eq!(echo(&env, &app, "main.scad", &with), "ECHO: \"env\"\n");
    env.ok(&app, &["fetch"]);
    // The pinned copy comes before OPENSCADPATH...
    assert_eq!(echo(&env, &app, "main.scad", &with), "ECHO: \"pinned\"\n");
    // ...in the session's hosts too (check runs on a session).
    let check = env.run_with(&app, &["check", "main.scad", "--format", "json"], &with);
    let v: Value = serde_json::from_slice(&check.stdout).unwrap();
    assert_eq!(v["diagnostics"]["echo"][0], "ECHO: \"pinned\"", "{v}");
    // ...and after the including file's own directory.
    std::fs::create_dir_all(app.join("Lib")).unwrap();
    std::fs::write(app.join("Lib/x.scad"), lib_v("local")).unwrap();
    assert_eq!(echo(&env, &app, "main.scad", &with), "ECHO: \"local\"\n");
    // An undeclared library gets the `add` hint.
    std::fs::write(app.join("o.scad"), "include <Other/y.scad>\n").unwrap();
    let o = env.run(&app, &["o.scad", "-o", "o.echo"]);
    assert!(
        text(&o.stderr).contains("neoscad add Other"),
        "{}",
        text(&o.stderr)
    );
    // Outside a project nothing changes.
    let outside = project(&base, "outside", MAIN);
    let o = env.run(&outside, &["main.scad", "-o", "out.echo"]);
    assert!(!text(&o.stderr).contains("hint"), "{}", text(&o.stderr));
}

#[test]
fn path_dependencies_and_project_paths() {
    let base = scratch("path");
    let env = Env::new(&base);
    let shared = base.join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::write(shared.join("x.scad"), lib_v("shared")).unwrap();
    let app = project(&base, "app", MAIN);
    env.ok(&app, &["add", "Lib", "--path", "../shared"]);
    let link = app.join(".scad/libraries/Lib");
    assert_eq!(std::fs::canonicalize(&link).unwrap(), shared);
    assert_eq!(echo(&env, &app, "main.scad", &[]), "ECHO: \"shared\"\n");
    // Edits show through: a path dependency is linked, not copied.
    std::fs::write(shared.join("x.scad"), lib_v("edited")).unwrap();
    assert_eq!(echo(&env, &app, "main.scad", &[]), "ECHO: \"edited\"\n");
    assert!(
        std::fs::read_to_string(app.join("scad.lock"))
            .unwrap()
            .contains("source = \"path+../shared\"")
    );
    // [paths] libraries: a folder the project keeps itself.
    env.ok(&app, &["remove", "Lib"]);
    assert!(!link.exists());
    std::fs::create_dir_all(app.join("lib/Lib")).unwrap();
    std::fs::write(app.join("lib/Lib/x.scad"), lib_v("kept")).unwrap();
    std::fs::write(app.join("scad.toml"), "[paths]\nlibraries = [\"lib\"]\n").unwrap();
    std::fs::create_dir_all(app.join("sub")).unwrap();
    std::fs::write(app.join("sub/m.scad"), MAIN).unwrap();
    assert_eq!(
        echo(&env, &app.join("sub"), "m.scad", &[]),
        "ECHO: \"kept\"\n"
    );
}

#[test]
fn vendor_for_stock_openscad() {
    let base = scratch("vendor");
    let env = Env::new(&base);
    let u = repo(&base.join("lib"), &[("x.scad", &lib_v("v1"))], "v1");
    let app = project(&base, "app", MAIN);
    env.ok(&app, &["add", "Lib", "--git", &u, "--tag", "v1"]);
    let o = env.ok(&app, &["vendor"]);
    let dir = app.join("libraries");
    assert_eq!(text(&o.stdout), format!("OPENSCADPATH={}\n", dir.display()));
    let f = dir.join("Lib/x.scad");
    let m = std::fs::symlink_metadata(&f).unwrap();
    assert!(m.is_file() && !m.permissions().readonly());
    assert!(
        !std::fs::symlink_metadata(dir.join("Lib"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_to_string(f).unwrap(), lib_v("v1"));
    // Without the managed links, OPENSCADPATH finds the vendored copy.
    std::fs::remove_dir_all(app.join(".scad")).unwrap();
    std::fs::remove_file(app.join("scad.toml")).unwrap();
    let p = dir.to_string_lossy().into_owned();
    assert_eq!(
        echo(&env, &app, "main.scad", &[("OPENSCADPATH", &p)]),
        "ECHO: \"v1\"\n"
    );
}
