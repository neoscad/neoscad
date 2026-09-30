//! A community benchmark result, schema 1: what `neoscad bench --json`
//! writes and what a submission carries. `bench/result.schema.json` is
//! the same schema as JSON Schema (draft 2020-12) for the benchmarks
//! repository's validation, and `docs/community-bench.md` explains every
//! field; the `schema_file_matches` test keeps the three in step.
//!
//! Nothing here may identify a person or a machine: no hostname, user
//! name, path, IP address or serial number (see [`crate::machine`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::machine::Machine;
use crate::official::Check;
use crate::timing::Measurement;

/// The result layout's version; bump it with any change a reader must
/// know about, and add the new version to the schema file.
pub const SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchResult {
    pub schema: u32,
    pub source: Source,
    pub neoscad: Neoscad,
    pub kit: KitInfo,
    pub method: Method,
    pub machine: Machine,
    /// The worker threads neoscad uses (the logical CPUs available to it).
    pub threads: u32,
    /// The OpenSCAD the user compared with, or null.
    pub openscad: Option<Reference>,
    pub cold_start: Pair,
    pub models: BTreeMap<String, Pair>,
    /// Models not run, and why (a model outside `--quick` is not listed).
    pub skipped: BTreeMap<String, String>,
    /// ISO 8601 UTC, to the second.
    pub started_at: String,
    pub finished_at: String,
}

/// Who ran it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    /// Someone running `neoscad bench`.
    User,
    /// The release workflow's own run on GitHub's runners
    /// (publish-packages.yml, `baseline`). The benchmarks repository
    /// accepts this only from that workflow's commits, never from an
    /// issue.
    CiBaseline,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Neoscad {
    pub version: String,
    /// The Rust target triple the binary was built for.
    pub target: String,
    /// SHA-256 of the running executable.
    pub sha256: String,
    /// Whether `sha256` is the release's published executable for
    /// `target`; the benchmarks repository re-checks it.
    pub official: bool,
    pub official_check: Check,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KitInfo {
    pub version: String,
    /// SHA-256 of the kit archive, when the kit came as one (null for an
    /// unpacked directory).
    pub archive_sha256: Option<String>,
    /// [`crate::kit::content_sha256`] of the unpacked kit.
    pub content_sha256: String,
    pub neoscad_commit: String,
    pub bosl2_commit: String,
    pub openscad_commit: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Method {
    /// [`crate::timing::METHOD_VERSION`].
    pub version: u32,
    /// Runs per model (best of), and for the cold start.
    pub runs: u32,
    pub cold_start_runs: u32,
    /// One run only once a run takes longer than this.
    pub single_run_over_s: f64,
    pub timeout_s: f64,
    pub quick: bool,
}

/// The OpenSCAD compared with: its `--version` line and backend. Never
/// its path, which would name the user's home directory.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reference {
    pub version: String,
    /// `manifold`, or `cgal` for a build without `--backend`.
    pub backend: String,
    /// The arguments added to every run (`--backend=manifold`).
    pub args: Vec<String>,
}

/// One model's times: neoscad's, and the reference's when there is one.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pair {
    pub neoscad: Measurement,
    pub openscad: Option<Measurement>,
}

/// The end-of-run summary: totals and the geometric mean speedup.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    /// Models run, and those neoscad finished.
    pub models: usize,
    pub neoscad_ok: usize,
    /// Sum of neoscad's best times over the models it finished.
    pub neoscad_total_s: f64,
    /// Models both finished, and the geometric mean of OpenSCAD's best
    /// over neoscad's (above 1: neoscad faster) over them.
    pub compared: usize,
    pub geomean_speedup: Option<f64>,
}

impl BenchResult {
    pub fn summary(&self) -> Summary {
        let mut s = Summary {
            models: self.models.len(),
            neoscad_ok: 0,
            neoscad_total_s: 0.0,
            compared: 0,
            geomean_speedup: None,
        };
        let mut log_sum = 0.0;
        for p in self.models.values() {
            let Some(n) = p.neoscad.best_s.filter(|_| p.neoscad.rc.ok()) else {
                continue;
            };
            s.neoscad_ok += 1;
            s.neoscad_total_s += n;
            if let Some(o) = p
                .openscad
                .as_ref()
                .filter(|o| o.rc.ok())
                .and_then(|o| o.best_s)
                && n > 0.0
                && o > 0.0
            {
                s.compared += 1;
                log_sum += (o / n).ln();
            }
        }
        if s.compared > 0 {
            s.geomean_speedup = Some((log_sum / s.compared as f64).exp());
        }
        s
    }

    /// A one-line description for the submission's title: version, OS,
    /// CPU and reference, e.g. "neoscad 0.1.1 on macOS 15.6 (24G84),
    /// Apple M3 Pro, vs OpenSCAD 2026.09.23 (manifold)".
    pub fn title(&self) -> String {
        let os = self
            .machine
            .os_version
            .clone()
            .unwrap_or_else(|| self.machine.os.clone());
        let cpu = self
            .machine
            .cpu
            .clone()
            .unwrap_or_else(|| self.machine.arch.clone());
        let vs = match &self.openscad {
            Some(r) => format!(", vs {} ({})", r.version, r.backend),
            None => String::new(),
        };
        format!("neoscad {} on {os}, {cpu}{vs}", self.neoscad.version)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::timing::{Rc, RcWord};
    use serde_json::Value;

    fn m(best: Option<f64>, rc: Rc) -> Measurement {
        Measurement {
            rc,
            timed_out: rc == Rc::Word(RcWord::Timeout),
            runs_s: vec![best],
            best_s: best,
            cpu_s: vec![0.1],
        }
    }

    /// A result with every optional part filled in, and one without.
    pub(crate) fn sample(with_openscad: bool) -> BenchResult {
        let reference = with_openscad.then(|| Reference {
            version: "OpenSCAD version 2026.09.23".into(),
            backend: "manifold".into(),
            args: vec!["--backend=manifold".into()],
        });
        let o = |b: f64| with_openscad.then(|| m(Some(b), Rc::Code(0)));
        let mut models = BTreeMap::new();
        models.insert(
            "a".to_string(),
            Pair {
                neoscad: m(Some(0.5), Rc::Code(0)),
                openscad: o(2.0),
            },
        );
        models.insert(
            "b".to_string(),
            Pair {
                neoscad: m(Some(1.0), Rc::Code(0)),
                openscad: o(0.5),
            },
        );
        models.insert(
            "c".to_string(),
            Pair {
                neoscad: m(None, Rc::Word(RcWord::Timeout)),
                openscad: with_openscad.then(|| m(None, Rc::Code(1))),
            },
        );
        BenchResult {
            schema: SCHEMA,
            source: Source::User,
            neoscad: Neoscad {
                version: "0.1.1".into(),
                target: "aarch64-apple-darwin".into(),
                sha256: "ab".repeat(32),
                official: true,
                official_check: Check::Matched,
            },
            kit: KitInfo {
                version: "0.1.1".into(),
                archive_sha256: Some("cd".repeat(32)),
                content_sha256: "ef".repeat(32),
                neoscad_commit: "1".repeat(40),
                bosl2_commit: "2".repeat(40),
                openscad_commit: "3".repeat(40),
            },
            method: Method {
                version: crate::timing::METHOD_VERSION,
                runs: 3,
                cold_start_runs: 20,
                single_run_over_s: 60.0,
                timeout_s: 300.0,
                quick: false,
            },
            machine: Machine {
                os: "macos".into(),
                os_version: Some("macOS 15.6 (24G84)".into()),
                arch: "aarch64".into(),
                cpu: Some("Apple M3 Pro".into()),
                hardware_model: with_openscad.then(|| "Mac15,6".into()),
                cores_logical: 12,
                cores_physical: Some(12),
                cores_performance: Some(6),
                cores_efficiency: Some(6),
                memory_bytes: Some(36 << 30),
                on_battery: Some(false),
                load_before: Some([1.0, 2.0, 3.0]),
                load_after: None,
                translated: Some(false),
            },
            threads: 12,
            openscad: reference,
            cold_start: Pair {
                neoscad: m(Some(0.003), Rc::Code(0)),
                openscad: o(0.05),
            },
            models,
            skipped: BTreeMap::from([("d".to_string(), "missing".to_string())]),
            started_at: "2026-09-30T12:00:00Z".into(),
            finished_at: "2026-09-30T12:30:00Z".into(),
        }
    }

    #[test]
    fn round_trips() {
        for with in [true, false] {
            let r = sample(with);
            let text = serde_json::to_string_pretty(&r).unwrap();
            let back: BenchResult = serde_json::from_str(&text).unwrap();
            assert_eq!(back, r);
        }
    }

    #[test]
    fn unknown_fields_are_refused() {
        let mut v = serde_json::to_value(sample(false)).unwrap();
        v["hostname"] = serde_json::json!("box");
        assert!(serde_json::from_value::<BenchResult>(v).is_err());
    }

    /// The subset of JSON Schema that `bench/result.schema.json` uses:
    /// enough to check that what this crate writes is what the benchmarks
    /// repository will validate against, without a schema crate.
    fn validate(schema: &Value, root: &Value, v: &Value, at: &str, errs: &mut Vec<String>) {
        if let Some(r) = schema.get("$ref").and_then(Value::as_str) {
            let name = r.strip_prefix("#/$defs/").expect("local refs only");
            return validate(&root["$defs"][name], root, v, at, errs);
        }
        if let Some(c) = schema.get("const")
            && c != v
        {
            errs.push(format!("{at}: {v} is not {c}"));
        }
        if let Some(e) = schema.get("enum").and_then(Value::as_array)
            && !e.contains(v)
        {
            errs.push(format!("{at}: {v} not in {e:?}"));
        }
        if let Some(alts) = schema.get("oneOf").and_then(Value::as_array) {
            let ok = alts
                .iter()
                .filter(|a| {
                    let mut e = Vec::new();
                    validate(a, root, v, at, &mut e);
                    e.is_empty()
                })
                .count();
            if ok != 1 {
                errs.push(format!("{at}: matches {ok} of oneOf"));
            }
        }
        if let Some(t) = schema.get("type") {
            let types: Vec<&str> = match t {
                Value::String(s) => vec![s.as_str()],
                Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
                _ => vec![],
            };
            let is = |t: &str| match t {
                "object" => v.is_object(),
                "array" => v.is_array(),
                "string" => v.is_string(),
                "boolean" => v.is_boolean(),
                "null" => v.is_null(),
                "number" => v.is_number(),
                "integer" => v.is_i64() || v.is_u64(),
                _ => false,
            };
            if !types.iter().any(|t| is(t)) {
                errs.push(format!("{at}: {v} is not {types:?}"));
                return;
            }
        }
        if let (Some(p), Some(s)) = (schema.get("pattern").and_then(Value::as_str), v.as_str())
            && !regex::Regex::new(p).unwrap().is_match(s)
        {
            errs.push(format!("{at}: '{s}' does not match {p}"));
        }
        if let (Some(m), Some(n)) = (schema.get("minimum").and_then(Value::as_f64), v.as_f64())
            && n < m
        {
            errs.push(format!("{at}: {n} < {m}"));
        }
        if let Some(a) = v.as_array() {
            if let Some(n) = schema.get("minItems").and_then(Value::as_u64)
                && (a.len() as u64) < n
            {
                errs.push(format!("{at}: fewer than {n} items"));
            }
            if let Some(n) = schema.get("maxItems").and_then(Value::as_u64)
                && (a.len() as u64) > n
            {
                errs.push(format!("{at}: more than {n} items"));
            }
            if let Some(items) = schema.get("items") {
                for (i, x) in a.iter().enumerate() {
                    validate(items, root, x, &format!("{at}[{i}]"), errs);
                }
            }
        }
        if let Some(o) = v.as_object() {
            let props = schema.get("properties").and_then(Value::as_object);
            for r in schema
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let r = r.as_str().unwrap();
                if !o.contains_key(r) {
                    errs.push(format!("{at}: missing {r}"));
                }
            }
            for (k, x) in o {
                let path = format!("{at}.{k}");
                match (
                    props.and_then(|p| p.get(k)),
                    schema.get("additionalProperties"),
                ) {
                    (Some(s), _) => validate(s, root, x, &path, errs),
                    (None, Some(Value::Bool(false))) => errs.push(format!("{path}: not allowed")),
                    (None, Some(extra)) if extra.is_object() => {
                        validate(extra, root, x, &path, errs)
                    }
                    _ => {}
                }
            }
        }
    }

    fn schema() -> Value {
        serde_json::from_str(include_str!("../../../bench/result.schema.json")).unwrap()
    }

    #[test]
    fn schema_file_matches() {
        let schema = schema();
        for with in [true, false] {
            let v = serde_json::to_value(sample(with)).unwrap();
            let mut errs = Vec::new();
            validate(&schema, &schema, &v, "$", &mut errs);
            assert!(errs.is_empty(), "{errs:#?}");
        }
        // The validator does reject what the schema forbids.
        let mut v = serde_json::to_value(sample(false)).unwrap();
        v["machine"]["hostname"] = serde_json::json!("box");
        v["neoscad"]["sha256"] = serde_json::json!("xyz");
        let mut errs = Vec::new();
        validate(&schema, &schema, &v, "$", &mut errs);
        assert_eq!(errs.len(), 2, "{errs:#?}");
    }

    /// Every property the schema's objects declare is one the writer
    /// emits, and the other way round (the validator checks the latter),
    /// so the schema cannot keep a field the code dropped.
    #[test]
    fn schema_declares_only_written_fields() {
        let schema = schema();
        let v = serde_json::to_value(sample(true)).unwrap();
        let check = |schema_obj: &Value, value: &Value, at: &str| {
            let declared: Vec<&String> = schema_obj["properties"]
                .as_object()
                .unwrap()
                .keys()
                .collect();
            for k in declared {
                assert!(
                    value.get(k).is_some(),
                    "{at}.{k} is in the schema but never written"
                );
            }
        };
        check(&schema, &v, "$");
        for part in ["neoscad", "kit", "method", "machine"] {
            check(&schema["properties"][part], &v[part], part);
        }
        check(
            &schema["$defs"]["measurement"],
            &v["cold_start"]["neoscad"],
            "measurement",
        );
        check(&schema["$defs"]["pair"], &v["cold_start"], "pair");
    }

    #[test]
    fn summary_geomean() {
        let s = sample(true).summary();
        assert_eq!((s.models, s.neoscad_ok, s.compared), (3, 2, 2));
        // sqrt(4 * 0.5) = sqrt(2)
        assert!((s.geomean_speedup.unwrap() - 2f64.sqrt()).abs() < 1e-12);
        assert!((s.neoscad_total_s - 1.5).abs() < 1e-12);
        assert_eq!(sample(false).summary().geomean_speedup, None);
    }
}
