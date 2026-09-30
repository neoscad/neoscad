//! `conformance depth`: the recursion-depth guard.
//!
//! neoscad stops a runaway recursion when the evaluation has used
//! `eval::DEFAULT_STACK_LIMIT` of stack, so how deep a program can recurse
//! depends on how large the evaluator's frames are, and those move with
//! the target, the compiler and above all the build: a profile-guided
//! build (`scripts/pgo.sh`) inlines more into the recursive evaluator and
//! reached a third less module recursion than the plain release build.
//! The conformance suite cannot see that, because OpenSCAD's expected
//! outputs cut the trace to `*** Excluding 1 frames ***`. This runs any
//! built binary on the recursions and fails unless each goes at least
//! [`MARGIN`] times as deep as OpenSCAD does, so a release build that
//! would reject programs OpenSCAD runs is caught before it ships.
//!
//! Two kinds of check:
//!
//! - OpenSCAD's own recursion tests, run to their recursion error: the
//!   depth is the `N` of the trace's `*** Excluding N frames ***` line
//!   (trace lines left out of the middle; both programs print the same
//!   head and tail, so `N` compares directly).
//! - The two plain recursions of `eval/src/recursion.rs`'s table, run at
//!   the depth the margin asks for, which must evaluate without an error.
//!
//! `issue4172` is reported but not gated: printing a nested vector is
//! capped at OpenSCAD's own 8 MiB of stack (`eval/src/print.rs`,
//! `PRINT_STACK_LIMIT`), which neoscad's larger module frames share, so it
//! prints fewer levels than OpenSCAD by design.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::ctx::Ctx;

/// The OpenSCAD the reference depths were measured with (`--version` and
/// `--info`): its recursion limit is `StackCheck`'s 8 MiB less 128 KiB, so
/// its depths are properties of that build's frames, not of the language.
pub const REFERENCE: &str = "OpenSCAD nightly 2026.09.23 (git 28fe66bc), macOS arm64, Clang 17";

/// How much deeper than [`REFERENCE`] every check must go. Frame sizes
/// differ between targets, compilers and profiles by tens of percent (the
/// PGO build's module frames are half as large again as the plain
/// build's), and the nightly was measured on one platform only, so a
/// build that merely matches it is too close to rejecting a program some
/// OpenSCAD build runs.
pub const MARGIN: f64 = 1.25;

/// One check and OpenSCAD's depth on it.
struct Check {
    id: &'static str,
    kind: Kind,
    /// [`REFERENCE`]'s depth.
    openscad: u32,
}

enum Kind {
    /// A file under the reference's `tests/data/scad`, whose trace's
    /// excluded-frame count is the depth.
    Reference(&'static str),
    /// A program with `{N}` for the depth, which must evaluate cleanly at
    /// the depth the margin asks for; its real depth is then bisected.
    Program(&'static str),
    /// A reference file whose `ECHO` lines count how many levels printed;
    /// reported, not gated.
    Echoes(&'static str),
}

const CHECKS: &[Check] = &[
    Check {
        id: "recursion-test-module",
        kind: Kind::Reference("misc/recursion-test-module.scad"),
        openscad: 30_261,
    },
    Check {
        id: "recursion-test-vector",
        kind: Kind::Reference("misc/recursion-test-vector.scad"),
        openscad: 30_261,
    },
    Check {
        id: "recursion-test-function3",
        kind: Kind::Reference("misc/recursion-test-function3.scad"),
        openscad: 9_170,
    },
    // The deepest n that evaluates without an error, bisected.
    Check {
        id: "module-if",
        kind: Kind::Program("module m(n) { if (n > 0) m(n - 1); else cube(1); }\nm({N});\n"),
        openscad: 7_052,
    },
    Check {
        id: "function-add",
        kind: Kind::Program("function f(n) = n == 0 ? 0 : 1 + f(n - 1);\necho(f({N}));\n"),
        openscad: 9_192,
    },
    Check {
        id: "issue4172-echo-vector-stack-exhaust",
        kind: Kind::Echoes("issues/issue4172-echo-vector-stack-exhaust.scad"),
        openscad: 434,
    },
];

pub struct DepthOptions {
    pub binary: Option<PathBuf>,
    pub timeout: Duration,
    /// Write the results here as JSON too.
    pub json: Option<PathBuf>,
}

/// The depth [`MARGIN`] asks for over OpenSCAD's `d`.
fn required(d: u32) -> u32 {
    (f64::from(d) * MARGIN).ceil() as u32
}

/// `N` of the first `*** Excluding N frames ***` line.
fn excluded_frames(text: &str) -> Option<u32> {
    text.lines().find_map(|l| {
        let rest = l.split("*** Excluding ").nth(1)?;
        rest.split(' ').next()?.parse().ok()
    })
}

pub fn depth(ctx: &Ctx, opts: &DepthOptions) -> Result<u8, String> {
    let binary = opts.binary.clone().unwrap_or_else(|| ctx.default_binary());
    let binary = binary
        .canonicalize()
        .map_err(|e| format!("{}: {e}", binary.display()))?;
    let version = version(&binary);
    let is_neoscad = version.starts_with("neoscad");
    let work = ctx.repo.join("target/conformance/depth");
    fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    let scad = ctx.ref_root.join("tests/data/scad");

    println!("binary: {} ({version})", binary.display());
    println!("reference: {REFERENCE}; required: {MARGIN}x its depth");
    let mut failed = 0;
    let mut rows = Vec::new();
    let run = Runner {
        binary: &binary,
        is_neoscad,
        work: &work,
        timeout: opts.timeout,
    };
    for c in CHECKS {
        let (value, pass, note): (Option<u32>, Option<bool>, String) = match c.kind {
            Kind::Reference(p) => match run.once(c.id, &scad.join(p)) {
                Err(why) => (None, Some(false), why),
                Ok((_, text)) => match excluded_frames(&text) {
                    Some(n) => (Some(n), Some(n >= required(c.openscad)), String::new()),
                    None => (
                        None,
                        Some(false),
                        "no `*** Excluding N frames ***` line".into(),
                    ),
                },
            },
            Kind::Program(src) => {
                let need = required(c.openscad);
                match run.program(c.id, src, need)? {
                    Err(why) => (None, Some(false), why),
                    Ok(()) => {
                        // How deep it really goes, for the record: bisect
                        // above the requirement, capped at 32 times
                        // OpenSCAD's depth (about 20 short runs).
                        let (mut lo, mut hi) = (need, c.openscad.saturating_mul(32));
                        while hi - lo > 1 {
                            let mid = lo + (hi - lo) / 2;
                            if run.program(c.id, src, mid)?.is_ok() {
                                lo = mid;
                            } else {
                                hi = mid;
                            }
                        }
                        (Some(lo), Some(true), String::new())
                    }
                }
            }
            Kind::Echoes(p) => match run.once(c.id, &scad.join(p)) {
                Err(why) => (None, None, why),
                Ok((_, text)) => {
                    let n = text.lines().filter(|l| l.starts_with("ECHO:")).count() as u32;
                    (
                        Some(n),
                        None,
                        "not gated: printing shares OpenSCAD's 8 MiB".into(),
                    )
                }
            },
        };
        let verdict = match pass {
            Some(true) => "ok",
            Some(false) => {
                failed += 1;
                "FAIL"
            }
            None => "info",
        };
        let got = value.map_or_else(|| "-".into(), |n| n.to_string());
        let ratio = value.map(|v| f64::from(v) / f64::from(c.openscad));
        println!(
            "{verdict:4} {:38} {got:>7}  openscad {:>6}  need {:>6}{}{}",
            c.id,
            c.openscad,
            if pass.is_some() {
                required(c.openscad).to_string()
            } else {
                "-".into()
            },
            ratio.map(|r| format!("  ({r:.2}x)")).unwrap_or_default(),
            if note.is_empty() {
                String::new()
            } else {
                format!("  {note}")
            }
        );
        rows.push(serde_json::json!({
            "id": c.id,
            "depth": value,
            "openscad": c.openscad,
            "required": required(c.openscad),
            "ratio": ratio.map(|r| (r * 1000.0).round() / 1000.0),
            "pass": pass,
            "note": note,
        }));
    }
    if let Some(path) = &opts.json {
        let doc = serde_json::json!({
            "binary": binary.display().to_string(),
            "version": version,
            "reference": REFERENCE,
            "margin": MARGIN,
            "checks": rows,
        });
        let text = serde_json::to_string_pretty(&doc).map_err(|e| e.to_string())?;
        fs::write(path, text + "\n").map_err(|e| format!("{}: {e}", path.display()))?;
    }
    if failed > 0 {
        println!("{failed} check(s) below {MARGIN}x OpenSCAD's depth");
    }
    Ok(u8::from(failed > 0))
}

/// Runs the binary under test on one input.
struct Runner<'a> {
    binary: &'a Path,
    is_neoscad: bool,
    work: &'a Path,
    timeout: Duration,
}

impl Runner<'_> {
    /// Export `input` to `.echo`: the exit code and the output's text, or
    /// why it did not finish (a timeout, a failed spawn, no output).
    fn once(&self, id: &str, input: &Path) -> Result<(Option<i32>, String), String> {
        let out = self.work.join(format!("{id}.echo"));
        let _ = fs::remove_file(&out);
        let mut cmd = Command::new(self.binary);
        cmd.current_dir(self.work)
            .env(crate::geometry::NO_SERVER_VAR, "1")
            .stdin(Stdio::null())
            .stdout(Stdio::null());
        // Bounded like every other run: neoscad takes resource limits,
        // which the deepest of these stay far inside (about 80 MB);
        // OpenSCAD, run to check the harness, has none to take.
        if self.is_neoscad {
            cmd.args(["--limit", "memory=2G", "--limit", "time=60"]);
        }
        cmd.arg("-o").arg(&out).arg(input);
        let stderr_path = self.work.join(format!("{id}.stderr"));
        let code = crate::geometry::exec_status(&mut cmd, self.timeout, &stderr_path)?;
        let text = fs::read_to_string(&out).unwrap_or_default();
        if text.is_empty() && code != Some(0) {
            return Err(format!(
                "exit {code:?}, no output: {}",
                first_error("", &stderr_path)
            ));
        }
        Ok((code, text))
    }

    /// Run `src` with `{N}` replaced by `n`: `Ok(Ok(()))` when it
    /// evaluates without an error, `Ok(Err(why))` when it does not.
    fn program(&self, id: &str, src: &str, n: u32) -> Result<Result<(), String>, String> {
        let p = self.work.join(format!("{id}.scad"));
        fs::write(&p, src.replace("{N}", &n.to_string()))
            .map_err(|e| format!("{}: {e}", p.display()))?;
        Ok(match self.once(id, &p) {
            Err(why) => Err(why),
            Ok((Some(0), text)) if !text.contains("ERROR") => Ok(()),
            Ok((code, text)) => Err(format!(
                "at {n}: exit {code:?}: {}",
                first_error(&text, &self.work.join(format!("{id}.stderr")))
            )),
        })
    }
}

/// The binary's `--version` line (neoscad prints it on stdout, OpenSCAD
/// on stderr).
fn version(binary: &Path) -> String {
    let Ok(out) = Command::new(binary)
        .arg("--version")
        .stdin(Stdio::null())
        .output()
    else {
        return "unknown".into();
    };
    let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.push_str(&String::from_utf8_lossy(&out.stderr));
    text.lines().next().unwrap_or("unknown").trim().to_string()
}

/// The first `ERROR` line of the output, else of stderr, for a failure note.
fn first_error(text: &str, stderr_path: &Path) -> String {
    let mut err = String::new();
    if let Ok(mut f) = fs::File::open(stderr_path) {
        let _ = f.read_to_string(&mut err);
    }
    text.lines()
        .chain(err.lines())
        .find(|l| l.contains("ERROR"))
        .unwrap_or("no error printed")
        .chars()
        .take(160)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_excluded_frame_count() {
        let text = "ERROR: Recursion detected calling module 'crash'\n\
                    TRACE: called by 'crash' in file a.scad, line 2\n\
                    TRACE:   *** Excluding 30261 frames ***\n";
        assert_eq!(excluded_frames(text), Some(30_261));
        assert_eq!(excluded_frames("ECHO: 1\n"), None);
    }

    #[test]
    fn the_margin_rounds_up() {
        assert_eq!(required(30_261), 37_827);
        assert_eq!(required(4), 5);
    }
}
