//! `conformance bench`'s `edit_loop` metric: the agent's edit loop, the
//! headline number for agents (docs/architecture.md, "Benchmarks").
//!
//! Each case is a model and a one-line edit (`find` replaced by `replace`,
//! whose `{N}` takes a new value every iteration, so no edit is ever a
//! repeat that the geometry cache would answer whole). It is timed four
//! ways, best and median of `runs` edits, all warm except the cold ones:
//!
//! - `serve`: `neoscad serve` on stdio, driven over JSON-RPC by this
//!   harness: `update` with the edited text, then `render` (the re-render)
//!   or `snapshot` (the contact sheet, render included). The time is from
//!   sending `update` to the answer.
//! - `cli_via_serve`: the edited file written to disk, then the command
//!   line (`neoscad FILE -o out.stl`, `neoscad snapshot FILE`) as a client
//!   of `neoscad serve --socket`. Process start, the socket round trip and
//!   the server's work.
//! - `cli_cold`: the same commands with no server.
//! - `nightly_cold`: the pinned nightly exporting the same edit to STL and
//!   to a PNG (`--render`, 1024x1024: one view where a snapshot draws four).

use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::bench::time_run_with;

#[derive(Debug, Deserialize)]
pub struct EditLoop {
    description: String,
    runs: u32,
    cases: BTreeMap<String, Case>,
}

#[derive(Debug, Deserialize)]
struct Case {
    description: String,
    #[serde(default)]
    file: Option<String>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    requires: Vec<String>,
    /// The text to edit (its first occurrence) and its replacement, where
    /// `{N}` is `start + step * i` for the i-th edit.
    find: String,
    replace: String,
    start: f64,
    step: f64,
}

pub struct Setup<'a> {
    pub neoscad: &'a Path,
    /// The nightly and its backend flag.
    pub nightly: Option<(&'a Path, &'a [String])>,
    pub work: &'a Path,
    pub libpath: &'a Path,
    pub timeout: Duration,
    /// `--runs` from the command line, overriding the config's.
    pub runs: Option<u32>,
}

impl Case {
    fn text(&self, base: &str, i: u32) -> String {
        let n = self.start + self.step * f64::from(i);
        base.replacen(&self.find, &self.replace.replace("{N}", &format!("{n}")), 1)
    }
}

fn ms_stats(v: &[f64]) -> Value {
    if v.is_empty() {
        return Value::Null;
    }
    let mut s = v.to_vec();
    s.sort_by(f64::total_cmp);
    let r = |x: f64| (x * 10.0).round() / 10.0;
    json!({"best_ms": r(s[0]), "median_ms": r(s[s.len() / 2]), "runs": s.len()})
}

/// `neoscad serve` on stdio.
struct Stdio_ {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Stdio_ {
    fn start(neoscad: &Path, cwd: &Path, libpath: &Path) -> Result<Stdio_, String> {
        let mut child = Command::new(neoscad)
            .arg("serve")
            .current_dir(cwd)
            .env("OPENSCADPATH", libpath)
            .env_remove("NEOSCAD_FONT_DIR")
            .env_remove("OPENSCAD_FONT_PATH")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("cannot start neoscad serve: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = BufReader::new(child.stdout.take().ok_or("no stdout")?);
        Ok(Stdio_ {
            child,
            stdin,
            stdout,
            next: 1,
        })
    }

    fn send(&mut self, v: &Value) -> Result<(), String> {
        let body = v.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len())
            .and_then(|_| self.stdin.flush())
            .map_err(|e| e.to_string())
    }

    fn read(&mut self) -> Result<Value, String> {
        let mut len = 0usize;
        loop {
            let mut line = String::new();
            if self
                .stdout
                .read_line(&mut line)
                .map_err(|e| e.to_string())?
                == 0
            {
                return Err("neoscad serve closed its output".into());
            }
            let l = line.trim_end();
            if l.is_empty() {
                if len > 0 {
                    break;
                }
                continue;
            }
            if let Some(v) = l.strip_prefix("Content-Length:") {
                len = v.trim().parse().map_err(|_| format!("bad header '{l}'"))?;
            }
        }
        let mut buf = vec![0; len];
        self.stdout
            .read_exact(&mut buf)
            .map_err(|e| e.to_string())?;
        serde_json::from_slice(&buf).map_err(|e| e.to_string())
    }

    fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next;
        self.next += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        loop {
            let m = self.read()?;
            if m.get("id") != Some(&json!(id)) {
                continue;
            }
            if let Some(e) = m.get("error") {
                return Err(format!("{method}: {e}"));
            }
            return Ok(m["result"].clone());
        }
    }

    fn stop(mut self) {
        let _ = self.call("shutdown", Value::Null);
        let _ = self.send(&json!({"jsonrpc": "2.0", "method": "exit"}));
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Run every case.
pub fn run(
    cfg: &EditLoop,
    s: &Setup<'_>,
    expand: &dyn Fn(&str) -> PathBuf,
    missing: &dyn Fn(&[String]) -> Option<String>,
) -> Result<Value, String> {
    let runs = s.runs.unwrap_or(cfg.runs).max(3);
    let dir = s.work.join("edit_loop");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let mut cases = serde_json::Map::new();
    for (id, c) in &cfg.cases {
        if let Some(why) = missing(&c.requires) {
            eprintln!("note: skipping edit_loop {id}: {why}");
            cases.insert(id.clone(), json!({"skipped": why}));
            continue;
        }
        let base = match (&c.file, &c.source) {
            (Some(f), _) => {
                let p = expand(f);
                std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?
            }
            (None, Some(src)) => src.clone(),
            _ => return Err(format!("edit_loop {id}: needs a file or a source")),
        };
        if !base.contains(&c.find) {
            return Err(format!("edit_loop {id}: '{}' is not in the model", c.find));
        }
        let file = dir.join(format!("{id}.scad"));
        let v = case(c, s, &base, &file, runs)?;
        eprintln!("{:22} {:18} {}", "edit_loop", id, summary_line(&v));
        cases.insert(id.clone(), v);
    }
    Ok(json!({"description": cfg.description, "cases": cases}))
}

fn summary_line(v: &Value) -> String {
    let b = |p: &str| {
        v.pointer(p)
            .and_then(|x| x["best_ms"].as_f64())
            .map_or("-".to_string(), |x| format!("{x:.1}"))
    };
    format!(
        "serve render={} snapshot={} | cli via serve stl={} snapshot={} | cold stl={} snapshot={} | nightly stl={} png={} (best ms)",
        b("/serve/render"),
        b("/serve/snapshot"),
        b("/cli_via_serve/export_stl"),
        b("/cli_via_serve/snapshot"),
        b("/cli_cold/export_stl"),
        b("/cli_cold/snapshot"),
        b("/nightly_cold/export_stl"),
        b("/nightly_cold/png"),
    )
}

fn case(c: &Case, s: &Setup<'_>, base: &str, file: &Path, runs: u32) -> Result<Value, String> {
    let dir = file.parent().expect("a file in the work directory");
    let path = file.to_string_lossy().into_owned();
    let png = dir.join("out.png").to_string_lossy().into_owned();
    let stl = dir.join("out.stl").to_string_lossy().into_owned();
    let mut i = 0u32;
    let next = |i: &mut u32| {
        *i += 1;
        c.text(base, *i)
    };

    // In-process protocol.
    let mut srv = Stdio_::start(s.neoscad, dir, s.libpath)?;
    srv.call("initialize", Value::Null)?;
    srv.call("open", json!({"path": path, "text": next(&mut i)}))?;
    let warm = srv.call(
        "snapshot",
        json!({"path": path, "output": png, "progress": false}),
    )?;
    if warm["exit_code"] != 0 {
        srv.stop();
        return Err(format!("{path}: the model fails: {warm}"));
    }
    let (mut render, mut snap) = (Vec::new(), Vec::new());
    for _ in 0..runs {
        let t = Instant::now();
        srv.call("update", json!({"path": path, "text": next(&mut i)}))?;
        let r = srv.call("render", json!({"path": path, "progress": false}))?;
        render.push(t.elapsed().as_secs_f64() * 1000.0);
        if r["exit_code"] != 0 {
            return Err(format!("{path}: render failed: {r}"));
        }
        let t = Instant::now();
        srv.call("update", json!({"path": path, "text": next(&mut i)}))?;
        srv.call(
            "snapshot",
            json!({"path": path, "output": png, "progress": false}),
        )?;
        snap.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    srv.stop();

    // The command line, with and without a server behind it.
    let socket = dir.join("s.sock");
    let _ = std::fs::remove_file(&socket);
    let mut daemon = Command::new(s.neoscad)
        .args(["serve", "--socket"])
        .arg(&socket)
        .args(["--idle-timeout", "120"])
        .current_dir(dir)
        .env("OPENSCADPATH", s.libpath)
        .env("NEOSCAD_SOCKET", &socket)
        .env_remove("NEOSCAD_FONT_DIR")
        .env_remove("OPENSCAD_FONT_PATH")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("cannot start neoscad serve --socket: {e}"))?;
    let t = Instant::now();
    while !socket.exists() && t.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(5));
    }
    let neo = s.neoscad.to_string_lossy().into_owned();
    let export_cmd = vec![neo.clone(), path.clone(), "-o".into(), stl.clone()];
    let snap_cmd = vec![
        neo.clone(),
        "snapshot".into(),
        path.clone(),
        "-o".into(),
        png.clone(),
    ];
    let log = dir.join("cli.stderr");
    let env: Vec<(&str, &Path)> = vec![("OPENSCADPATH", s.libpath), ("NEOSCAD_SOCKET", &socket)];
    let timed = |cmd: &[String], served: bool, i: &mut u32| -> Result<f64, String> {
        std::fs::write(file, next(i)).map_err(|e| e.to_string())?;
        let r = time_run_with(cmd, dir, &env, s.timeout, &log, served)?;
        if r.code != Some(0) {
            let why = std::fs::read_to_string(&log).unwrap_or_default();
            return Err(format!("{} failed ({:?}): {why}", cmd.join(" "), r.code));
        }
        Ok(r.wall_s * 1000.0)
    };
    // Warm the server's caches and the GPU once, as an agent's first run
    // would.
    timed(&snap_cmd, true, &mut i)?;
    timed(&export_cmd, true, &mut i)?;
    let requests_before = served_requests(s.neoscad, &socket);
    let (mut via_export, mut via_snap, mut cold_export, mut cold_snap) =
        (Vec::new(), Vec::new(), Vec::new(), Vec::new());
    for _ in 0..runs {
        via_export.push(timed(&export_cmd, true, &mut i)?);
        via_snap.push(timed(&snap_cmd, true, &mut i)?);
    }
    let served = served_requests(s.neoscad, &socket).saturating_sub(requests_before);
    for _ in 0..runs {
        cold_export.push(timed(&export_cmd, false, &mut i)?);
        cold_snap.push(timed(&snap_cmd, false, &mut i)?);
    }
    let _ = Command::new(s.neoscad)
        .args(["serve", "--stop"])
        .env("NEOSCAD_SOCKET", &socket)
        .stderr(Stdio::null())
        .status();
    let _ = daemon.kill();
    let _ = daemon.wait();

    // The nightly, cold.
    let nightly = match s.nightly {
        Some((bin, args)) => {
            let bin = bin.to_string_lossy().into_owned();
            let with = |extra: &[&str]| -> Vec<String> {
                let mut v = vec![bin.clone()];
                v.extend(args.iter().cloned());
                v.extend(extra.iter().map(|x| x.to_string()));
                v
            };
            let n_stl = with(&["-o", &stl, &path]);
            let n_png = with(&[
                "--render",
                "--viewall",
                "--autocenter",
                "--imgsize=1024,1024",
                "-o",
                &png,
                &path,
            ]);
            let (mut e, mut p) = (Vec::new(), Vec::new());
            for _ in 0..runs {
                e.push(timed(&n_stl, false, &mut i)?);
                p.push(timed(&n_png, false, &mut i)?);
            }
            json!({"export_stl": ms_stats(&e), "png": ms_stats(&p)})
        }
        None => Value::Null,
    };
    Ok(json!({
        "description": c.description,
        "edit": {"find": c.find, "replace": c.replace},
        "serve": {"render": ms_stats(&render), "snapshot": ms_stats(&snap)},
        "cli_via_serve": {
            "export_stl": ms_stats(&via_export),
            "snapshot": ms_stats(&via_snap),
            // Requests the server answered during the timed runs: 2 per run
            // when every client run went through it.
            "served_requests": served,
        },
        "cli_cold": {"export_stl": ms_stats(&cold_export), "snapshot": ms_stats(&cold_snap)},
        "nightly_cold": nightly,
    }))
}

/// The server's request count, from `neoscad serve --status --format json`.
fn served_requests(neoscad: &Path, socket: &Path) -> u64 {
    Command::new(neoscad)
        .args(["serve", "--status", "--format", "json"])
        .env("NEOSCAD_SOCKET", socket)
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok())
        .and_then(|v| v["stats"]["requests"].as_u64())
        .unwrap_or(0)
}
