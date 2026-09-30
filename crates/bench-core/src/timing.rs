//! How one command is timed: spawned, polled and measured best-of-N.
//!
//! `conformance bench` and `neoscad bench` both time through here, so a
//! community result and the repository's own benchmark series are measured
//! the same way and [`METHOD_VERSION`] names that way for both.

use std::ffi::OsStr;
use std::fs::File;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// The version of the timing method: how a run is spawned, polled and
/// timed ([`time_run`], [`measure`]), the environment it gets, and (in
/// `conformance bench`) how `eval_only` decides a pass. Results carry it
/// (the conformance cache keys, and every community result's
/// `method.version`), so bumping it keeps results measured a different
/// way from being compared with new ones as if they were alike. Bump it
/// with any change to that code.
pub const METHOD_VERSION: u32 = 1;

/// Environment variables removed from every timed run: a font directory
/// set in the user's shell would change what `text()` loads, and so the
/// time, in a way the result could not show.
pub const UNSET_ENV: [&str; 2] = ["NEOSCAD_FONT_DIR", "OPENSCAD_FONT_PATH"];

/// One process run.
#[derive(Debug, Clone, Copy)]
pub struct Run {
    /// Exit code; `None` for a timeout (or death by signal).
    pub code: Option<i32>,
    pub timed_out: bool,
    pub wall_s: f64,
    pub cpu_s: f64,
}

/// User plus system time of every waited-for child so far (0 where the OS
/// does not say, which is Windows).
fn children_cpu_s() -> f64 {
    #[cfg(unix)]
    {
        use nix::sys::resource::{UsageWho, getrusage};
        if let Ok(u) = getrusage(UsageWho::RUSAGE_CHILDREN) {
            let t = |tv: nix::sys::time::TimeVal| tv.tv_sec() as f64 + tv.tv_usec() as f64 / 1e6;
            return t(u.user_time()) + t(u.system_time());
        }
    }
    0.0
}

/// Run `cmd` in `cwd` with `env` set and [`UNSET_ENV`] removed, timing
/// it. The child is polled with short sleeps (at most 1 ms, far less for
/// short runs) so the measured wall time is within a small fraction of the
/// process's; its stdout is discarded and its stderr goes to `stderr_to`
/// for diagnosis.
pub fn time_run(
    cmd: &[String],
    cwd: &Path,
    env: &[(&str, &OsStr)],
    timeout: Duration,
    stderr_to: &Path,
) -> Result<Run, String> {
    let err = File::create(stderr_to).map_err(|e| format!("{}: {e}", stderr_to.display()))?;
    let mut c = Command::new(&cmd[0]);
    c.args(&cmd[1..])
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(err);
    for k in UNSET_ENV {
        c.env_remove(k);
    }
    for (k, v) in env {
        c.env(k, v);
    }
    let cpu0 = children_cpu_s();
    let start = Instant::now();
    let mut child = c.spawn().map_err(|e| format!("{}: {e}", cmd[0]))?;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let wall_s = start.elapsed().as_secs_f64();
                return Ok(Run {
                    code: status.code(),
                    timed_out: false,
                    wall_s,
                    cpu_s: children_cpu_s() - cpu0,
                });
            }
            Ok(None) => {
                let e = start.elapsed();
                if e > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(Run {
                        code: None,
                        timed_out: true,
                        wall_s: e.as_secs_f64(),
                        cpu_s: children_cpu_s() - cpu0,
                    });
                }
                // Poll at about 1% of the elapsed time, 50 us to 1 ms.
                let nap = (e / 100).clamp(Duration::from_micros(50), Duration::from_millis(1));
                std::thread::sleep(nap);
            }
            Err(e) => return Err(e.to_string()),
        }
    }
}

/// How the last run of a measurement ended: an exit code, or a word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Rc {
    Code(i32),
    Word(RcWord),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RcWord {
    /// Killed at the timeout.
    Timeout,
    /// Ended by a signal (a crash, or killed from outside).
    Signal,
}

impl Rc {
    pub fn ok(self) -> bool {
        self == Rc::Code(0)
    }
}

/// Best-of-N timing of one command.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Measurement {
    /// The last run's outcome; anything but 0 ends the series.
    pub rc: Rc,
    pub timed_out: bool,
    /// Every run's wall time in seconds (4 places); `null` for a run that
    /// timed out.
    pub runs_s: Vec<Option<f64>>,
    /// The best wall time, when every run succeeded.
    pub best_s: Option<f64>,
    /// Every completed run's CPU time (user + system, 3 places); 0 on
    /// Windows, where it is not measured.
    pub cpu_s: Vec<f64>,
}

/// The audit's method: up to `runs` runs one after another, keeping the
/// best wall time; a single run once one takes longer than `single_over`
/// seconds, and none after a failure or a timeout. A failure makes
/// `best_s` null (a failed model has no time); a timeout after a completed
/// run keeps that run's time as `best_s`, as `conformance bench` always has,
/// with `rc` "timeout" saying the series was cut short.
pub fn measure(
    cmd: &[String],
    cwd: &Path,
    env: &[(&str, &OsStr)],
    runs: u32,
    single_over: f64,
    timeout: Duration,
    stderr_to: &Path,
) -> Result<Measurement, String> {
    let mut m = Measurement {
        rc: Rc::Code(0),
        timed_out: false,
        runs_s: Vec::new(),
        best_s: None,
        cpu_s: Vec::new(),
    };
    let mut best: Option<f64> = None;
    for _ in 0..runs.max(1) {
        let r = time_run(cmd, cwd, env, timeout, stderr_to)?;
        if r.timed_out {
            m.rc = Rc::Word(RcWord::Timeout);
            m.timed_out = true;
            m.runs_s.push(None);
            break;
        }
        m.runs_s.push(Some(round(r.wall_s, 4)));
        m.cpu_s.push(round(r.cpu_s, 3));
        match r.code {
            Some(0) => {
                m.rc = Rc::Code(0);
                best = Some(best.map_or(r.wall_s, |b: f64| b.min(r.wall_s)));
            }
            Some(c) => {
                m.rc = Rc::Code(c);
                best = None;
                break;
            }
            None => {
                m.rc = Rc::Word(RcWord::Signal);
                best = None;
                break;
            }
        }
        if r.wall_s > single_over {
            break;
        }
    }
    m.best_s = best.map(|b| round(b, 4));
    Ok(m)
}

/// `x` rounded to `places` decimal places.
pub fn round(x: f64, places: i32) -> f64 {
    let f = 10f64.powi(places);
    (x * f).round() / f
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rc_serializes_as_a_code_or_a_word() {
        assert_eq!(serde_json::to_string(&Rc::Code(0)).unwrap(), "0");
        assert_eq!(
            serde_json::to_string(&Rc::Word(RcWord::Timeout)).unwrap(),
            "\"timeout\""
        );
        let back: Rc = serde_json::from_str("\"signal\"").unwrap();
        assert_eq!(back, Rc::Word(RcWord::Signal));
        let back: Rc = serde_json::from_str("3").unwrap();
        assert_eq!(back, Rc::Code(3));
    }

    #[test]
    fn round_keeps_places() {
        assert_eq!(round(1.234_56, 4), 1.2346);
        assert_eq!(round(0.000_04, 4), 0.0);
    }
}
