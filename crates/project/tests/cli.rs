//! The `scad-deps` command line end to end: `add`, `remove`, `fetch`,
//! `update`, `tree`, `vendor` and `store` over local git repositories
//! (file:// URLs, no network), each test with its own store. Needs `git`
//! on the PATH.

use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::Value;

const BIN: &str = env!("CARGO_BIN_EXE_scad-deps");

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
    let d = std::env::temp_dir().join(format!("scad-deps-{}-{name}", std::process::id()));
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

struct Env {
    store: PathBuf,
}

impl Env {
    fn new(base: &Path) -> Env {
        Env {
            store: base.join("store"),
        }
    }

    fn run(&self, dir: &Path, args: &[&str]) -> Output {
        self.run_with(dir, args, &[])
    }

    fn run_with(&self, dir: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
        let mut c = Command::new(BIN);
        c.args(args)
            .current_dir(dir)
            .env("NEOSCAD_STORE", &self.store)
            .env_remove("OPENSCADPATH")
            .env_remove("NEOSCAD_GIT")
            .stdin(Stdio::null());
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
            "scad-deps {args:?}:\n{}{}",
            text(&o.stderr),
            text(&o.stdout)
        );
        o
    }

    /// `scad-deps <args> --format json`, which must succeed.
    fn json(&self, dir: &Path, args: &[&str]) -> Value {
        let mut a = args.to_vec();
        a.extend(["--format", "json"]);
        let o = self.ok(dir, &a);
        serde_json::from_slice(&o.stdout).unwrap_or_else(|e| panic!("{e}: {}", text(&o.stdout)))
    }
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

fn project(base: &Path, name: &str) -> PathBuf {
    let d = base.join(name);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn lib_v(v: &str) -> String {
    format!("module hello() {{ echo(\"{v}\"); cube(1); }}\n")
}

/// What `include <Lib/x.scad>` reads in `app`.
fn linked(app: &Path) -> String {
    std::fs::read_to_string(app.join(".scad/libraries/Lib/x.scad")).unwrap()
}

#[test]
fn add_fetches_locks_and_links() {
    let base = scratch("add");
    let env = Env::new(&base);
    let u = repo(&base.join("lib"), &[("x.scad", &lib_v("v1"))], "v1.0.0");
    let app = project(&base, "app");
    let o = env.ok(&app, &["add", "Lib", "--git", &u, "--tag", "v1.0.0"]);
    assert!(
        text(&o.stderr).contains("scad-deps: 1 dependency locked"),
        "{}",
        text(&o.stderr)
    );
    let manifest = std::fs::read_to_string(app.join("scad.toml")).unwrap();
    assert_eq!(
        manifest,
        format!(
            "[package]\nname = \"app\"\n\n[dependencies]\nLib = {{ git = \"{u}\", tag = \"v1.0.0\" }}\n"
        )
    );
    let lock = std::fs::read_to_string(app.join("scad.lock")).unwrap();
    let commit = git(&base.join("lib"), &["rev-parse", "HEAD"]);
    assert!(
        lock.contains(&format!(
            "source = \"git+{u}\"\ntag = \"v1.0.0\"\ncommit = \"{commit}\"\nhash = \"sha256:"
        )),
        "{lock}"
    );
    // The link leads into the store, whose copy is read-only.
    let link = app.join(".scad/libraries/Lib");
    let target = std::fs::read_link(&link).unwrap();
    assert!(
        target.starts_with(env.store.canonicalize().unwrap().join("checkouts")),
        "{}",
        target.display()
    );
    assert!(
        std::fs::metadata(target.join("x.scad"))
            .unwrap()
            .permissions()
            .readonly()
    );
    assert!(!target.join(".git").exists());
    assert_eq!(
        std::fs::read_to_string(app.join(".scad/.gitignore"))
            .unwrap()
            .lines()
            .last(),
        Some("*")
    );
    assert_eq!(linked(&app), lib_v("v1"));
    // A second fetch only checks the links: no git at all.
    let o = env.run_with(&app, &["fetch"], &[("NEOSCAD_GIT", "/nonexistent/git")]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert!(
        text(&o.stderr).contains("up to date"),
        "{}",
        text(&o.stderr)
    );
    // A subdirectory belongs to the project too.
    std::fs::create_dir_all(app.join("parts")).unwrap();
    env.ok(&app.join("parts"), &["fetch"]);
    let tree = env.ok(&app, &["tree"]);
    assert_eq!(
        text(&tree.stdout),
        format!("app\n└── Lib v1.0.0 ({}) {u}\n", &commit[..7])
    );
    let printed = env.ok(&app, &["fetch", "--print-path"]);
    assert_eq!(
        text(&printed.stdout),
        format!("{}\n", app.join(".scad/libraries").display())
    );
}

#[test]
fn two_projects_share_one_checkout() {
    let base = scratch("share");
    let env = Env::new(&base);
    let u = repo(&base.join("lib"), &[("x.scad", &lib_v("v1"))], "v1.0.0");
    let a = project(&base, "a");
    let b = project(&base, "b");
    let first = env.json(&a, &["add", "Lib", "--git", &u, "--tag", "v1.0.0"]);
    assert_eq!(
        (first["cloned"].as_u64(), first["checked_out"].as_u64()),
        (Some(1), Some(1))
    );
    let second = env.json(&b, &["add", "Lib", "--git", &u, "--tag", "v1.0.0"]);
    assert_eq!(
        (
            second["cloned"].as_u64(),
            second["fetched"].as_u64(),
            second["checked_out"].as_u64()
        ),
        (Some(0), Some(0), Some(0)),
        "{second}"
    );
    let ra = std::fs::canonicalize(a.join(".scad/libraries/Lib")).unwrap();
    let rb = std::fs::canonicalize(b.join(".scad/libraries/Lib")).unwrap();
    assert_eq!(ra, rb);
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let ia = std::fs::metadata(ra.join("x.scad")).unwrap().ino();
        let ib = std::fs::metadata(rb.join("x.scad")).unwrap().ino();
        assert_eq!(ia, ib);
    }
    let listing = env.json(&a, &["store", "list"]);
    let entries = listing["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2, "{listing}");
    for e in entries {
        assert_eq!(e["used_by"].as_array().unwrap().len(), 2, "{e}");
    }
    // Both lockfiles say the same.
    assert_eq!(
        std::fs::read_to_string(a.join("scad.lock")).unwrap(),
        std::fs::read_to_string(b.join("scad.lock")).unwrap()
    );
}

#[test]
fn update_moves_a_tag_and_fetch_does_not() {
    let base = scratch("update");
    let env = Env::new(&base);
    let lib = base.join("lib");
    let u = repo(&lib, &[("x.scad", &lib_v("v1"))], "v1.0.0");
    let app = project(&base, "app");
    env.ok(&app, &["add", "Lib", "--git", &u, "--tag", "v1.0.0"]);
    let old = std::fs::read_to_string(app.join("scad.lock")).unwrap();
    // Upstream moves the tag.
    let moved = commit(&lib, &[("x.scad", &lib_v("v1-moved"))], "v1.0.0");
    // The manifest has not changed, so fetch keeps the lock.
    env.ok(&app, &["fetch"]);
    assert_eq!(std::fs::read_to_string(app.join("scad.lock")).unwrap(), old);
    assert_eq!(linked(&app), lib_v("v1"));
    // --locked passes while nothing would change; update fails with it.
    env.ok(&app, &["fetch", "--locked"]);
    let o = env.run(&app, &["update", "--locked"]);
    assert_eq!(o.status.code(), Some(1));
    let r = env.json(&app, &["update", "Lib"]);
    assert_eq!(r["lock_changed"], true, "{r}");
    assert_eq!(r["fetched"], 1, "{r}");
    let lock = std::fs::read_to_string(app.join("scad.lock")).unwrap();
    assert!(lock.contains(&moved), "{lock}");
    assert_eq!(linked(&app), lib_v("v1-moved"));
    let o = env.run(&app, &["update", "Nope"]);
    assert!(
        text(&o.stderr)
            .contains("`Nope` is not a dependency of this project (see `scad-deps tree`)"),
        "{}",
        text(&o.stderr)
    );
}

#[test]
fn locked_and_offline() {
    let base = scratch("offline");
    let env = Env::new(&base);
    let u = repo(&base.join("lib"), &[("x.scad", &lib_v("v1"))], "v1.0.0");
    let u2 = repo(&base.join("other"), &[("y.scad", "y = 1;\n")], "v0.1");
    let app = project(&base, "app");
    // No lockfile: --locked refuses, naming this command line.
    std::fs::write(
        app.join("scad.toml"),
        format!("[dependencies]\nLib = {{ git = \"{u}\", tag = \"v1.0.0\" }}\n"),
    )
    .unwrap();
    let o = env.run(&app, &["fetch", "--locked"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        text(&o.stderr).contains("no scad.lock; run `scad-deps fetch` without --locked"),
        "{}",
        text(&o.stderr)
    );
    // Offline with an empty store: says what is missing.
    let o = env.run(&app, &["fetch", "--offline"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("--offline"), "{}", text(&o.stderr));
    env.ok(&app, &["fetch"]);
    // A changed manifest: --locked refuses and leaves everything alone.
    let lock = std::fs::read_to_string(app.join("scad.lock")).unwrap();
    std::fs::write(
        app.join("scad.toml"),
        format!("[dependencies]\nLib = {{ git = \"{u}\", tag = \"v1.0.0\" }}\nOther = {{ git = \"{u2}\", tag = \"v0.1\" }}\n"),
    )
    .unwrap();
    let o = env.run(&app, &["fetch", "--locked"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        text(&o.stderr).contains("out of date"),
        "{}",
        text(&o.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(app.join("scad.lock")).unwrap(),
        lock
    );
    // Offline cannot reach Other's remote.
    let o = env.run(&app, &["fetch", "--offline"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(text(&o.stderr).contains("`Other`"), "{}", text(&o.stderr));
    // A second project, offline, from what the store already has.
    let b = project(&base, "b");
    std::fs::write(
        b.join("scad.toml"),
        format!("[dependencies]\nLib = {{ git = \"{u}\", tag = \"v1.0.0\" }}\n"),
    )
    .unwrap();
    let r = env.json(&b, &["fetch", "--offline"]);
    assert_eq!(r["cloned"], 0);
    assert_eq!(linked(&b), lib_v("v1"));
}

#[test]
fn a_modified_checkout_fails_the_hash() {
    let base = scratch("tamper");
    let env = Env::new(&base);
    let u = repo(&base.join("lib"), &[("x.scad", &lib_v("v1"))], "v1.0.0");
    let app = project(&base, "app");
    env.ok(&app, &["add", "Lib", "--git", &u, "--tag", "v1.0.0"]);
    let target = std::fs::canonicalize(app.join(".scad/libraries/Lib")).unwrap();
    writable(&target);
    std::fs::write(target.join("x.scad"), lib_v("evil")).unwrap();
    let o = env.run(&app, &["fetch"]);
    assert_eq!(o.status.code(), Some(1));
    let e = text(&o.stderr);
    assert!(
        e.contains("was modified") && e.contains("content hash"),
        "{e}"
    );
    // Deleting it lets the next fetch restore it.
    std::fs::remove_dir_all(&target).unwrap();
    let r = env.json(&app, &["fetch"]);
    assert_eq!(
        (r["checked_out"].as_u64(), r["cloned"].as_u64()),
        (Some(1), Some(0))
    );
    assert_eq!(linked(&app), lib_v("v1"));
}

#[test]
fn transitive_dependencies_and_conflicts() {
    let base = scratch("graph");
    let env = Env::new(&base);
    let ub = repo(
        &base.join("b"),
        &[("b.scad", "function b() = \"b1\";\n")],
        "v1.0.0",
    );
    let b11 = commit(
        &base.join("b"),
        &[("b.scad", "function b() = \"b11\";\n")],
        "v1.1.0",
    );
    let ua = repo(
        &base.join("a"),
        &[
            ("src/a.scad", "use <B/b.scad>\nmodule a() echo(b());\n"),
            (
                "scad.toml",
                &format!(
                    "[package]\nname = \"A\"\n\n[dependencies]\nB = {{ git = \"{ub}\", tag = \"v1.0.0\" }}\n"
                ),
            ),
        ],
        "v1",
    );
    let b_file =
        |app: &Path| std::fs::read_to_string(app.join(".scad/libraries/B/b.scad")).unwrap();
    // The app wants A (which wants B v1.0.0) and B v1.1.0: v1.1.0 wins.
    let app = project(&base, "app");
    std::fs::write(
        app.join("scad.toml"),
        format!("[package]\nname = \"app\"\n\n[dependencies]\nA = {{ git = \"{ua}\", tag = \"v1\", include_root = \"src\" }}\nB = {{ git = \"{ub}\", tag = \"v1.1.0\" }}\n"),
    )
    .unwrap();
    let r = env.json(&app, &["fetch"]);
    let pkgs = r["packages"].as_array().unwrap();
    assert_eq!(pkgs.len(), 2);
    assert_eq!(pkgs[1]["commit"], b11.as_str(), "{r}");
    assert_eq!(b_file(&app), "function b() = \"b11\";\n");
    // include_root: A's link is its src folder.
    assert!(app.join(".scad/libraries/A/a.scad").is_file());
    let tree = text(&env.ok(&app, &["tree"]).stdout);
    assert!(
        tree.contains("├── A v1 (")
            && tree.contains("│   └── B v1.1.0 (")
            && tree.contains("└── B v1.1.0 ("),
        "{tree}"
    );
    // Only A: B comes in through A's manifest.
    std::fs::write(
        app.join("scad.toml"),
        format!("[dependencies]\nA = {{ git = \"{ua}\", tag = \"v1\", include_root = \"src\" }}\n"),
    )
    .unwrap();
    env.ok(&app, &["fetch"]);
    assert_eq!(b_file(&app), "function b() = \"b1\";\n");
    // Exact revisions that disagree are a conflict naming both sides.
    let b1 = git(&base.join("b"), &["rev-parse", "v1.0.0"]);
    std::fs::write(
        app.join("scad.toml"),
        format!("[package]\nname = \"app\"\n\n[dependencies]\nA = {{ git = \"{ua}\", tag = \"v1\", include_root = \"src\" }}\nB = {{ git = \"{ub}\", rev = \"{}\" }}\n", &b11[..10]),
    )
    .unwrap();
    let o = env.run(&app, &["fetch"]);
    assert_eq!(o.status.code(), Some(1));
    let e = text(&o.stderr);
    assert!(
        e.contains("conflicting requirements for `B`")
            && e.contains("`app` wants rev")
            && e.contains("`A` wants tag v1.0.0")
            && e.contains(&b1[..7]),
        "{e}"
    );
}

#[test]
fn path_dependencies_and_project_paths() {
    let base = scratch("path");
    let env = Env::new(&base);
    let shared = base.join("shared");
    std::fs::create_dir_all(&shared).unwrap();
    std::fs::write(shared.join("x.scad"), lib_v("shared")).unwrap();
    let app = project(&base, "app");
    env.ok(&app, &["add", "Lib", "--path", "../shared"]);
    let link = app.join(".scad/libraries/Lib");
    assert_eq!(std::fs::canonicalize(&link).unwrap(), shared);
    // Linked, not copied: edits show through.
    std::fs::write(shared.join("x.scad"), lib_v("edited")).unwrap();
    assert_eq!(linked(&app), lib_v("edited"));
    assert!(
        std::fs::read_to_string(app.join("scad.lock"))
            .unwrap()
            .contains("source = \"path+../shared\"")
    );
    env.ok(&app, &["remove", "Lib"]);
    assert!(!link.exists());
    // [paths] libraries come after .scad/libraries on the printed path.
    std::fs::write(app.join("scad.toml"), "[paths]\nlibraries = [\"lib\"]\n").unwrap();
    let printed = env.ok(&app, &["fetch", "--print-path"]);
    let sep = if cfg!(windows) { ";" } else { ":" };
    assert_eq!(
        text(&printed.stdout),
        format!(
            "{}{sep}{}\n",
            app.join(".scad/libraries").display(),
            app.join("lib").display()
        )
    );
}

#[test]
fn vendor_writes_a_plain_folder() {
    let base = scratch("vendor");
    let env = Env::new(&base);
    let u = repo(&base.join("lib"), &[("x.scad", &lib_v("v1"))], "v1");
    let app = project(&base, "app");
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
    // Into another folder, as JSON.
    let v = env.json(&app, &["vendor", "out"]);
    let out = app.join("out").canonicalize().unwrap();
    assert_eq!(v["dir"], out.to_string_lossy().as_ref(), "{v}");
    assert_eq!(v["folders"], serde_json::json!(["Lib"]), "{v}");
    assert_eq!(
        std::fs::read_to_string(out.join("Lib/x.scad")).unwrap(),
        lib_v("v1")
    );
}

#[test]
fn gc_keeps_what_projects_use() {
    let base = scratch("gc");
    let env = Env::new(&base);
    let u = repo(&base.join("lib"), &[("x.scad", &lib_v("v1"))], "v1");
    let u2 = repo(&base.join("only-b"), &[("y.scad", "y = 2;\n")], "v1");
    let a = project(&base, "a");
    let b = project(&base, "b");
    env.ok(&a, &["add", "Lib", "--git", &u, "--tag", "v1"]);
    env.ok(&b, &["add", "Lib", "--git", &u, "--tag", "v1"]);
    env.ok(&b, &["add", "Only", "--git", &u2, "--tag", "v1"]);
    let before = env.json(&a, &["store", "list"]);
    assert_eq!(before["entries"].as_array().unwrap().len(), 4, "{before}");
    // Nothing is unused yet.
    let r = env.json(&a, &["store", "gc"]);
    assert_eq!(r["removed"].as_array().unwrap().len(), 0, "{r}");
    // Project b goes away.
    writable(&b);
    std::fs::remove_dir_all(&b).unwrap();
    let dry = env.json(&a, &["store", "gc", "--dry-run"]);
    assert_eq!(dry["removed"].as_array().unwrap().len(), 2, "{dry}");
    let r = env.json(&a, &["store", "gc"]);
    let removed: Vec<&str> = r["removed"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(removed.len(), 2, "{r}");
    assert!(removed.iter().all(|p| p.contains("only-b")), "{removed:?}");
    let after = env.json(&a, &["store", "list"]);
    assert_eq!(after["entries"].as_array().unwrap().len(), 2, "{after}");
    // a still fetches, with no git.
    let o = env.run_with(&a, &["fetch"], &[("NEOSCAD_GIT", "/nonexistent/git")]);
    assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
    assert_eq!(linked(&a), lib_v("v1"));
    // A removed dependency's checkout is unused after the next fetch.
    env.ok(&a, &["remove", "Lib"]);
    let r = env.json(&a, &["store", "gc"]);
    assert_eq!(r["removed"].as_array().unwrap().len(), 2, "{r}");
    let path = env.ok(&a, &["store", "path"]);
    assert_eq!(text(&path.stdout), format!("{}\n", env.store.display()));
}

#[test]
fn concurrent_fetches_share_the_store() {
    let base = scratch("concurrent");
    let env = Env::new(&base);
    let u = repo(&base.join("lib"), &[("x.scad", &lib_v("v1"))], "v1");
    let projects: Vec<PathBuf> = (0..4)
        .map(|i| {
            let p = project(&base, &format!("p{i}"));
            std::fs::write(
                p.join("scad.toml"),
                format!("[dependencies]\nLib = {{ git = \"{u}\", tag = \"v1\" }}\n"),
            )
            .unwrap();
            p
        })
        .collect();
    let children: Vec<_> = projects
        .iter()
        .map(|p| {
            Command::new(BIN)
                .args(["fetch", "--format", "json"])
                .current_dir(p)
                .env("NEOSCAD_STORE", &env.store)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    let mut cloned = 0;
    let mut checked_out = 0;
    for c in children {
        let o = c.wait_with_output().unwrap();
        assert_eq!(o.status.code(), Some(0), "{}", text(&o.stderr));
        let v: Value = serde_json::from_slice(&o.stdout).unwrap();
        cloned += v["cloned"].as_u64().unwrap();
        checked_out += v["checked_out"].as_u64().unwrap();
    }
    assert_eq!((cloned, checked_out), (1, 1));
    let targets: std::collections::BTreeSet<PathBuf> = projects
        .iter()
        .map(|p| std::fs::canonicalize(p.join(".scad/libraries/Lib")).unwrap())
        .collect();
    assert_eq!(targets.len(), 1);
    for p in &projects {
        assert_eq!(linked(p), lib_v("v1"));
    }
}

#[test]
fn add_leaves_the_manifest_alone_when_the_fetch_fails() {
    let base = scratch("add-fail");
    let env = Env::new(&base);
    let u = repo(&base.join("lib"), &[("x.scad", &lib_v("v1"))], "v1");
    let app = project(&base, "app");
    let o = env.run(&app, &["add", "Lib", "--git", &u, "--tag", "v9"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        text(&o.stderr).contains("tag v9 is not in"),
        "{}",
        text(&o.stderr)
    );
    assert!(!app.join("scad.toml").exists());
    env.ok(&app, &["add", "Lib", "--git", &u, "--tag", "v1"]);
    let before = std::fs::read_to_string(app.join("scad.toml")).unwrap();
    let o = env.run(
        &app,
        &[
            "add",
            "Lib2",
            "--git",
            &u,
            "--tag",
            "v1",
            "--include-root",
            "nope",
        ],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(
        text(&o.stderr).contains("include_root 'nope'"),
        "{}",
        text(&o.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(app.join("scad.toml")).unwrap(),
        before
    );
}

#[test]
fn help_and_version_name_the_program() {
    let base = scratch("help");
    let env = Env::new(&base);
    let v = env.ok(&base, &["--version"]);
    assert_eq!(
        text(&v.stdout),
        format!("scad-deps {}\n", env!("CARGO_PKG_VERSION"))
    );
    let h = env.ok(&base, &["--help"]);
    for c in [
        "add", "remove", "fetch", "update", "tree", "vendor", "store",
    ] {
        assert!(text(&h.stdout).contains(c), "{}", text(&h.stdout));
    }
    let h = env.ok(&base, &["add", "--help"]);
    assert!(
        text(&h.stdout).contains("Usage: scad-deps add")
            && text(&h.stdout).contains("scad-deps add BOSL2 --git"),
        "{}",
        text(&h.stdout)
    );
    // Outside a project: what to do, under this program's name.
    let o = env.run(&base, &["fetch"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        text(&o.stderr).starts_with("scad-deps fetch: no scad.toml in")
            && text(&o.stderr).contains("`scad-deps add NAME"),
        "{}",
        text(&o.stderr)
    );
    let o = env.run(&base, &["frobnicate"]);
    assert_ne!(o.status.code(), Some(0));
}
