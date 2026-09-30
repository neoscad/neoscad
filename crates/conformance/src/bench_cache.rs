//! The reference-result cache of `conformance bench`:
//! `progress/bench/ref-cache.json`.
//!
//! A reference binary's time on a model changes only when the binary, the
//! model, the method or the machine does, yet timing the three references
//! is most of a full run (about 40 minutes, including CGAL and 2021.01
//! timeouts of 300 s each). So every reference result is stored under a key
//! naming everything it depends on, and a later run with the same key
//! reuses it. neoscad, the thing under test, is never cached.
//!
//! There is one entry per (reference, model): a run whose key differs
//! re-measures and replaces it. The key is compared field by field, so a
//! miss can say what changed ("model.text_sha256", "binary.sha256").

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use crate::ctx::git;

/// The cache file's own layout.
pub(crate) const CACHE_SCHEMA: u32 = 1;

/// The version of the timing method (crates/bench-core, `timing.rs`).
/// Every key carries it, so bumping it invalidates the whole cache.
pub(crate) use bench_core::timing::METHOD_VERSION;

/// The first commit whose timing code is [`METHOD_VERSION`]'s:
/// `crates/conformance/src/bench.rs` and `conformance/bench.json` are
/// unchanged from it to the commit that added the cache (the timing code
/// has since moved, unchanged, to crates/bench-core). Seeding accepts
/// only runs measured at or after it. Move it with [`METHOD_VERSION`].
pub(crate) const METHOD_SINCE: &str = "ab912d216135e794b0e2e78fd2b5af8e7029757a";

/// Environment variables the bench removes from every run.
pub(crate) use bench_core::timing::UNSET_ENV;

/// Which cached results a run may use.
#[derive(Debug, Default, Clone)]
pub(crate) struct Policy {
    /// `--fresh-refs`: re-measure every reference.
    pub fresh_all: bool,
    /// `--fresh-ref ID`: re-measure these references.
    pub fresh: Vec<String>,
    /// `--refs-max-age DAYS`: entries older than this are misses.
    pub max_age_days: Option<f64>,
}

/// Who measured a result, and when.
#[derive(Debug, Clone)]
pub(crate) struct Measured {
    pub at_unix: u64,
    /// ISO 8601 UTC.
    pub at: String,
    pub sha: String,
    pub dirty: bool,
    /// The result file a seeded entry came from.
    pub seeded_from: Option<String>,
}

pub(crate) struct RefCache {
    path: PathBuf,
    entries: Map<String, Value>,
}

/// Keys of a result that belong to one run, not to the measurement: the
/// mesh comparison is against that run's neoscad, and the cache marks are
/// added on the way out.
const PER_RUN_FIELDS: [&str; 4] = ["mesh_vs_neoscad", "cached", "measured_at", "measured_sha"];

impl RefCache {
    /// The cache at `path`; empty when it doesn't exist. An unreadable or
    /// malformed file is reported and treated as empty rather than failing
    /// the run: the cache only saves time, and the next store rewrites it.
    pub(crate) fn load(path: &Path) -> RefCache {
        let mut entries = Map::new();
        if let Ok(text) = fs::read_to_string(path) {
            match serde_json::from_str::<Value>(&text) {
                Ok(v) if v["schema"] == json!(CACHE_SCHEMA) => {
                    if let Some(e) = v["entries"].as_object() {
                        entries = e.clone();
                    }
                }
                Ok(_) => eprintln!(
                    "note: {} has another schema; starting an empty cache",
                    path.display()
                ),
                Err(e) => eprintln!("note: {}: {e}; starting an empty cache", path.display()),
            }
        }
        RefCache {
            path: path.to_path_buf(),
            entries,
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.entries.len()
    }

    /// The cached result of `reference` on `model` under `key`, marked
    /// `cached: true` with its `measured_at`; or why there is none.
    pub(crate) fn lookup(
        &self,
        reference: &str,
        model: &str,
        key: &Value,
        policy: &Policy,
        now_unix: u64,
    ) -> Result<Value, String> {
        if policy.fresh_all || policy.fresh.iter().any(|f| f == reference) {
            return Err("fresh measurement requested".into());
        }
        let Some(e) = self.entries.get(&entry_name(reference, model)) else {
            return Err("not cached".into());
        };
        let diff = key_diff(&e["key"], key);
        if !diff.is_empty() {
            return Err(format!("key differs: {}", diff.join(", ")));
        }
        if let (Some(max), Some(at)) = (policy.max_age_days, e["measured_at_unix"].as_u64()) {
            let age = now_unix.saturating_sub(at) as f64 / 86_400.0;
            if age > max {
                return Err(format!("{age:.1} days old (limit {max})"));
            }
        }
        let mut r = e["result"].clone();
        r["cached"] = json!(true);
        r["measured_at"] = e["measured_at"].clone();
        r["measured_sha"] = e["measured_sha"].clone();
        Ok(r)
    }

    /// Store a fresh (or seeded) result and write the file, so that an
    /// interrupted run keeps what it measured.
    pub(crate) fn store(
        &mut self,
        reference: &str,
        model: &str,
        key: &Value,
        result: &Value,
        m: &Measured,
    ) -> Result<(), String> {
        self.insert(reference, model, key, result, m);
        self.save()
    }

    fn insert(&mut self, reference: &str, model: &str, key: &Value, result: &Value, m: &Measured) {
        let mut result = result.clone();
        if let Some(o) = result.as_object_mut() {
            for f in PER_RUN_FIELDS {
                o.remove(f);
            }
        }
        let mut e = json!({
            "reference": reference,
            "model": model,
            "measured_at": m.at,
            "measured_at_unix": m.at_unix,
            "measured_sha": m.sha,
            "measured_dirty": m.dirty,
            "key": key,
            "result": result,
        });
        if let Some(f) = &m.seeded_from {
            e["seeded_from"] = json!(f);
        }
        self.entries.insert(entry_name(reference, model), e);
    }

    /// Seed an entry unless one measured later is already there.
    pub(crate) fn seed(
        &mut self,
        reference: &str,
        model: &str,
        key: &Value,
        result: &Value,
        m: &Measured,
    ) -> bool {
        let newer = self
            .entries
            .get(&entry_name(reference, model))
            .and_then(|e| e["measured_at_unix"].as_u64())
            .is_some_and(|t| t > m.at_unix);
        if !newer {
            self.insert(reference, model, key, result, m);
        }
        !newer
    }

    pub(crate) fn save(&self) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let doc = json!({
            "schema": CACHE_SCHEMA,
            "comment": "Reference results of `conformance bench`, reused while their key matches (crates/conformance/README.md, \"Reference-result cache\"). Safe to delete.",
            "entries": self.entries,
        });
        let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())? + "\n";
        // Write then rename, so a run killed mid-write can't leave half a
        // file that the next run would discard.
        let tmp = self.path.with_extension("json.tmp");
        fs::write(&tmp, text).map_err(|e| format!("{}: {e}", tmp.display()))?;
        fs::rename(&tmp, &self.path).map_err(|e| format!("{}: {e}", self.path.display()))
    }
}

fn entry_name(reference: &str, model: &str) -> String {
    format!("{reference}/{model}")
}

/// The cache key of one reference result. `kind` is `model`, `cold_start`
/// or `eval_only`, which are measured differently.
pub(crate) fn key(
    kind: &str,
    binary: &Value,
    args: &[String],
    env: &Value,
    model: &Value,
    method: &Value,
    machine: &Value,
) -> Value {
    json!({
        "method_version": METHOD_VERSION,
        "kind": kind,
        "binary": binary,
        "args": args,
        "env": env,
        "model": model,
        "method": method,
        "machine": machine,
    })
}

/// The fields of `new` that differ from `old`, one level into objects
/// (`model.text_sha256`), so a miss names its cause.
pub(crate) fn key_diff(old: &Value, new: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let (Some(o), Some(n)) = (old.as_object(), new.as_object()) else {
        if old != new {
            out.push("key".to_string());
        }
        return out;
    };
    let mut names: Vec<&String> = o.keys().chain(n.keys()).collect();
    names.sort();
    names.dedup();
    for k in names {
        let (a, b) = (&o.get(k), &n.get(k));
        if a == b {
            continue;
        }
        match (a.and_then(Value::as_object), b.and_then(Value::as_object)) {
            (Some(ao), Some(bo)) => {
                let mut sub: Vec<&String> = ao.keys().chain(bo.keys()).collect();
                sub.sort();
                sub.dedup();
                for s in sub {
                    if ao.get(s) != bo.get(s) {
                        out.push(format!("{k}.{s}"));
                    }
                }
            }
            _ => out.push(k.clone()),
        }
    }
    out
}

/// The identity of a reference binary: its resolved path, its whole
/// `--version` output, and the size and SHA-256 of the executable (about
/// 0.1 s for the 44 MB nightly; a size-and-mtime key would be cheaper but
/// misses a same-size rebuild and needlessly misses on a reinstall of the
/// same build). An app bundle's `Info.plist` is hashed too, since the
/// bundle's resources (fonts, libraries) are part of what runs.
pub(crate) fn binary_identity(bin: &Path, version: Option<&str>) -> Result<Value, String> {
    let path = fs::canonicalize(bin).map_err(|e| format!("{}: {e}", bin.display()))?;
    let data = fs::read(&path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut v = json!({
        "path": path.to_string_lossy(),
        "version": version,
        "size": data.len(),
        "sha256": crate::sha256::hex(&data),
    });
    if let Some(plist) = bundle_info_plist(&path)
        && let Ok(p) = fs::read(&plist)
    {
        v["bundle_info_plist_sha256"] = json!(crate::sha256::hex(&p));
    }
    Ok(v)
}

/// `X.app/Contents/Info.plist` for `X.app/Contents/MacOS/exe`.
pub(crate) fn bundle_info_plist(exe: &Path) -> Option<PathBuf> {
    let contents = exe.parent()?.parent()?;
    let p = contents.join("Info.plist");
    (contents.file_name()? == "Contents" && p.is_file()).then_some(p)
}

/// The environment a run gets: its working directory, `OPENSCADPATH`, the
/// variables removed and set, and any `OPENSCAD*`/`NEOSCAD*` variable
/// inherited from the caller's environment (which could change what a
/// binary does; nothing else inherited is known to).
pub(crate) fn env_identity(
    cwd: &Path,
    libpath: &Path,
    set: &[(&str, &str)],
    inherited: &BTreeMap<String, String>,
) -> Value {
    let set: BTreeMap<&str, &str> = set.iter().copied().collect();
    json!({
        "cwd": cwd.to_string_lossy(),
        "OPENSCADPATH": libpath.to_string_lossy(),
        "unset": UNSET_ENV,
        "set": set,
        "inherited": inherited,
    })
}

/// The caller's `OPENSCAD*`/`NEOSCAD*` variables, less the ones every run
/// overrides.
pub(crate) fn inherited_env(overridden: &[&str]) -> BTreeMap<String, String> {
    std::env::vars()
        .filter(|(k, _)| k.starts_with("OPENSCAD") || k.starts_with("NEOSCAD"))
        .filter(|(k, _)| !overridden.contains(&k.as_str()) && !UNSET_ENV.contains(&k.as_str()))
        .collect()
}

/// The identity of what a run reads: the model's text, each generated
/// input by content, and the libraries it may include.
pub(crate) fn model_identity(
    text: &str,
    inputs: &BTreeMap<String, String>,
    libraries: Option<&Value>,
) -> Value {
    json!({
        "text_sha256": crate::sha256::hex(text.as_bytes()),
        "inputs_sha256": inputs,
        "libraries": libraries,
    })
}

/// Whether a model reads library files: it declares a `requires`, or it
/// has an `include`/`use` (which resolves through `OPENSCADPATH`).
pub(crate) fn reads_libraries(text: &str, requires: &[String]) -> bool {
    !requires.is_empty() || text.contains("include <") || text.contains("use <")
}

/// The `OPENSCADPATH` corpus a model's includes resolve in: the entries of
/// the library directory (a new one could shadow a path) and, for each
/// library, its git commit and a hash of its uncommitted changes to
/// tracked files. Untracked files are left out: an include can only reach
/// them by naming them, and scratch directories come and go in these
/// checkouts. Checking out another commit, editing a file or adding a
/// library is a miss.
pub(crate) fn library_fingerprint(libdir: &Path, libraries: &[(String, PathBuf)]) -> Value {
    let mut entries: Vec<String> = fs::read_dir(libdir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .filter(|n| !n.starts_with('.'))
                .collect()
        })
        .unwrap_or_default();
    entries.sort();
    let mut libs = Map::new();
    for (name, p) in libraries {
        let commit = git(p, &["rev-parse", "HEAD"]);
        let changes = git(p, &["diff", "--no-ext-diff", "HEAD"]).unwrap_or_default();
        libs.insert(
            name.clone(),
            json!({
                "commit": commit,
                "tracked_changes_sha256": crate::sha256::hex(changes.as_bytes()),
            }),
        );
    }
    json!({"entries": entries, "libraries": libs})
}

/// Unix time of an ISO 8601 UTC timestamp `YYYY-MM-DDTHH:MM:SSZ`.
pub(crate) fn unix_from_iso(s: &str) -> Option<u64> {
    let b = s.as_bytes();
    if b.len() != 20 || b[4] != b'-' || b[7] != b'-' || b[10] != b'T' || b[19] != b'Z' {
        return None;
    }
    let n = |r: std::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (y, mo, d) = (n(0..4)?, n(5..7)?, n(8..10)?);
    let (h, mi, sec) = (n(11..13)?, n(14..16)?, n(17..19)?);
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    u64::try_from(days * 86_400 + h * 3600 + mi * 60 + sec).ok()
}

/// A file's status-change time. Unlike the modification time, no tool can
/// set it back, so a ctime before some moment proves the file's content has
/// been the same since.
pub(crate) fn ctime(p: &Path) -> Option<i64> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(p).ok().map(|m| m.ctime())
    }
    #[cfg(not(unix))]
    {
        let _ = p;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        let inputs = BTreeMap::from([("big.stl".to_string(), "ab".to_string())]);
        key(
            "model",
            &json!({"path": "/bin/x", "version": "X 1", "size": 3, "sha256": "00"}),
            &["--backend=cgal".to_string()],
            &env_identity(
                Path::new("/w"),
                Path::new("/lib"),
                &[("NEOSCAD_NO_SERVER", "1")],
                &BTreeMap::new(),
            ),
            &model_identity("cube(1);", &inputs, None),
            &json!({"runs": 3, "single_run_over_s": 60.0, "timeout_s": 300.0}),
            &json!({"model": "Mac", "os": "macOS 27.0 (26A428)", "power": "AC"}),
        )
    }

    #[test]
    fn key_names_every_dependency() {
        let k = sample();
        for f in [
            "method_version",
            "kind",
            "binary",
            "args",
            "env",
            "model",
            "method",
            "machine",
        ] {
            assert!(k.get(f).is_some(), "{f} missing");
        }
        assert_eq!(k["env"]["unset"], json!(UNSET_ENV));
        assert_eq!(k["env"]["set"]["NEOSCAD_NO_SERVER"], json!("1"));
        // The model is keyed by content, not by name.
        assert_eq!(
            k["model"]["text_sha256"],
            json!(crate::sha256::hex(b"cube(1);"))
        );
        assert_eq!(key_diff(&k, &sample()), Vec::<String>::new());
    }

    #[test]
    fn key_diff_names_what_changed() {
        let a = sample();
        let mut b = sample();
        b["model"]["text_sha256"] = json!("ff");
        b["method"]["timeout_s"] = json!(600.0);
        b["args"] = json!(["--backend=manifold"]);
        assert_eq!(
            key_diff(&a, &b),
            vec!["args", "method.timeout_s", "model.text_sha256"]
        );
        let mut c = sample();
        c["machine"]["power"] = json!("battery");
        assert_eq!(key_diff(&a, &c), vec!["machine.power"]);
    }

    #[test]
    fn lookup_hits_misses_and_expires() {
        let dir = std::env::temp_dir().join(format!("refcache-{}", std::process::id()));
        let path = dir.join("ref-cache.json");
        let _ = fs::remove_file(&path);
        let mut c = RefCache::load(&path);
        let k = sample();
        let m = Measured {
            at_unix: 1_000_000,
            at: "1970-01-12T13:46:40Z".into(),
            sha: "abc".into(),
            dirty: false,
            seeded_from: None,
        };
        let res =
            json!({"rc": "timeout", "runs_s": [null], "best_s": null, "mesh_vs_neoscad": "ok"});
        c.store("nightly-cgal", "m", &k, &res, &m).unwrap();
        let c = RefCache::load(&path);
        let p = Policy::default();
        let hit = c.lookup("nightly-cgal", "m", &k, &p, 1_000_100).unwrap();
        assert_eq!(hit["rc"], json!("timeout"));
        assert_eq!(hit["cached"], json!(true));
        assert_eq!(hit["measured_at"], json!("1970-01-12T13:46:40Z"));
        // The mesh comparison belongs to the run that made it.
        assert!(hit.get("mesh_vs_neoscad").is_none());
        // Another model, another reference, another key: misses.
        assert!(c.lookup("nightly-cgal", "n", &k, &p, 0).is_err());
        assert!(c.lookup("openscad-2021.01", "m", &k, &p, 0).is_err());
        let mut longer = k.clone();
        longer["method"]["timeout_s"] = json!(600.0);
        let e = c.lookup("nightly-cgal", "m", &longer, &p, 0).unwrap_err();
        assert!(e.contains("method.timeout_s"), "{e}");
        // Fresh flags and expiry.
        let fresh = Policy {
            fresh: vec!["nightly-cgal".into()],
            ..Policy::default()
        };
        assert!(c.lookup("nightly-cgal", "m", &k, &fresh, 0).is_err());
        let aged = Policy {
            max_age_days: Some(1.0),
            ..Policy::default()
        };
        assert!(
            c.lookup("nightly-cgal", "m", &k, &aged, 1_000_000 + 3600)
                .is_ok()
        );
        assert!(
            c.lookup("nightly-cgal", "m", &k, &aged, 1_000_000 + 2 * 86_400)
                .is_err()
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn seeding_never_replaces_a_newer_entry() {
        let mut c = RefCache {
            path: PathBuf::from("/nonexistent/ref-cache.json"),
            entries: Map::new(),
        };
        let k = sample();
        let at = |t: u64| Measured {
            at_unix: t,
            at: String::new(),
            sha: String::new(),
            dirty: false,
            seeded_from: Some("f.json".into()),
        };
        assert!(c.seed("r", "m", &k, &json!({"best_s": 2.0}), &at(20)));
        assert!(!c.seed("r", "m", &k, &json!({"best_s": 1.0}), &at(10)));
        assert_eq!(c.entries["r/m"]["result"]["best_s"], json!(2.0));
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn library_reads() {
        assert!(reads_libraries("include <BOSL2/std.scad>\n", &[]));
        assert!(reads_libraries("cube(1);", &["BOSL2".to_string()]));
        assert!(!reads_libraries("cube(1);", &[]));
    }

    #[test]
    fn iso_round_trips() {
        for t in [0u64, 951_782_400, 1_790_000_000, 4_102_444_799] {
            let (_, iso) = crate::record::utc_timestamps(t);
            assert_eq!(unix_from_iso(&iso), Some(t), "{iso}");
        }
        assert_eq!(unix_from_iso("2026-09-27"), None);
    }
}
