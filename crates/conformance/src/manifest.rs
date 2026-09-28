//! The committed test manifest (`conformance/manifest.json`): every test
//! OpenSCAD's `tests/CMakeLists.txt` registers, mapped to a NeoSCAD tier and
//! marked runnable, pending or skipped.
//!
//! Tiers follow docs/architecture.md ("Validation") and the phase-0 audit's
//! test map (docs/audits/phase0.md, B2):
//!
//! | Tier | Name     | What                                              |
//! |------|----------|---------------------------------------------------|
//! | 0    | Parse    | `.ast` output: astdump, customizer                |
//! | 1    | Evaluate | `.echo` output: echo and its variants             |
//! | 2    | Tree     | `.csg`/`.term` output: dump, csgterm              |
//! | 3    | Geometry | exported files and `--render` images              |
//! | 4    | Image    | preview/throwntogether, camera, view, colours     |
//! | 5    | Other    | exit-code-only tests and harness self-tests       |

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::cmake::{Evaluation, RegKind, Registration};

pub const SCHEMA: u32 = 1;

pub const TIER_NAMES: [&str; 6] = ["Parse", "Evaluate", "Tree", "Geometry", "Image", "Other"];

/// What the runner does with a case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Runner {
    /// Run neoscad and compare text output as `test_cmdline_tool.py` does.
    Text,
    /// Tier 3 geometry test (see `geometry.rs`): neoscad exports the
    /// model's mesh, the pinned OpenSCAD nightly renders it with the test's
    /// arguments, and the PNG is compared as `image_compare.py` does; or,
    /// for `stlexportsanitytest.py`, the exported STL is validated.
    Geometry,
    /// A test OpenSCAD drives with one of its Python scripts or a raw
    /// command, ported natively in `script.rs` and needing no renderer:
    /// SVG re-export (`export_import_pngtest.py` with an `.svg` result),
    /// PDF export (`export_pngtest.py`, the PDF rasterised and compared as
    /// an image), exit codes (`shouldfail.py`) and the relative-output
    /// commands.
    Script,
    /// Tier 4 image drawn by neoscad's own renderer (`-o x.png` with
    /// `--render`), compared with the expected PNG under tier 4's rule:
    /// OpenSCAD's `image_compare` or the perceptual score (see `run.rs`).
    Image,
    /// In scope, but the comparison is not implemented yet (tiers 3-5);
    /// `pending_reason` says what is missing.
    Pending,
    /// Out of scope; `skip_reason` says why.
    Skip,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Case {
    /// ctest name: `<group>_<input basename>`.
    pub id: String,
    pub tier: u8,
    pub runner: Runner,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    /// For a pending case: the missing piece that keeps it from running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_reason: Option<String>,
    /// Registration group (`add_cmdline_test` basename).
    pub group: String,
    /// Input path relative to the reference checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    /// Expected output relative to the reference checkout.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    pub suffix: String,
    /// Arguments after the input file. `{REF}` stands for the reference
    /// checkout, `{OPENSCAD}` for the binary under test.
    pub args: Vec<String>,
    /// Input on stdin (`-`) and output on stdout (`-o -`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stdio: bool,
    /// Python driver for script-based tests (not run by us).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script: Option<String>,
    /// Full command for raw `add_test` registrations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub command: Vec<String>,
    /// ctest configurations (Default, Examples, Bugs, Heavy, ...).
    pub configs: Vec<String>,
    /// `OPENSCAD_TEST_EXCLUDE_LINE` regex for this test, when it differs
    /// from the manifest's `default_exclude_line` (`""` means none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_line: Option<String>,
    /// Registration line in tests/CMakeLists.txt.
    pub cmake_line: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TierCounts {
    pub total: usize,
    pub text: usize,
    #[serde(default)]
    pub geometry: usize,
    #[serde(default)]
    pub script: usize,
    #[serde(default)]
    pub image: usize,
    pub pending: usize,
    pub skip: usize,
    /// Runnable (text or geometry) cases whose expected file does not exist in the checkout.
    #[serde(default)]
    pub missing_expected: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reference {
    /// Relative to the NeoSCAD repository root.
    pub path: String,
    pub commit: String,
}

/// A file CMake's configure step would create before tests run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Generated {
    /// Output path relative to the reference checkout.
    pub output: String,
    /// `configure_file` template relative to the reference checkout, or
    /// absent for files produced by a generator script.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// `configure_file(... COPYONLY)`: copy without substituting variables.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub copy_only: bool,
    /// Name of a natively ported generator script (e.g. `gen_issue2342`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub schema: u32,
    pub generated_by: String,
    pub reference: Reference,
    pub tier_names: Vec<String>,
    pub counts: BTreeMap<String, TierCounts>,
    pub skip_reasons: BTreeMap<String, usize>,
    pub generated_files: Vec<Generated>,
    /// `OPENSCAD_TEST_EXCLUDE_LINE` shared by (almost) every test, stored
    /// once to keep the file small; see `Case::exclude_line`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_exclude_line: Option<String>,
    /// Warnings from evaluating the CMake file (e.g. missing MCAD).
    pub diagnostics: Vec<String>,
    pub tests: Vec<Case>,
}

impl Manifest {
    /// Serialise with one compact line per test: the file stays small and a
    /// change to one test is a one-line diff.
    pub fn to_text(&self) -> Result<String, String> {
        let mut head = self.clone();
        head.tests.clear();
        let head = serde_json::to_string_pretty(&head).map_err(|e| e.to_string())?;
        let stub = "\"tests\": []\n}";
        let body = head
            .strip_suffix(stub)
            .ok_or("unexpected manifest layout")?;
        let mut out = String::with_capacity(self.tests.len() * 400);
        out.push_str(body);
        out.push_str("\"tests\": [\n");
        for (i, t) in self.tests.iter().enumerate() {
            out.push_str("    ");
            out.push_str(&serde_json::to_string(t).map_err(|e| e.to_string())?);
            out.push_str(if i + 1 < self.tests.len() {
                ",\n"
            } else {
                "\n"
            });
        }
        out.push_str("  ]\n}\n");
        Ok(out)
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| format!("{}: {e} (run `conformance manifest` first)", path.display()))?;
        serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
    }
}

/// Build the manifest from an evaluated CMakeLists.
///
/// `ref_root` is the absolute reference checkout; `binpath` is the value
/// the CMake file computed for OPENSCAD_BINPATH, rewritten to `{OPENSCAD}`.
pub fn build(eval: &Evaluation, ref_root: &str, ref_rel: &str, commit: &str) -> Manifest {
    let binpath = eval
        .vars
        .get("OPENSCAD_BINPATH")
        .cloned()
        .unwrap_or_default();
    let rel = |p: &str| -> String {
        p.strip_prefix(ref_root)
            .map(|s| s.trim_start_matches('/').to_string())
            .unwrap_or_else(|| p.to_string())
    };
    let placeholder = |a: &str| -> String {
        let a = if binpath.is_empty() {
            a.to_string()
        } else {
            a.replace(&binpath, "{OPENSCAD}")
        };
        a.replace(ref_root, "{REF}")
    };

    let default_exclude_line = eval
        .registrations
        .first()
        .and_then(|r| r.exclude_line.clone());
    let mut tests = Vec::new();
    for r in &eval.registrations {
        let tier = tier_of(r);
        let skip_reason = skip_reason(r);
        let runner = if skip_reason.is_some() {
            Runner::Skip
        } else if is_text(r, tier) {
            Runner::Text
        } else if is_geometry(r, tier) {
            Runner::Geometry
        } else if is_image(r, tier) {
            Runner::Image
        } else if is_script(r, &binpath) {
            Runner::Script
        } else {
            Runner::Pending
        };
        let pending_reason = (runner == Runner::Pending).then(|| pending_reason(r, tier));
        let expected = (r.kind == RegKind::Cmdline).then(|| {
            let dir = r.expected_dir.as_deref().unwrap_or(&r.group);
            format!(
                "tests/regression/{dir}/{}-expected.{}",
                r.basename, r.suffix
            )
        });
        tests.push(Case {
            id: r.name.clone(),
            tier,
            runner,
            skip_reason,
            pending_reason,
            group: r.group.clone(),
            input: r.file.as_deref().map(rel),
            expected,
            suffix: r.suffix.clone(),
            args: r.args.iter().map(|a| placeholder(a)).collect(),
            stdio: r.stdio,
            script: r.script.as_deref().map(rel),
            command: r.command.iter().map(|a| placeholder(a)).collect(),
            configs: r.configs.clone(),
            exclude_line: (r.exclude_line != default_exclude_line)
                .then(|| r.exclude_line.clone().unwrap_or_default()),
            cmake_line: r.line,
        });
    }

    let mut counts: BTreeMap<String, TierCounts> = BTreeMap::new();
    let mut skip_reasons: BTreeMap<String, usize> = BTreeMap::new();
    for t in &tests {
        let c = counts.entry(t.tier.to_string()).or_default();
        c.total += 1;
        match t.runner {
            Runner::Text | Runner::Geometry => {
                if t.runner == Runner::Text {
                    c.text += 1;
                } else {
                    c.geometry += 1;
                }
                let exists = t
                    .expected
                    .as_deref()
                    .is_some_and(|e| Path::new(ref_root).join(e).exists());
                if !exists {
                    c.missing_expected += 1;
                }
            }
            Runner::Script => {
                c.script += 1;
                // Only the SVG re-export and PDF cases compare with a file.
                let missing = t
                    .expected
                    .as_deref()
                    .is_some_and(|e| !Path::new(ref_root).join(e).exists());
                if missing {
                    c.missing_expected += 1;
                }
            }
            Runner::Image => {
                c.image += 1;
                let exists = t
                    .expected
                    .as_deref()
                    .is_some_and(|e| Path::new(ref_root).join(e).exists());
                if !exists {
                    c.missing_expected += 1;
                }
            }
            Runner::Pending => c.pending += 1,
            Runner::Skip => {
                c.skip += 1;
                *skip_reasons
                    .entry(t.skip_reason.clone().unwrap_or_default())
                    .or_default() += 1;
            }
        }
    }

    // Inputs CMake creates at configure/build time: the configured
    // templates, the issue2342 stress file, and the SVG viewbox files the
    // tier 4 `svgviewbox-*` images import.
    let mut generated_files: Vec<Generated> = eval
        .configured_files
        .iter()
        .filter(|(_, out, _)| out.ends_with(".scad"))
        .map(|(tpl, out, copy_only)| Generated {
            output: rel(out),
            template: Some(rel(tpl)),
            copy_only: *copy_only,
            generator: None,
        })
        .collect();
    let needs_issue2342 = tests.iter().any(|t| {
        t.input
            .as_deref()
            .is_some_and(|i| i.ends_with("/issues/issue2342.scad"))
    });
    if needs_issue2342 {
        let out = tests
            .iter()
            .find_map(|t| {
                t.input
                    .clone()
                    .filter(|i| i.ends_with("/issues/issue2342.scad"))
            })
            .expect("checked above");
        generated_files.push(Generated {
            output: out,
            template: None,
            copy_only: false,
            generator: Some("gen_issue2342".into()),
        });
    }

    // `gen_svg_viewbox_tests.py` writes one SVG per viewbox test from one
    // template; each test names its file in a `-Dfile=...` argument.
    let mut viewboxes: Vec<String> = tests
        .iter()
        .flat_map(|t| t.args.iter())
        .filter_map(|a| {
            let i = a.find("/build/tests/data/svg/viewbox/")?;
            let end = a[i..].find(".svg")? + i + 4;
            Some(a[i + 1..end].to_string())
        })
        .collect();
    viewboxes.sort();
    viewboxes.dedup();
    for out in viewboxes {
        generated_files.push(Generated {
            output: out,
            template: Some("tests/data/svg/viewbox/viewbox-tests.svg.in".into()),
            copy_only: false,
            generator: Some("gen_svg_viewbox".into()),
        });
    }

    Manifest {
        schema: SCHEMA,
        generated_by: "cargo run --release -p neoscad-conformance -- manifest".into(),
        reference: Reference {
            path: ref_rel.into(),
            commit: commit.into(),
        },
        tier_names: TIER_NAMES.iter().map(|s| s.to_string()).collect(),
        counts,
        skip_reasons,
        generated_files,
        default_exclude_line,
        diagnostics: eval
            .diagnostics
            .iter()
            .map(|d| d.replace(ref_root, "{REF}"))
            .collect(),
        tests,
    }
}

/// A test compared as text, as `test_cmdline_tool.py` does: the tiers 0-2
/// outputs, the `.json` of `export-param` (tier 3, compared as parsed
/// JSON like `compare_json`), and exact mesh files (the `export-*` tests
/// of `predictible-output`, whose sorted STL/OBJ/3MF/POV output is
/// compared line for line; a 3MF first has its model XML extracted as
/// `post_process_3mf` does).
fn is_text(r: &Registration, tier: u8) -> bool {
    r.kind == RegKind::Cmdline
        && r.openscad
        && r.script.is_none()
        && (tier <= 2
            || (r.suffix == "json" && !r.stdio)
            || (tier == 3 && EXACT_MESH_SUFFIXES.contains(&r.suffix.as_str())))
}

/// Mesh formats a tier 3 test can compare as exact files.
const EXACT_MESH_SUFFIXES: &[&str] = &["stl", "obj", "3mf", "pov", "off", "wrl"];

/// A test the script runner ports (see [`Runner::Script`]). A raw command
/// is supported when it runs the binary under test (`{OPENSCAD}`) or
/// `cmake -E cat`, which is all the relative-output tests use.
fn is_script(r: &Registration, binpath: &str) -> bool {
    match r.kind {
        RegKind::Failing => r
            .script
            .as_deref()
            .is_some_and(|s| s.ends_with("/shouldfail.py")),
        RegKind::Raw => match r.command.first().map(String::as_str) {
            Some("cmake") => r.command.get(1).is_some_and(|a| a == "-E"),
            Some(c) => !binpath.is_empty() && c == binpath,
            None => false,
        },
        RegKind::Cmdline => match r.script.as_deref() {
            Some(s) if s.ends_with("/export_import_pngtest.py") => r.suffix != "png" && !r.stdio,
            Some(s) => s.ends_with("/export_pngtest.py"),
            None => false,
        },
    }
}

/// A tier 3 test the geometry runner handles: a PNG from a direct
/// `--render` of the input (the runner substitutes a mesh export and has the
/// nightly render it) or from `export_import_pngtest.py`, whose export step
/// is exactly what neoscad is being tested on; and
/// `stlexportsanitytest.py`, which validates an exported STL.
/// PDF, SVG re-export and `export-param` belong to other runners
/// ([`is_script`], [`is_text`]).
fn is_geometry(r: &Registration, tier: u8) -> bool {
    if tier != 3 || r.kind != RegKind::Cmdline || r.stdio {
        return false;
    }
    match &r.script {
        None => r.openscad && r.suffix == "png",
        Some(s) if s.ends_with("/export_import_pngtest.py") => r.suffix == "png",
        Some(s) => s.ends_with("/stlexportsanitytest.py"),
    }
}

/// A tier 4 image neoscad's renderer draws: a direct PNG of the input,
/// in render mode (`--render`) or as a preview (the OpenCSG preview, or
/// `--preview=throwntogether`), with any `--view` options, camera, image
/// size, projection or colour scheme.
fn is_image(r: &Registration, tier: u8) -> bool {
    tier == 4
        && r.kind == RegKind::Cmdline
        && r.openscad
        && r.script.is_none()
        && !r.stdio
        && r.suffix == "png"
}

/// `--view` or `--view=...`, but not `--viewall`.
pub fn is_view_option(arg: &str) -> bool {
    arg == "--view" || arg.starts_with("--view=")
}

/// What a pending case waits for.
fn pending_reason(_r: &Registration, _tier: u8) -> String {
    "no runner for this test yet".into()
}

fn has_arg(r: &Registration, pred: impl Fn(&str) -> bool) -> bool {
    r.test_args.iter().any(|a| pred(a))
}

/// Assign a tier. Text outputs map directly by format; image tests split
/// into geometry (a `--render` of the model, or an export/re-import) versus
/// renderer behaviour (preview modes, cameras, view options, colour
/// schemes), per the architecture's tier 3/4 definitions.
fn tier_of(r: &Registration) -> u8 {
    if r.kind == RegKind::Raw && r.suffix == "png" {
        // `relative-output_png_*` writes a PNG with the binary under test:
        // image rendering by neoscad's own renderer, which is tier 4.
        return 4;
    }
    if r.kind != RegKind::Cmdline {
        return 5;
    }
    if r.script.is_some() {
        return 3; // export -> import -> image, or an exported file
    }
    match r.suffix.as_str() {
        "ast" => 0,
        "echo" => 1,
        "csg" | "term" => 2,
        "png" => {
            let renders = has_arg(r, |a| a.starts_with("--render"));
            let renderer_specific = has_arg(r, |a| {
                [
                    "--camera",
                    "--view",
                    "--colorscheme",
                    "--imgsize",
                    "--projection",
                    "--preview",
                ]
                .iter()
                .any(|p| a.starts_with(p))
            });
            if renders && !renderer_specific { 3 } else { 4 }
        }
        _ => 3,
    }
}

/// OpenSCAD's experimental features neoscad implements (its `--enable`
/// accepts them as OpenSCAD does, `eval::Feature::supported`). Their
/// cases run like any other, under the rules below (a case runs when
/// every feature it enables is here). The harness keeps its
/// own list rather than asking the evaluator, so that pointing it at the
/// nightly (`--binary`) checks the same cases.
const SUPPORTED_FEATURES: &[&str] = &[
    "textmetrics",
    "object-function",
    "import-function",
    "predictible-output",
    "vector-swizzle",
];

/// Why a registration is out of scope, if it is. The first matching rule
/// wins, so each skipped test is counted under one reason.
fn skip_reason(r: &Registration) -> Option<String> {
    if r.experimental {
        // The first feature neoscad lacks names the reason. A test whose
        // features are all supported runs, and so do the two registered
        // EXPERIMENTAL with no `--enable` at all (offcolorpngtest,
        // 3mfcolorpngtest: colour OFF/3MF export and re-import), which need
        // nothing experimental from the binary.
        let unsupported = r.test_args.iter().enumerate().find_map(|(i, a)| {
            a.strip_prefix("--enable=")
                .map(String::from)
                .or_else(|| {
                    (a == "--enable")
                        .then(|| r.test_args.get(i + 1).cloned())
                        .flatten()
                })
                .filter(|f| !SUPPORTED_FEATURES.contains(&f.as_str()))
        });
        if let Some(f) = unsupported {
            return Some(format!("experimental feature ({f})"));
        }
    }
    let cgal_args = r.test_args.iter().any(|a| a == "--backend=cgal");
    if r.group.contains("cgal") || cgal_args {
        return Some("CGAL backend only".into());
    }
    if r.kind == RegKind::Raw && matches!(r.suffix.as_str(), "nef3" | "nefdbg") {
        return Some("CGAL-only export format".into());
    }
    if r.disabled_at.is_some() {
        return Some("disabled upstream".into());
    }
    if r.configs.iter().any(|c| c == "Bugs") {
        return Some("known upstream bug (Bugs config)".into());
    }
    if r.kind == RegKind::Raw
        && r.command
            .iter()
            .any(|c| c.ends_with("test_pretty_print_logfile.py"))
    {
        return Some("tests OpenSCAD's own harness".into());
    }
    None
}
