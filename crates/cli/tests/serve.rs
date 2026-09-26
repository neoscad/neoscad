//! `neoscad serve` end to end: the JSON-RPC protocol over stdio
//! (docs/serve-protocol.md), and the command line as a client of a socket
//! server, whose outputs must be the ones a local run makes.

use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Output, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_neoscad");

/// A fresh directory for one test (short: socket paths are limited).
fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nss-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

struct Stdio_ {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
    /// Notifications seen, in order.
    notes: Vec<Value>,
}

impl Stdio_ {
    fn start(dir: &Path) -> Stdio_ {
        Stdio_::start_env(dir, &[])
    }

    fn start_env(dir: &Path, env: &[(&str, &str)]) -> Stdio_ {
        let mut child = Command::new(BIN)
            .arg("serve")
            .current_dir(dir)
            .envs(env.iter().copied())
            .env_remove("OPENSCADPATH")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Stdio_ {
            stdin: child.stdin.take().unwrap(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
            next: 1,
            notes: Vec::new(),
        }
    }

    fn send(&mut self, v: &Value) {
        let body = v.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        self.stdin.flush().unwrap();
    }

    fn read(&mut self) -> Value {
        let mut len = 0;
        loop {
            let mut line = String::new();
            assert!(
                self.stdout.read_line(&mut line).unwrap() > 0,
                "server closed"
            );
            let l = line.trim_end();
            if l.is_empty() {
                if len > 0 {
                    break;
                }
                continue;
            }
            let (k, v) = l.split_once(':').unwrap();
            assert_eq!(k, "Content-Length");
            len = v.trim().parse().unwrap();
        }
        let mut buf = vec![0; len];
        self.stdout.read_exact(&mut buf).unwrap();
        serde_json::from_slice(&buf).unwrap()
    }

    /// The whole response (result or error).
    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let m = self.read();
            if m.get("id") == Some(&json!(id)) {
                assert_eq!(m["jsonrpc"], "2.0");
                return m;
            }
            assert!(m.get("method").is_some(), "a notification: {m}");
            self.notes.push(m);
        }
    }

    fn result(&mut self, method: &str, params: Value) -> Value {
        let m = self.call(method, params);
        assert!(m.get("error").is_none(), "{method}: {m}");
        m["result"].clone()
    }
}

#[test]
fn the_protocol_round_trips_over_stdio() {
    let d = scratch("proto");
    let mut s = Stdio_::start(&d);
    let init = s.result("initialize", json!({}));
    assert_eq!(init["protocol"], 1);
    let methods = init["capabilities"]["methods"].as_array().unwrap();
    for m in [
        "open", "update", "render", "export", "snapshot", "cancel", "evaluate",
    ] {
        assert!(methods.contains(&json!(m)), "{m}");
    }

    // A document that exists only in the server's memory.
    let path = d.join("m.scad");
    let p = path.to_str().unwrap();
    let doc = s.result(
        "open",
        json!({"path": p, "text": "cube(10);\necho(\"a\");\n"}),
    );
    assert_eq!(doc["length"], 21);
    let r = s.result("render", json!({"path": p}));
    assert_eq!(r["exit_code"], 0);
    assert_eq!(r["geometry"]["volume"], 1000.0);
    assert_eq!(r["echo"], json!(["ECHO: \"a\""]));
    assert!(
        s.notes
            .iter()
            .any(|n| n["method"] == "progress" && n["params"]["stage"] == "geometry"),
        "{:?}",
        s.notes
    );
    assert!(s.notes.iter().any(|n| n["method"] == "diagnostics"));

    // An incremental edit: "cube(10)" becomes "cub(10)", a warning with a
    // hint.
    let doc = s.result(
        "update",
        json!({"path": p, "edits": [{"start": 3, "end": 4, "text": ""}]}),
    );
    assert_eq!(doc["length"], 20);
    let e = s.result("evaluate", json!({"path": p, "csg": true}));
    let diag = &e["diagnostics"][0];
    assert_eq!(diag["code"], "unknown-module");
    assert_eq!(diag["severity"], "warning");
    assert_eq!(diag["line"], 1);
    assert_eq!(
        diag["text"],
        "WARNING: Ignoring unknown module 'cub' in file m.scad, line 1"
    );
    assert_eq!(diag["hints"][0]["message"], "did you mean 'cube'?");
    // Nothing is instantiated, so the tree is empty (as the nightly's
    // `.csg` of this file is).
    assert!(e["csg"].is_string());

    // Export writes the file where the request's cwd says.
    s.result(
        "update",
        json!({"path": p, "text": "difference() { cube(4, center=true); sphere(2.5); }\n"}),
    );
    let x = s.result(
        "export",
        json!({"path": "m.scad", "cwd": d, "output": "m.stl"}),
    );
    assert_eq!(x["exit_code"], 0);
    assert_eq!(x["format"], "stl");
    let stl = std::fs::read(d.join("m.stl")).unwrap();
    assert_eq!(x["bytes"], stl.len());
    assert!(stl.starts_with(b"solid OpenSCAD_Model"));

    // The other formats: the messages, the tree, the program, an image.
    s.result(
        "update",
        json!({"path": p, "text": "echo(\"e\");\ncube(2);\n"}),
    );
    for (f, starts) in [
        ("echo", &b"ECHO: \"e\""[..]),
        ("csg", b"cube(size = [2, 2, 2]"),
        ("ast", b"echo(\"e\");"),
        ("png", b"\x89PNG"),
    ] {
        let out = format!("m.{f}");
        let x = s.call("export", json!({"path": p, "output": d.join(&out)}));
        if f == "png" && x.get("error").is_some() {
            // No GPU on this machine.
            continue;
        }
        let x = &x["result"];
        assert_eq!(x["exit_code"], 0, "{f}: {x}");
        let data = std::fs::read(d.join(&out)).unwrap();
        assert!(
            data.starts_with(starts),
            "{f}: {}",
            String::from_utf8_lossy(&data)
        );
        assert_eq!(x["bytes"], data.len());
    }

    // Errors: unknown methods and bad parameters.
    assert_eq!(s.call("nope", json!({}))["error"]["code"], -32601);
    assert_eq!(s.call("render", json!({}))["error"]["code"], -32602);
    assert_eq!(
        s.call("render", json!({"path": p, "mode": "x"}))["error"]["code"],
        -32602
    );
    let st = s.result("stats", Value::Null);
    assert!(st["geometry_cache"]["entries"].as_u64().unwrap() > 0);
    assert_eq!(st["documents"], 1);
    assert_eq!(s.result("close", json!({"path": p}))["closed"], true);

    s.result("shutdown", Value::Null);
    s.send(&json!({"jsonrpc": "2.0", "method": "exit"}));
    let status = s.child.wait().unwrap();
    assert!(status.success());
}

#[test]
fn a_stale_request_is_cancelled_by_a_newer_one() {
    let d = scratch("cancel");
    let mut s = Stdio_::start(&d);
    let p = d.join("slow.scad");
    let p = p.to_str().unwrap();
    s.result(
        "open",
        json!({"path": p, "text": "n = 8000;\necho(len([for (i = [0:n]) for (j = [0:n]) if (i * j < 0) 1]));\n"}),
    );
    // Send the slow request, then (once it is evaluating) replace the text
    // and render again: the first answers "cancelled".
    s.send(&json!({"jsonrpc": "2.0", "id": 100, "method": "evaluate", "params": {"path": p}}));
    let t = Instant::now();
    loop {
        let m = s.read();
        if m["method"] == "progress"
            && m["params"]["id"] == 100
            && m["params"]["stage"] == "evaluate"
        {
            break;
        }
    }
    s.send(&json!({"jsonrpc": "2.0", "id": 101, "method": "update", "params": {"path": p, "text": "cube(1);"}}));
    s.send(&json!({"jsonrpc": "2.0", "id": 102, "method": "render", "params": {"path": p}}));
    let mut answers = std::collections::HashMap::new();
    while answers.len() < 3 {
        let m = s.read();
        if let Some(id) = m.get("id").and_then(Value::as_u64) {
            answers.insert(id, m);
        }
    }
    assert_eq!(
        answers[&100]["error"]["code"], -32800,
        "{:?}",
        answers[&100]
    );
    assert_eq!(answers[&102]["result"]["geometry"]["volume"], 1.0);
    assert!(t.elapsed() < Duration::from_secs(10));
    s.result("shutdown", Value::Null);
}

fn neoscad(dir: &Path, socket: Option<&Path>, args: &[&str]) -> Output {
    let mut c = Command::new(BIN);
    c.args(args).current_dir(dir).env_remove("OPENSCADPATH");
    match socket {
        Some(s) => c.env("NEOSCAD_SOCKET", s).env_remove("NEOSCAD_NO_SERVER"),
        None => c.env("NEOSCAD_NO_SERVER", "1"),
    };
    c.output().unwrap()
}

/// stderr without the render summary's cache count and time, which are
/// the server's (warm) rather than a fresh process's.
fn comparable(stderr: &[u8]) -> String {
    String::from_utf8_lossy(stderr)
        .lines()
        .filter(|l| {
            !l.starts_with("Geometries in cache:") && !l.starts_with("Total rendering time:")
        })
        .map(|l| format!("{l}\n"))
        .collect()
}

fn served_requests(dir: &Path, socket: &Path) -> u64 {
    let o = neoscad(
        dir,
        Some(socket),
        &["serve", "--status", "--format", "json"],
    );
    let v: Value = serde_json::from_slice(&o.stdout).unwrap_or(Value::Null);
    v["stats"]["requests"].as_u64().unwrap_or(0)
}

#[test]
fn served_outputs_are_the_direct_ones() {
    let d = scratch("determinism");
    std::fs::create_dir_all(d.join("sub")).unwrap();
    let models: &[(&str, &str, &[&str])] = &[
        (
            "boolean.scad",
            "difference() { cube(10, center=true); sphere(6, $fn=40); }\nfor (i=[0:3]) translate([15*i,0,0]) rotate([0,0,30*i]) cylinder(h=5, r1=3, r2=1);\n",
            &["stl", "off", "3mf", "obj", "wrl"],
        ),
        (
            "sub/twod.scad",
            "offset(r=1) difference() { square(10); translate([5,5]) circle(3); }\ntext(\"Hi\", size=4);\n",
            &["svg", "dxf"],
        ),
        (
            "warns.scad",
            "echo(\"x\", [1,2]);\nunion() { cube(1); square(2); }\ncub(3);\nassert(true);\n",
            &["stl"],
        ),
        ("empty.scad", "echo(1);\n", &["stl"]),
        ("syntax.scad", "cube(1)\nsphere(2)\n", &["stl"]),
        ("mixed.scad", "cube(3);\n", &["svg"]),
    ];
    for (name, src, _) in models {
        std::fs::write(d.join(name), src).unwrap();
    }
    let socket = d.join("s.sock");
    let mut server = Command::new(BIN)
        .args(["serve", "--socket"])
        .arg(&socket)
        .current_dir(&d)
        .env_remove("OPENSCADPATH")
        .env("NEOSCAD_SOCKET", &socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let t = Instant::now();
    while !socket.exists() && t.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(socket.exists(), "the server did not start");
    let mut expected_requests = 0;
    for (name, _, formats) in models {
        for f in *formats {
            for extra in [&[][..], &["-D", "$fn=12", "--render=force"][..]] {
                let out_direct = format!("direct.{f}");
                let out_served = format!("served.{f}");
                let mut a = vec![*name, "-o", out_direct.as_str()];
                a.extend_from_slice(extra);
                let direct = neoscad(&d, None, &a);
                let mut b = vec![*name, "-o", out_served.as_str()];
                b.extend_from_slice(extra);
                let served = neoscad(&d, Some(&socket), &b);
                expected_requests += 1;
                let what = format!("{name} -> {f} {extra:?}");
                assert_eq!(direct.status.code(), served.status.code(), "{what}");
                assert_eq!(
                    comparable(&direct.stderr),
                    comparable(&served.stderr),
                    "{what}"
                );
                let (x, y) = (
                    std::fs::read(d.join(&out_direct)).ok(),
                    std::fs::read(d.join(&out_served)).ok(),
                );
                // 3MF files carry a creation date; the rest must match
                // byte for byte.
                if *f != "3mf" {
                    assert_eq!(x, y, "{what}");
                } else {
                    assert_eq!(x.map(|v| v.len()), y.map(|v| v.len()), "{what}");
                }
                let _ = std::fs::remove_file(d.join(&out_direct));
                let _ = std::fs::remove_file(d.join(&out_served));
            }
        }
    }
    // Images, where a GPU is available: the preview, the render and the
    // view flags, drawn by the server from its warm caches.
    let probe = neoscad(&d, None, &["boolean.scad", "-o", "probe.png"]);
    if probe.status.success() {
        for extra in [
            &[][..],
            &["--render"][..],
            &["--preview=throwntogether", "--view=axes,scales"][..],
            &[
                "--render",
                "--camera=0,0,0,55,0,25,80",
                "--imgsize=300,200",
                "--colorscheme=Metallic",
            ][..],
        ] {
            let mut a = vec!["warns.scad", "-o", "direct.png"];
            a.extend_from_slice(extra);
            let mut b = vec!["warns.scad", "-o", "served.png"];
            b.extend_from_slice(extra);
            let direct = neoscad(&d, None, &a);
            let served = neoscad(&d, Some(&socket), &b);
            expected_requests += 1;
            let what = format!("png {extra:?}");
            assert_eq!(direct.status.code(), served.status.code(), "{what}");
            assert_eq!(
                comparable(&direct.stderr),
                comparable(&served.stderr),
                "{what}"
            );
            assert_eq!(
                std::fs::read(d.join("direct.png")).unwrap(),
                std::fs::read(d.join("served.png")).unwrap(),
                "{what}"
            );
        }
    } else {
        eprintln!(
            "skipped the image comparison: {}",
            String::from_utf8_lossy(&probe.stderr)
        );
    }

    // The same through `--format json`, and every run really was served.
    let a = neoscad(&d, None, &["warns.scad", "-o", "w.stl", "--format", "json"]);
    let b = neoscad(
        &d,
        Some(&socket),
        &["warns.scad", "-o", "w.stl", "--format", "json"],
    );
    expected_requests += 1;
    let (mut a, mut b): (Value, Value) = (
        serde_json::from_slice(&a.stdout).unwrap(),
        serde_json::from_slice(&b.stdout).unwrap(),
    );
    assert_eq!(
        (a["served"].clone(), b["served"].clone()),
        (json!(false), json!(true))
    );
    for v in [&mut a, &mut b] {
        v["served"] = Value::Null;
        v["timings_ms"] = Value::Null;
        v["log"] = Value::Null;
    }
    assert_eq!(a, b);
    assert_eq!(served_requests(&d, &socket), expected_requests);

    // Snapshots, where a GPU is available.
    let a = neoscad(
        &d,
        None,
        &[
            "snapshot",
            "boolean.scad",
            "-o",
            "a.png",
            "--format",
            "json",
        ],
    );
    if a.status.success() {
        let b = neoscad(
            &d,
            Some(&socket),
            &[
                "snapshot",
                "boolean.scad",
                "-o",
                "b.png",
                "--format",
                "json",
            ],
        );
        assert!(b.status.success(), "{}", String::from_utf8_lossy(&b.stderr));
        assert_eq!(
            std::fs::read(d.join("a.png")).unwrap(),
            std::fs::read(d.join("b.png")).unwrap()
        );
        let (mut a, mut b): (Value, Value) = (
            serde_json::from_slice(&a.stdout).unwrap(),
            serde_json::from_slice(&b.stdout).unwrap(),
        );
        for v in [&mut a, &mut b] {
            v["timings_ms"] = Value::Null;
            v["output"] = Value::Null;
        }
        assert_eq!(a, b);
    } else {
        eprintln!(
            "skipped the snapshot comparison: {}",
            String::from_utf8_lossy(&a.stderr)
        );
    }

    let stop = neoscad(&d, Some(&socket), &["serve", "--stop"]);
    assert!(stop.status.success());
    let status = server.wait().unwrap();
    assert!(status.success());
    assert!(!socket.exists(), "the socket is removed on exit");
}

/// A request that panics is answered with an internal error, and the
/// server, its documents and its caches carry on.
#[test]
fn a_panicking_request_does_not_end_the_server() {
    let d = scratch("panic");
    let mut s = Stdio_::start_env(&d, &[("NEOSCAD_SERVE_TEST_PANIC", "1")]);
    // The geometry budget is the real one before the first render.
    let st = s.result("stats", json!({}));
    assert_eq!(st["geometry_cache"]["budget"], 200 << 20, "{st}");
    let path = d.join("m.scad");
    let p = path.to_str().unwrap();
    s.result("open", json!({"path": p, "text": "cube(10);"}));
    let first = s.result("render", json!({"path": p}));
    let m = s.call("debug.panic", json!({"path": p}));
    assert_eq!(m["error"]["code"], -32603, "{m}");
    assert!(
        m["error"]["message"]
            .as_str()
            .unwrap()
            .contains("debug.panic requested"),
        "{m}"
    );
    let again = s.result("render", json!({"path": p}));
    assert_eq!(again["geometry"], first["geometry"]);
    assert_eq!(
        s.result("documents", json!({})).as_array().unwrap().len(),
        1
    );
}

#[test]
fn check_and_measure_are_served() {
    let d = scratch("check");
    let mut s = Stdio_::start(&d);
    let init = s.result("initialize", json!({}));
    let methods = init["capabilities"]["methods"].as_array().unwrap();
    for m in ["check", "measure", "cli.check", "cli.measure"] {
        assert!(methods.contains(&json!(m)), "{m}");
    }
    assert_eq!(init["capabilities"]["features"], json!(["part"]));
    let path = d.join("m.scad");
    let p = path.to_str().unwrap();
    s.result(
        "open",
        json!({"path": p, "text": "part(\"a\") cube(10); part(\"b\") translate([12,0,0]) cube([5,0.5,5]);"}),
    );
    let c = s.result(
        "check",
        json!({"path": p, "enable": ["part"], "bed": "100x100x100"}),
    );
    assert_eq!(c["exit_code"], 0, "{c}");
    assert_eq!(c["settings"]["bed"], json!([100.0, 100.0, 100.0]));
    let thin: Vec<&Value> = c["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["code"] == "thin-wall")
        .collect();
    assert_eq!(thin.len(), 1, "{c}");
    assert_eq!(thin[0]["part"], "b");
    let bad = s.call("check", json!({"path": p, "max_overhang": 100}));
    assert_eq!(bad["error"]["code"], -32602, "{bad}");
    let m = s.result(
        "measure",
        json!({"path": p, "parts": true, "between": ["a", "b"], "section": "z=1", "svg": true}),
    );
    assert_eq!(m["between"]["distance"], 2.0, "{m}");
    assert_eq!(m["section"]["area"], 102.5, "{m}");
    assert!(
        m["section"]["svg_text"]
            .as_str()
            .unwrap()
            .starts_with("<?xml")
    );
    // Without `part` enabled the same text has no parts.
    let m = s.result("measure", json!({"path": p}));
    assert_eq!(m["parts"], json!([]), "{m}");
    assert_eq!(m["diagnostics"]["warnings"], 2, "{m}");
}

#[test]
fn format_docs_and_test_are_served() {
    let d = scratch("fdt");
    let mut s = Stdio_::start(&d);
    let init = s.result("initialize", json!({}));
    let methods = init["capabilities"]["methods"].as_array().unwrap();
    for m in ["format", "docs", "test"] {
        assert!(methods.contains(&json!(m)), "{m}");
    }
    // `format` reads an open document's unsaved text and writes nothing.
    let path = d.join("m.scad");
    let p = path.to_str().unwrap();
    std::fs::write(&path, "cube(1);\n").unwrap();
    s.result("open", json!({"path": p, "text": "module m(){cube(2);}"}));
    let f = s.result("format", json!({"path": p, "diff": true}));
    assert_eq!(f["changed"], true, "{f}");
    assert_eq!(f["text"], "module m() {\n    cube(2);\n}\n");
    assert!(f["diff"].as_str().unwrap().contains("+    cube(2);"), "{f}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), "cube(1);\n");
    let f = s.result("format", json!({"text": "x=1;", "indent": 2}));
    assert_eq!(f["text"], "x = 1;\n");
    let f = s.result("format", json!({"text": "x=;"}));
    assert_eq!(f["exit_code"], 1);
    assert_eq!(f["error"]["kind"], "syntax");
    assert_eq!(s.call("format", json!({}))["error"]["code"], -32602);
    // `docs`.
    let r = s.result("docs", json!({"name": "cube"}));
    assert_eq!(r["entries"][0]["signature"], "cube(size=1, center=false)");
    let r = s.result("docs", json!({"name": "m", "file": p}));
    assert_eq!(r["entries"][0]["signature"], "module m()", "{r}");
    // `test`: the document's buffer is what runs.
    let t = d.join("m_test.scad");
    std::fs::write(&t, "// @expect volume 8\nmodule test_c() cube(2);\n").unwrap();
    let r = s.result("test", json!({"cwd": d.to_str().unwrap(), "jobs": 2}));
    assert_eq!(r["exit_code"], 0, "{r}");
    assert_eq!(r["counts"]["tests"], 1);
    assert_eq!(r["tests"][0]["id"], "m_test.scad::test_c");
}

#[test]
fn versions_limits_and_missing_files_over_stdio() {
    let d = scratch("harden");
    let mut s = Stdio_::start(&d);
    let p = d.join("v.scad");
    let p = p.to_str().unwrap();
    // One version per change: a rejected update changes nothing, the
    // version included (the audit saw open 1, then 3).
    assert_eq!(
        s.result("open", json!({"path": p, "text": "cube(1);"}))["version"],
        1
    );
    let bad = s.call(
        "update",
        json!({"path": p, "edits": [{"start": 50, "end": 60, "text": "x"}]}),
    );
    assert_eq!(bad["error"]["code"], -32602, "{bad}");
    assert_eq!(s.result("render", json!({"path": p}))["exit_code"], 0);
    assert_eq!(
        s.result("update", json!({"path": p, "text": "cube(2);"}))["version"],
        2
    );
    // The server runs with the agent limits unless a request changes them.
    s.result(
        "update",
        json!({"path": p, "text": "circle(r=1, $fn=100000);"}),
    );
    let r = s.result("render", json!({"path": p}));
    assert_eq!(r["exit_code"], 1, "{r}");
    assert_eq!(r["diagnostics"][0]["code"], "resource-limit", "{r}");
    assert!(
        r["diagnostics"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("ERROR: Resource limit exceeded: circle() would make 100,000 fragments")
    );
    let r = s.result(
        "render",
        json!({"path": p, "limits": {"fragments": 200000}}),
    );
    assert_eq!(r["exit_code"], 0, "{r}");
    let bad = s.call("render", json!({"path": p, "limits": {"frags": 1}}));
    assert_eq!(bad["error"]["code"], -32602, "{bad}");
    // A missing input says so, with a stable code (finding 5).
    let r = s.result("render", json!({"path": d.join("nope.scad")}));
    assert_eq!(r["exit_code"], 1);
    assert_eq!(r["diagnostics"][0]["code"], "input-not-found", "{r}");
    // An export that cannot be written, likewise.
    std::fs::write(d.join("ok.scad"), "cube(1);").unwrap();
    let r = s.result(
        "export",
        json!({"path": d.join("ok.scad"), "output": d.join("no/such/dir/x.stl")}),
    );
    assert_eq!(r["exit_code"], 1, "{r}");
    assert_eq!(r["diagnostics"][0]["code"], "output-not-writable", "{r}");
    // check's settings errors name the parameter, not a flag.
    let bad = s.call("check", json!({"path": d.join("ok.scad"), "nozzle": -1}));
    let m = bad["error"]["message"].as_str().unwrap();
    assert!(m.starts_with("`nozzle` must be"), "{m}");
    s.result("shutdown", Value::Null);
}

#[test]
#[cfg(unix)]
fn the_socket_never_replaces_a_file_and_the_command_line_stays_unlimited() {
    let d = scratch("sock");
    // `--socket notes.txt` deleted the notes (finding 7).
    std::fs::write(d.join("notes.txt"), "precious notes").unwrap();
    let o = neoscad(
        &d,
        None,
        &["serve", "--socket", "notes.txt", "--idle-timeout", "1"],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("not a socket"),
        "{o:?}"
    );
    assert_eq!(
        std::fs::read_to_string(d.join("notes.txt")).unwrap(),
        "precious notes"
    );
    // The server has the agent limits; the command line's requests to it
    // are unlimited, as the command line is in its own process.
    let socket = d.join("s.sock");
    let mut server = Command::new(BIN)
        .args(["serve", "--socket"])
        .arg(&socket)
        .args(["--idle-timeout", "60"])
        .current_dir(&d)
        .env_remove("OPENSCADPATH")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let t = Instant::now();
    while !socket.exists() && t.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(10));
    }
    std::fs::write(d.join("fine.scad"), "circle(r=1, $fn=20000);").unwrap();
    let before = served_requests(&d, &socket);
    let o = neoscad(&d, Some(&socket), &["fine.scad", "-o", "fine.svg"]);
    assert_eq!(
        o.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(
        served_requests(&d, &socket) > before,
        "the run was not served"
    );
    assert!(d.join("fine.svg").exists());
    // `--limit` on the command line applies (and runs in-process).
    let o = neoscad(
        &d,
        Some(&socket),
        &["fine.scad", "-o", "limited.svg", "--limit", "fragments=100"],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&o.stderr)
            .contains("ERROR: Resource limit exceeded: circle() would make 20,000 fragments, over the fragments limit of 100 in file fine.scad, line 1"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(!d.join("limited.svg").exists());
    let _ = neoscad(&d, Some(&socket), &["serve", "--stop"]);
    let _ = server.wait();
}
