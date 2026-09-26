//! `neoscad mcp` end to end over stdio (docs/mcp.md): both handshakes of
//! MCP 2026-07-28's dual-era model, `tools/list`, a `tools/call` of every
//! tool, resources, and the roots that fence the server's file access.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{Value, json};

const BIN: &str = env!("CARGO_BIN_EXE_neoscad");

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("nsmcp-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.canonicalize().unwrap()
}

struct Mcp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Mcp {
    fn start(dir: &Path, args: &[&str]) -> Mcp {
        let mut child = Command::new(BIN)
            .arg("mcp")
            .args(args)
            .current_dir(dir)
            .env_remove("OPENSCADPATH")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        Mcp {
            stdin: child.stdin.take().unwrap(),
            stdout: BufReader::new(child.stdout.take().unwrap()),
            child,
            next: 1,
        }
    }

    fn send(&mut self, v: &Value) {
        // The stdio binding: one message per line.
        writeln!(self.stdin, "{v}").unwrap();
        self.stdin.flush().unwrap();
    }

    fn read(&mut self) -> Value {
        let mut line = String::new();
        assert!(
            self.stdout.read_line(&mut line).unwrap() > 0,
            "server closed"
        );
        serde_json::from_str(&line).unwrap()
    }

    /// A request and its response (the server sends nothing else).
    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let r = self.read();
        assert_eq!(r["id"], id, "{r}");
        r
    }

    fn tool(&mut self, name: &str, args: Value) -> Value {
        let r = self.call("tools/call", json!({"name": name, "arguments": args}));
        r["result"].clone()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn text(r: &Value) -> String {
    r["content"][0]["text"].as_str().unwrap_or("").to_string()
}

fn modern() -> Value {
    json!({"io.modelcontextprotocol/protocolVersion": "2026-07-28",
           "io.modelcontextprotocol/clientCapabilities": {}})
}

#[test]
fn handshakes_and_versions() {
    let dir = scratch("hs");
    let mut s = Mcp::start(&dir, &[]);
    // Legacy: `initialize` echoes a revision it speaks...
    let r = s.call(
        "initialize",
        json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "1"}}),
    );
    assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    assert!(r["result"]["capabilities"]["tools"].is_object());
    assert_eq!(r["result"]["serverInfo"]["name"], "neoscad");
    s.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    // ...and answers one it does not with its newest.
    let r = s.call("initialize", json!({"protocolVersion": "2099-01-01"}));
    assert_eq!(r["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(s.call("ping", json!({}))["result"], json!({}));
    // Modern: `server/discover`, and every result says it is complete
    // and carries the caching hints where the spec requires them.
    let r = s.call("server/discover", json!({"_meta": modern()}));
    let d = &r["result"];
    assert_eq!(d["supportedVersions"], json!(["2026-07-28"]));
    assert_eq!(d["resultType"], "complete");
    assert!(d["ttlMs"].as_u64().is_some() && d["cacheScope"] == "public");
    assert_eq!(
        d["_meta"]["io.modelcontextprotocol/serverInfo"]["name"],
        "neoscad"
    );
    // An unknown version is -32022 with what is supported.
    let r = s.call(
        "tools/list",
        json!({"_meta": {"io.modelcontextprotocol/protocolVersion": "1900-01-01",
                          "io.modelcontextprotocol/clientCapabilities": {}}}),
    );
    assert_eq!(r["error"]["code"], -32022);
    assert_eq!(r["error"]["data"]["requested"], "1900-01-01");
    // A modern request without its capabilities is malformed.
    let r = s.call(
        "tools/list",
        json!({"_meta": {"io.modelcontextprotocol/protocolVersion": "2026-07-28"}}),
    );
    assert_eq!(r["error"]["code"], -32602);
    // Unknown methods and tools are protocol errors.
    assert_eq!(s.call("nope", json!({}))["error"]["code"], -32601);
    let r = s.call("tools/call", json!({"name": "nope", "arguments": {}}));
    assert_eq!(r["error"]["code"], -32602);
    // A line that is not JSON is answered and the stream goes on.
    writeln!(s.stdin, "{{not json").unwrap();
    s.stdin.flush().unwrap();
    assert_eq!(s.read()["error"]["code"], -32700);
    assert_eq!(s.call("ping", json!({}))["result"], json!({}));
}

#[test]
fn tools_list_is_small_and_ordered() {
    let dir = scratch("list");
    let mut s = Mcp::start(&dir, &[]);
    let r = s.call("tools/list", json!({"_meta": modern()}));
    let tools = r["result"]["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "evaluate", "render", "snapshot", "check", "measure", "test", "format", "docs"
        ]
    );
    for t in tools {
        assert_eq!(t["inputSchema"]["type"], "object", "{t}");
        // Descriptions cost context in every session: one or two sentences.
        let d = t["description"].as_str().unwrap();
        assert!(
            d.len() < 300,
            "{} description is {} bytes",
            t["name"],
            d.len()
        );
    }
    // What a client puts in the model's context (name, description and
    // schema of each tool) stays small: about 1,400 tokens.
    let visible: Vec<Value> = tools
        .iter()
        .map(|t| json!([t["name"], t["description"], t["inputSchema"]]))
        .collect();
    let size = Value::Array(visible).to_string().len();
    assert!(size < 5500, "the tools are {size} bytes");
    eprintln!("tools as the model sees them: {size} bytes");
    assert_eq!(r["result"]["resultType"], "complete");
}

#[test]
fn every_tool_round_trips() {
    let dir = scratch("tools");
    std::fs::write(
        dir.join("box.scad"),
        "module box() difference() { cube([20, 20, 10]); translate([1, 1, 1]) cube([18, 18, 10]); }\nbox();\n",
    )
    .unwrap();
    std::fs::write(dir.join("messy.scad"), "cube( [1,2,3] ) ;\n").unwrap();
    let mut s = Mcp::start(&dir, &[]);

    // evaluate: diagnostics with hints, and echo.
    let r = s.tool("evaluate", json!({"source": "cub(2);\necho(\"hi\", 3);"}));
    assert_eq!(r["isError"], false);
    let t = text(&r);
    assert!(t.starts_with("ok: 1 warning"), "{t}");
    assert!(
        t.contains("warning inline.scad:1: Ignoring unknown module 'cub' (did you mean 'cube'?)"),
        "{t}"
    );
    assert!(t.contains("ECHO: \"hi\", 3"), "{t}");
    let d = &r["structuredContent"]["diagnostics"][0];
    assert_eq!(d["code"], "unknown-module");
    assert_eq!(d["hint"], "did you mean 'cube'?");
    assert!(
        d.get("file").is_none(),
        "the model's own file is implied: {d}"
    );
    // A syntax error is a result that says so, not a tool error.
    let r = s.tool("evaluate", json!({"source": "cube(1\nsphere(2);"}));
    assert_eq!(r["isError"], false);
    assert!(
        text(&r).starts_with("failed (exit 1): 1 error"),
        "{}",
        text(&r)
    );

    // render: the numbers, and an export.
    let r = s.tool("render", json!({"path": "box.scad"}));
    let t = text(&r);
    assert!(t.contains("3D bbox 20 x 20 x 10 mm"), "{t}");
    assert!(t.contains("volume 1084 mm³"), "{t}");
    assert_eq!(r["structuredContent"]["geometry"]["volume"], 1084.0);
    // The export's directory is made for it.
    let r = s.tool(
        "render",
        json!({"path": "box.scad", "export": "out/box.stl"}),
    );
    assert!(text(&r).contains("wrote "), "{}", text(&r));
    assert!(
        std::fs::read(dir.join("out/box.stl"))
            .unwrap()
            .starts_with(b"solid")
    );

    // snapshot: a PNG image, the summary, and a diff.
    let r = s.tool(
        "snapshot",
        json!({"path": "box.scad", "diff_source": "cube([20, 20, 10]);", "size": "256x256"}),
    );
    assert_eq!(r["content"][1]["type"], "image");
    assert_eq!(r["content"][1]["mimeType"], "image/png");
    assert!(
        r["content"][1]["data"]
            .as_str()
            .unwrap()
            .starts_with("iVBORw0KGgo")
    );
    let t = text(&r);
    assert!(
        t.contains("diff: added 0 mm³ (green), removed 2916 mm³ (red)"),
        "{t}"
    );
    assert_eq!(r["structuredContent"]["size"], json!([256, 256]));
    // Saved only when asked.
    assert!(!dir.join("box-snapshot.png").exists());
    let r = s.tool(
        "snapshot",
        json!({"path": "box.scad", "size": "128x128", "output": "s.png"}),
    );
    assert!(text(&r).contains("saved "), "{}", text(&r));
    assert!(dir.join("s.png").exists());

    // Issues on the sheet come back as terse findings (no bboxes).
    let r = s.tool(
        "snapshot",
        json!({"source": "cube(10); translate([0, 0, 20]) cube(2);", "issues": true, "size": "128x128"}),
    );
    let f = &r["structuredContent"]["issues"]["findings"][0];
    assert_eq!(f["code"], "floating", "{r}");
    assert!(f.get("location").is_none() && f["point"].is_array(), "{f}");

    // check: findings with locations and fixes.
    let r = s.tool(
        "check",
        json!({"source": "cube(10); translate([0, 0, 20]) cube(2);"}),
    );
    let t = text(&r);
    assert!(t.contains("error floating"), "{t}");
    assert!(t.contains("Fix: "), "{t}");
    assert_eq!(r["structuredContent"]["counts"]["errors"], 1);

    // measure: a section.
    let r = s.tool("measure", json!({"path": "box.scad", "section": "z=5"}));
    let t = text(&r);
    assert!(t.contains("section z=5: area 76 mm²"), "{t}");

    // test: inline test source against a model file.
    let r = s.tool(
        "test",
        json!({"source": "include <box.scad>\n// @expect volume 1084\nmodule test_box() box();\n// @expect volume 1\nmodule test_wrong() box();\n"}),
    );
    let t = text(&r);
    assert!(t.starts_with("1 passed, 1 failed"), "{t}");
    assert!(
        t.contains("FAIL inline_test.scad::test_wrong\n    @expect volume 1: expected 1, got 1084"),
        "{t}"
    );

    // format: source gives the text; a path is rewritten, or diffed with check.
    let r = s.tool("format", json!({"source": "cube( [1,2,3] ) ;"}));
    assert_eq!(text(&r), "cube([1, 2, 3]);\n");
    // Its answer is the text: a client that shows structured content in
    // place of text (Claude Code does) must not lose it.
    assert!(r.get("structuredContent").is_none(), "{r}");
    let r = s.tool("format", json!({"path": "messy.scad", "check": true}));
    assert!(text(&r).contains("+cube([1, 2, 3]);"), "{}", text(&r));
    s.tool("format", json!({"path": "messy.scad"}));
    assert_eq!(
        std::fs::read_to_string(dir.join("messy.scad")).unwrap(),
        "cube([1, 2, 3]);\n"
    );

    // docs: a builtin, and a file's definitions.
    let r = s.tool("docs", json!({"name": "cylinder"}));
    assert!(text(&r).starts_with("module cylinder("), "{}", text(&r));
    assert!(r.get("structuredContent").is_none(), "{r}");
    let r = s.tool("docs", json!({"path": "box.scad"}));
    assert!(text(&r).contains("box"), "{}", text(&r));

    // verbose: the server's whole result.
    let r = s.tool("render", json!({"path": "box.scad", "verbose": true}));
    assert!(r["structuredContent"]["timings_ms"].is_object());
}

#[test]
fn resources_serve_the_docs() {
    let dir = scratch("res");
    let mut s = Mcp::start(&dir, &[]);
    let r = s.call("resources/list", json!({}));
    assert_eq!(r["result"]["resources"][0]["uri"], "neoscad://docs");
    let r = s.call("resources/read", json!({"uri": "neoscad://docs/cube"}));
    assert!(
        r["result"]["contents"][0]["text"]
            .as_str()
            .unwrap()
            .contains("cube(")
    );
    // Not found: -32002 before 2026-07-28, -32602 since.
    let r = s.call("resources/read", json!({"uri": "neoscad://docs/nothing"}));
    assert_eq!(r["error"]["code"], -32002);
    let r = s.call(
        "resources/read",
        json!({"uri": "neoscad://docs/nothing", "_meta": modern()}),
    );
    assert_eq!(r["error"]["code"], -32602);
}

#[test]
fn file_access_stays_inside_the_roots() {
    let top = scratch("roots");
    let (inside, other, outside) = (top.join("in"), top.join("other"), top.join("out"));
    for d in [&inside, &other, &outside] {
        std::fs::create_dir(d).unwrap();
    }
    std::fs::write(outside.join("secret.scad"), "module secret() cube(7);").unwrap();
    std::fs::write(other.join("lib.scad"), "module lib() cube(3);").unwrap();
    let mut s = Mcp::start(&inside, &["--root", "../other"]);
    // Tool arguments outside the roots are refused, saying how to allow them.
    for (tool, args) in [
        ("render", json!({"path": outside.join("secret.scad")})),
        (
            "render",
            json!({"source": "cube(1);", "export": outside.join("x.stl")}),
        ),
        (
            "evaluate",
            json!({"source": "cube(1);", "base_dir": "../out"}),
        ),
        (
            "snapshot",
            json!({"source": "cube(1);", "output": "../out/s.png"}),
        ),
    ] {
        let r = s.tool(tool, args);
        assert_eq!(r["isError"], true, "{r}");
        assert!(text(&r).contains("outside the allowed roots"), "{r}");
        assert!(text(&r).contains("--root"), "{r}");
    }
    assert!(!outside.join("x.stl").exists());
    // A model cannot reach around them with include or import either.
    let r = s.tool(
        "render",
        json!({"source": format!("use <{}>\nsecret();", outside.join("secret.scad").display())}),
    );
    assert!(text(&r).contains("Can't open library"), "{}", text(&r));
    assert!(text(&r).contains("empty"), "{}", text(&r));
    // A --root is readable, and relative includes resolve against base_dir.
    let r = s.tool(
        "render",
        json!({"source": "use <lib.scad>\nlib();", "base_dir": "../other"}),
    );
    assert!(text(&r).contains("volume 27 mm³"), "{}", text(&r));
}

#[test]
fn parallel_calls_on_one_file_both_answer() {
    // An agent's parallel calls are both wanted: unlike an editor's, a
    // newer one must not cancel an older one on the same document.
    let dir = scratch("par");
    std::fs::write(dir.join("m.scad"), "sphere(10, $fn = 64);").unwrap();
    let mut s = Mcp::start(&dir, &[]);
    for (id, tool) in [(1, "check"), (2, "measure"), (3, "render")] {
        s.send(&json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
                       "params": {"name": tool, "arguments": {"path": "m.scad"}}}));
    }
    let mut seen = Vec::new();
    for _ in 0..3 {
        let r = s.read();
        assert_eq!(r["result"]["isError"], false, "{r}");
        assert!(!text(&r["result"]).contains("cancelled"), "{r}");
        seen.push(r["id"].as_u64().unwrap());
    }
    seen.sort_unstable();
    assert_eq!(seen, [1, 2, 3]);
}
