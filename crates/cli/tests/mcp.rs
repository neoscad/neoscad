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
    // Plain, not `\\?\` verbatim on Windows, as a client names files.
    lang::paths::plain(d.canonicalize().unwrap())
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

/// Whether a snapshot `r` failed only because this machine has no GPU to
/// draw with, printing why. A snapshot needs a GPU adapter and a build
/// sandbox or CI runner often has none; like the CLI's own PNG tests
/// (`tests/flags.rs`), the drawing part of a test then skips instead of
/// failing, while the parts that need no GPU still run. Any other failure
/// is left to the caller's assertions, so a real snapshot bug still fails.
fn no_gpu(r: &Value) -> bool {
    let t = text(r);
    let missing = r["isError"] == true && t.contains("GPU");
    if missing {
        eprintln!("skipped a snapshot: {t}");
    }
    missing
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
    // `test` and `format` are listed only when asked for (`--tool`).
    assert_eq!(
        names,
        ["evaluate", "render", "snapshot", "check", "measure", "docs"]
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
    let mut s = Mcp::start(&dir, &["--tool", "test", "--tool", "format"]);

    // evaluate: diagnostics with hints, and echo.
    let r = s.tool("evaluate", json!({"source": "cub(2);\necho(\"hi\", 3);"}));
    assert_eq!(r["isError"], false);
    let t = text(&r);
    assert!(t.starts_with("ok: 1 warning"), "{t}");
    assert!(
        t.contains("warning inline.scad:1:1: Ignoring unknown module 'cub' (did you mean 'cube'?)"),
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
    // Manifold sums the volume over the triangles, so the last bit
    // depends on how the cubes' faces were split (libtess2's diagonals
    // since the port: 1083.9999999999998).
    let volume = r["structuredContent"]["geometry"]["volume"]
        .as_f64()
        .unwrap();
    assert!((volume - 1084.0).abs() < 1e-9, "{volume}");
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

    // snapshot: a PNG image, the summary, and a diff. Without a GPU the
    // snapshot calls are skipped and the other tools still checked.
    let r = s.tool(
        "snapshot",
        json!({"path": "box.scad", "diff_source": "cube([20, 20, 10]);", "size": "256x256"}),
    );
    if !no_gpu(&r) {
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
    }

    // check: findings with locations and fixes.
    let r = s.tool(
        "check",
        json!({"source": "cube(10); translate([0, 0, 20]) cube(2);"}),
    );
    let t = text(&r);
    assert!(t.contains("error floating"), "{t}");
    assert!(t.contains("Fix: "), "{t}");
    assert_eq!(r["structuredContent"]["counts"]["errors"], 1);
    assert_eq!(r["structuredContent"]["exit_code"], 1);

    // Each fix once per code: later findings point back to it.
    let r = s.tool(
        "check",
        json!({"source": "cube(10); translate([30, 0, 20]) cube(2); translate([0, 30, 20]) cube(2);"}),
    );
    let f = r["structuredContent"]["findings"].as_array().unwrap();
    let floating: Vec<&Value> = f.iter().filter(|x| x["code"] == "floating").collect();
    assert_eq!(floating.len(), 2, "{r}");
    assert!(floating[0]["fix"].is_string(), "{r}");
    assert!(floating[1].get("fix").is_none(), "{r}");
    assert_eq!(floating[1]["fix_as"], floating[0]["id"], "{r}");
    assert!(text(&r).contains("Fix: as #"), "{}", text(&r));

    // measure: a section, each contour with its radii; the model's own
    // numbers are left out when a section is asked for.
    let r = s.tool("measure", json!({"path": "box.scad", "section": "z=5"}));
    let t = text(&r);
    assert!(t.contains("section z=5: area 76 mm²"), "{t}");
    assert!(t.contains("hole 324 mm²"), "{t}");
    assert!(!t.contains("model:"), "{t}");
    let sc = &r["structuredContent"];
    assert!(sc.get("model").is_none(), "{sc}");
    assert_eq!(sc["section"]["outlines"][1]["hole"], true, "{sc}");
    // A radius profile along z about the box's centre.
    let r = s.tool(
        "measure",
        json!({"path": "box.scad", "profile": [2, 8, 2], "center": [10, 10]}),
    );
    let p = &r["structuredContent"]["profile"];
    assert_eq!(p["bands"].as_array().unwrap().len(), 4, "{p}");
    assert_eq!(p["bands"][0], json!([2.0, 10.0, 14.1421]), "{p}");
    assert!(
        text(&r).contains("profile along z at [10, 10]"),
        "{}",
        text(&r)
    );
    let r = s.tool("measure", json!({"path": "box.scad", "profile": [0, 1]}));
    assert!(
        text(&r).contains("`profile` takes 3 numbers"),
        "{}",
        text(&r)
    );

    // render: two cubes sharing an edge are not manifold as a file.
    let r = s.tool(
        "render",
        json!({"source": "cube(10); translate([10, 10, 0]) cube(10);"}),
    );
    let t = text(&r);
    assert!(t.contains("NOT manifold"), "{t}");
    assert!(
        t.contains(
            "not manifold as a file: 1 edge shared by more than two faces, the first at [10, 10, 5]"
        ),
        "{t}"
    );
    let g = &r["structuredContent"]["geometry"];
    assert_eq!(g["manifold"], false, "{g}");
    assert_eq!(g["pinched"]["edges"], 1, "{g}");
    assert!(
        g["pinched"]["fix"].as_str().unwrap().contains("overlap"),
        "{g}"
    );

    // render: faces 1e-7 apart are one face in an STL, read as 32-bit
    // floats, though the solid is two clean pieces.
    let r = s.tool(
        "render",
        json!({"source": "cube(100); translate([100 + 1e-7, 0, 0]) cube(100);"}),
    );
    let t = text(&r);
    assert!(
        t.contains("not manifold as an STL: vertices a hair apart merge at 32-bit precision"),
        "{t}"
    );
    let g = &r["structuredContent"]["geometry"];
    assert_eq!(g["manifold"], true, "{g}");
    assert!(
        g["stl_precision"]["nonmanifold_edges"].as_u64().unwrap() > 0,
        "{g}"
    );
    assert_eq!(g["stl_precision"]["point"][0], 100.0, "{g}");
    assert!(
        g["stl_precision"]["fix"]
            .as_str()
            .unwrap()
            .contains("coincident"),
        "{g}"
    );

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
    // With check, how many lines would change; the diff when asked.
    let r = s.tool("format", json!({"path": "messy.scad", "check": true}));
    assert_eq!(
        text(&r),
        "not formatted: 1 line would change (diff: true shows them)"
    );
    let r = s.tool(
        "format",
        json!({"path": "messy.scad", "check": true, "diff": true}),
    );
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
    // Not found: the hint names the tool's argument; an MCP client has
    // no `--in` flag.
    let r = s.tool("docs", json!({"name": "gear"}));
    assert!(
        text(&r).ends_with("add `path` (the file that defines or includes it)"),
        "{}",
        text(&r)
    );
    // The printing recipes in the instructions: agents ask for them by
    // name, or by part of it, and were told "no builtin".
    let r = s.tool("docs", json!({"name": "snap_hook"}));
    assert!(
        text(&r).starts_with("The printing recipe snap_hook")
            && text(&r).contains("// Upright snap hook")
            && text(&r).contains("module snap_hook("),
        "{}",
        text(&r)
    );
    assert_eq!(r["isError"], false);
    let r = s.tool("docs", json!({"name": "snap"}));
    assert!(
        text(&r).starts_with("'snap' is not a builtin; the printing recipe snap_hook"),
        "{}",
        text(&r)
    );
    let r = s.tool("docs", json!({"name": "countersunk"}));
    assert!(text(&r).contains("module countersink("), "{}", text(&r));
    // A builtin keeps its own answer, and the index names the recipes.
    let r = s.tool("docs", json!({"name": "round"}));
    assert!(!text(&r).contains("recipe"), "{}", text(&r));
    let r = s.tool("docs", json!({}));
    assert!(
        text(&r).ends_with(
            "Printing recipes (ask for one by name): countersink rounded_plate fillet thread snap_hook"
        ),
        "{}",
        text(&r)
    );

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
fn inside_out_polyhedra_are_named_in_render_evaluate_and_check() {
    // A cube polyhedron with its faces counter-clockwise seen from
    // outside (OpenSCAD wants clockwise), subtracted from a slab: the
    // difference comes out with pinched edges, and the polyhedron, not
    // the pinch's usual "overlap them", is what to fix.
    let dir = scratch("inside-out");
    let src = "P = [[0,0,0],[4,0,0],[4,4,0],[0,4,0],[0,0,4],[4,0,4],[4,4,4],[0,4,4]];\n\
               difference() { cube([6,6,2]); polyhedron(P, [[3,2,1,0],[0,1,5,4],[4,5,6,7],[1,2,6,5],[2,3,7,6],[3,0,4,7]]); }\n";
    let mut s = Mcp::start(&dir, &[]);
    let r = s.tool("render", json!({"source": src}));
    let t = text(&r);
    assert!(
        t.contains(
            "warning inline.scad:2:31: this polyhedron is inside out: all 6 faces point inward"
        ),
        "{t}"
    );
    let sc = &r["structuredContent"];
    let d = &sc["diagnostics"][0];
    assert_eq!(d["code"], "polyhedron-inside-out", "{sc}");
    assert_eq!(d["severity"], "warning");
    assert_eq!(d["line"], 2);
    assert!(d["hint"].as_str().unwrap().contains("clockwise"), "{d}");
    // The pinched edges point to the polyhedron.
    let fix = sc["geometry"]["pinched"]["fix"].as_str().unwrap();
    assert!(
        fix.starts_with("fix the polyhedron-inside-out warning (line 2) first"),
        "{fix}"
    );
    assert!(t.contains(fix), "{t}");
    // evaluate says so without building geometry.
    let r = s.tool("evaluate", json!({"source": src}));
    assert_eq!(
        r["structuredContent"]["diagnostics"][0]["code"], "polyhedron-inside-out",
        "{r}"
    );
    // check lists it first.
    let r = s.tool("check", json!({"source": src}));
    let f = &r["structuredContent"]["findings"];
    assert_eq!(f[0]["code"], "polyhedron-inside-out", "{f}");
    assert!(text(&r).contains("Fix: fix #1 first"), "{}", text(&r));
    // Fixed, all of it goes away.
    let fixed = src.replace(
        "[[3,2,1,0],[0,1,5,4],[4,5,6,7],[1,2,6,5],[2,3,7,6],[3,0,4,7]]",
        "[[0,1,2,3],[4,5,1,0],[7,6,5,4],[5,6,2,1],[6,7,3,2],[7,4,0,3]]",
    );
    let r = s.tool("render", json!({"source": fixed}));
    let sc = &r["structuredContent"];
    assert_eq!(sc["diagnostics"], json!([]), "{sc}");
    assert_eq!(sc["geometry"]["manifold"], true, "{sc}");
}

#[test]
fn check_snapshot_and_measure_carry_warnings_and_echo_in_structured_content() {
    // Claude Code shows an agent the structured content in place of the
    // text, so warnings and echo only in the text are invisible to it: a
    // `check` read as warning-free and agents ran `render` as well to see
    // them.
    let dir = scratch("check-log");
    let src = "echo(a = 1);\ncub(3);\ncube(10);\n";
    let mut s = Mcp::start(&dir, &[]);
    let r = s.tool("check", json!({"source": src}));
    let sc = &r["structuredContent"];
    assert_eq!(sc["diagnostics"][0]["code"], "unknown-module", "{sc}");
    assert_eq!(sc["diagnostics"][0]["line"], 2, "{sc}");
    assert_eq!(sc["echo"], json!(["ECHO: a = 1"]), "{sc}");
    for (tool, args) in [
        (
            "snapshot",
            json!({"source": src, "views": ["iso"], "size": "64x64"}),
        ),
        ("measure", json!({"source": src})),
    ] {
        let r = s.tool(tool, args);
        assert_eq!(
            r["structuredContent"]["echo"],
            json!(["ECHO: a = 1"]),
            "{tool}: {r}"
        );
    }
    // Nothing echoed, no `echo` key: it would cost every call a few tokens.
    let r = s.tool("check", json!({"source": "cube(10);"}));
    let sc = &r["structuredContent"];
    assert!(sc.get("echo").is_none(), "{sc}");
    assert_eq!(sc["diagnostics"], json!([]), "{sc}");
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

#[test]
fn resource_limits_stop_runaway_models_fast() {
    // The agent-surface audit's finding 1: each of these asked for
    // gigabytes (a sphere at $fn=100000 reached 43 GB). With the agent
    // limits on by default, each fails at once with a `resource-limit`
    // error naming the limit and how to raise it.
    let dir = scratch("limits");
    let mut s = Mcp::start(&dir, &[]);
    for (src, limit) in [
        ("sphere(10, $fn=100000);", "fragments"),
        ("x = rands(0,1,1e9);", "rands"),
        (
            "function f(s,n) = n==0 ? s : f(str(s,s), n-1); echo(len(f(\"a\",40)));",
            "string",
        ),
        (
            "function g(v,n) = n==0 ? v : g(concat(v,v), n-1); echo(len(g([1],40)));",
            "list",
        ),
        (
            "linear_extrude(height=10, slices=100000000) square(1);",
            "slices",
        ),
        ("circle(r=1, $fn=1e9);", "fragments"),
        ("cylinder(h=1, r=1, $fn=3e8);", "fragments"),
        ("sphere(10, $fn=5000);", "triangles"),
    ] {
        let t0 = std::time::Instant::now();
        let r = s.tool("render", json!({"source": src}));
        assert!(
            t0.elapsed().as_secs_f64() < 5.0,
            "{src}: {:?}",
            t0.elapsed()
        );
        assert_eq!(r["isError"], false, "{r}");
        let st = &r["structuredContent"];
        assert_eq!(st["exit_code"], 1, "{src}: {r}");
        let d = &st["diagnostics"][0];
        assert_eq!(d["code"], "resource-limit", "{src}: {r}");
        assert!(
            d["message"]
                .as_str()
                .unwrap()
                .contains(&format!("{limit} limit")),
            "{src}: {d}"
        );
        assert!(
            d["hint"]
                .as_str()
                .unwrap()
                .contains(&format!("--limit {limit}=N")),
            "{d}"
        );
    }
    // A model under the limits is untouched.
    let r = s.tool("render", json!({"source": "sphere(10, $fn=64);"}));
    assert_eq!(r["structuredContent"]["exit_code"], 0, "{r}");
    // `--limit` raises one (and `off` removes it).
    let mut s = Mcp::start(
        &dir,
        &["--limit", "fragments=200000", "--limit", "triangles=off"],
    );
    let r = s.tool("render", json!({"source": "circle(r=1, $fn=100000);"}));
    assert_eq!(r["structuredContent"]["exit_code"], 0, "{r}");
    // Time: an evaluation that would run for minutes stops at the limit.
    let mut s = Mcp::start(&dir, &["--limit", "time=1"]);
    let t0 = std::time::Instant::now();
    let r = s.tool(
        "evaluate",
        json!({"source": "function f(n) = n == 0 ? 0 : 1 + f(n - 1);\nfor (i = [0:99999]) for (j = [0:99]) if (f(1000) < 0) cube(1);"}),
    );
    assert!(t0.elapsed().as_secs_f64() < 5.0, "{:?}", t0.elapsed());
    let d = &r["structuredContent"]["diagnostics"][0];
    assert_eq!(d["code"], "resource-limit", "{r}");
    assert!(
        d["message"].as_str().unwrap().contains("time limit of 1 s"),
        "{d}"
    );
    // Memory: a nest of lists past the budget.
    let mut s = Mcp::start(&dir, &["--limit", "memory=256M"]);
    let r = s.tool(
        "evaluate",
        json!({"source": "x = [for (i = [0:999]) [for (j = [0:99999]) j]];"}),
    );
    let d = &r["structuredContent"]["diagnostics"][0];
    assert_eq!(d["code"], "resource-limit", "{r}");
    assert!(
        d["message"]
            .as_str()
            .unwrap()
            .contains("memory limit of 256 MiB"),
        "{d}"
    );
    // A bad --limit is refused at start.
    let out = Command::new(BIN)
        .args(["mcp", "--limit", "frags=1"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown limit 'frags'"));
}

#[test]
fn the_end_of_input_cancels_running_calls_and_exits() {
    // A dead client must not leave an orphan computing (the audit's
    // finding 1): at the end of input the server cancels its calls and
    // exits. With the limits off, this render would run for minutes.
    let dir = scratch("eof");
    let mut child = Command::new(BIN)
        .args(["mcp", "--limit", "time=off", "--limit", "fragments=off"])
        .args(["--limit", "triangles=off", "--limit", "memory=off"])
        .current_dir(&dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let init = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}});
    let call = json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call",
        "params": {"name": "render", "arguments": {"source":
            "function f(n) = n == 0 ? 0 : 1 + f(n - 1);\nfor (i = [0:99999]) for (j = [0:99]) if (f(1000) < 0) cube(1);"}}});
    writeln!(stdin, "{init}\n{call}").unwrap();
    stdin.flush().unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    let t0 = std::time::Instant::now();
    drop(stdin);
    let status = loop {
        if let Some(st) = child.try_wait().unwrap() {
            break st;
        }
        if t0.elapsed().as_secs_f64() > 10.0 {
            let _ = child.kill();
            panic!("still running {:?} after the end of input", t0.elapsed());
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    assert!(status.success());
    // Well inside the 2 s grace: the call was cancelled, not cut off.
    assert!(t0.elapsed().as_secs_f64() < 1.5, "{:?}", t0.elapsed());
}

#[test]
fn writes_never_follow_links_out_or_replace_other_files() {
    // Findings 2 and 3 of the agent-surface audit.
    let top = scratch("writes");
    let (root, outside) = (top.join("R"), top.join("outside"));
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(root.join("model.scad"), "cube(2);\n").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.join("newfile.stl"), root.join("dangling.stl")).unwrap();
        std::os::unix::fs::symlink(outside.join("dangle.png"), root.join("dangle.png")).unwrap();
        std::os::unix::fs::symlink("model.scad", root.join("alias.stl")).unwrap();
    }
    let mut s = Mcp::start(&root, &[]);
    #[cfg(unix)]
    {
        let r = s.tool(
            "render",
            json!({"source": "cube(1);", "export": "dangling.stl"}),
        );
        assert_eq!(r["isError"], true, "{r}");
        assert!(text(&r).contains("outside the allowed roots"), "{r}");
        let r = s.tool(
            "snapshot",
            json!({"source": "cube(1);", "output": "dangle.png", "size": "64x64"}),
        );
        assert_eq!(r["isError"], true, "{r}");
        assert!(!outside.join("newfile.stl").exists());
        assert!(!outside.join("dangle.png").exists());
        // A link inside the root to the model: the file it leads to is
        // a .scad, so an STL never replaces it.
        let r = s.tool(
            "render",
            json!({"source": "cube(1);", "export": "alias.stl", "overwrite": true}),
        );
        assert_eq!(r["isError"], true, "{r}");
        assert!(text(&r).contains("existing .scad file"), "{r}");
    }
    // The snapshot's output is a PNG, never the model.
    let r = s.tool(
        "snapshot",
        json!({"source": "cube(1);", "output": "model.scad", "size": "64x64"}),
    );
    assert_eq!(r["isError"], true, "{r}");
    assert!(text(&r).contains("must end in .png"), "{r}");
    assert_eq!(
        std::fs::read_to_string(root.join("model.scad")).unwrap(),
        "cube(2);\n"
    );
    // An export of an unknown type is refused before any directory is made.
    let r = s.tool(
        "render",
        json!({"source": "cube(1);", "export": "new/x.txt"}),
    );
    assert_eq!(r["isError"], true, "{r}");
    assert!(!root.join("new").exists());
    // Same type: only with overwrite.
    let r = s.tool(
        "render",
        json!({"source": "cube(1);", "export": "out/a.stl"}),
    );
    assert!(text(&r).contains("wrote "), "{r}");
    let r = s.tool(
        "render",
        json!({"source": "cube(2);", "export": "out/a.stl"}),
    );
    assert_eq!(r["isError"], true, "{r}");
    assert!(text(&r).contains("overwrite: true"), "{r}");
    let r = s.tool(
        "render",
        json!({"source": "cube(2);", "export": "out/a.stl", "overwrite": true}),
    );
    assert!(text(&r).contains("wrote "), "{r}");
}

#[test]
fn bad_calls_are_explained_in_the_callers_terms() {
    let dir = scratch("args");
    let mut s = Mcp::start(&dir, &[]);
    // Wrong types are refused, naming the argument (finding 10 of the
    // audit): they were silently treated as absent.
    for (tool, args, want) in [
        (
            "check",
            json!({"source": "cube(1);", "nozzle": "big"}),
            "argument `nozzle` of check must be a number",
        ),
        (
            "render",
            json!({"source": "cube(1);", "parts": "yes"}),
            "argument `parts` of render must be a boolean",
        ),
        (
            "snapshot",
            json!({"source": "cube(1);", "views": "iso"}),
            "argument `views` of snapshot must be an array",
        ),
        (
            "measure",
            json!({"source": "cube(1);", "between": ["a", 2]}),
            "must be an array of strings",
        ),
        (
            "render",
            json!({"source": "cube(1);", "file": "x"}),
            "render has no argument `file`",
        ),
    ] {
        let r = s.tool(tool, args);
        assert_eq!(r["isError"], true, "{r}");
        assert!(text(&r).contains(want), "{}", text(&r));
    }
    // The shared parsers' messages name arguments, not command-line flags.
    for (tool, args, want) in [
        (
            "snapshot",
            json!({"source": "cube(1);", "size": "3x3"}),
            "`size` must be WxH",
        ),
        (
            "check",
            json!({"source": "cube(1);", "max_overhang": 400}),
            "`max_overhang` must be",
        ),
        (
            "check",
            json!({"source": "cube(1);", "bed": [1, 2]}),
            "bed must be",
        ),
        (
            "measure",
            json!({"source": "cube(1);", "between": ["a"]}),
            "`between` takes two part names",
        ),
    ] {
        let r = s.tool(tool, args);
        assert!(!text(&r).contains("--"), "{}", text(&r));
        assert!(text(&r).contains(want), "{}", text(&r));
    }
    let r = s.tool("measure", json!({"source": "cube(1);", "part": "a"}));
    assert!(text(&r).contains("`parts: true`"), "{}", text(&r));
    // A missing file says so, with a stable code (finding 5), and check
    // has an exit code.
    let r = s.tool("render", json!({"path": "nope.scad"}));
    let d = &r["structuredContent"]["diagnostics"][0];
    assert_eq!(d["code"], "input-not-found", "{r}");
    assert!(text(&r).contains("Can't open input file"), "{r}");
    let r = s.tool("check", json!({"path": "nope.scad"}));
    assert_eq!(r["structuredContent"]["exit_code"], 1, "{r}");
    assert_eq!(r["structuredContent"]["counts"]["errors"], 1, "{r}");
    assert_eq!(
        r["structuredContent"]["diagnostics"][0]["code"],
        "input-not-found"
    );
    // A syntax error has its column, and the hint names the token (finding 6).
    let r = s.tool("evaluate", json!({"source": "rotate(45 cube(3);"}));
    let d = &r["structuredContent"]["diagnostics"][0];
    assert_eq!(d["code"], "syntax-error");
    assert_eq!(d["column"], 11, "{d}");
    assert!(text(&r).contains("inline.scad:1:11"), "{}", text(&r));
    assert!(
        d["hint"]
            .as_str()
            .unwrap()
            .starts_with("unexpected `cube` at line 1, column 11"),
        "{d}"
    );
    let r = s.tool(
        "evaluate",
        json!({"source": "use &lt;x.scad&gt;\ncube(1);"}),
    );
    let d = &r["structuredContent"]["diagnostics"][0];
    assert!(d["hint"].as_str().unwrap().contains("HTML-escaped"), "{d}");
}

#[test]
fn library_indexes_are_short_by_default() {
    // Finding 9: a file's index named every library file by a long `../`
    // path and listed every `_private` helper (21 KB for a BOSL2 model).
    let top = scratch("docidx");
    let (work, lib) = (top.join("work"), top.join("lib"));
    std::fs::create_dir_all(lib.join("big")).unwrap();
    std::fs::create_dir(&work).unwrap();
    let mut big = String::new();
    for i in 0..150 {
        big.push_str(&format!(
            "module part{i}() cube({i});\nfunction _helper{i}() = {i};\n"
        ));
    }
    std::fs::write(lib.join("big/all.scad"), big).unwrap();
    std::fs::write(
        work.join("m.scad"),
        "include <big/all.scad>\nmodule mine() part1();\n",
    )
    .unwrap();
    let mut child = Command::new(BIN)
        .args(["mcp", "--root", "../lib"])
        .current_dir(&work)
        .env("OPENSCADPATH", &lib)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut s = Mcp {
        stdin: child.stdin.take().unwrap(),
        stdout: BufReader::new(child.stdout.take().unwrap()),
        child,
        next: 1,
    };
    let r = s.tool("docs", json!({"path": "m.scad"}));
    let t = text(&r);
    assert!(t.contains("m.scad: mine"), "{t}");
    assert!(
        t.contains("150 more from includes and uses, by file: big/all.scad (150)"),
        "{t}"
    );
    assert!(!t.contains("_helper") && !t.contains("../"), "{t}");
    assert!(t.len() < 400, "{t}");
    let r = s.tool("docs", json!({"path": "m.scad", "verbose": true}));
    let t = text(&r);
    assert!(t.contains("big/all.scad: part0 _helper0()"), "{t}");
}

#[test]
fn enable_turns_on_openscads_experimental_features_for_every_call() {
    // Off by default, as in OpenSCAD: the call warns and gives undef.
    let dir = scratch("enable-off");
    let mut s = Mcp::start(&dir, &[]);
    let r = s.tool("evaluate", json!({"source": "echo(object(a = 1));"}));
    let t = text(&r);
    assert!(
        t.contains("Experimental builtin function 'object' is not enabled"),
        "{t}"
    );
    // `neoscad mcp --enable ...` turns them on for every call.
    let dir = scratch("enable-on");
    let mut s = Mcp::start(
        &dir,
        &["--enable", "object-function", "--enable", "vector-swizzle"],
    );
    let r = s.tool(
        "evaluate",
        json!({"source": "echo(object(a = 1), [1, 2, 3].zy);"}),
    );
    assert!(
        text(&r).contains("ECHO: { a = 1; }, [3, 2]"),
        "{}",
        text(&r)
    );
}

/// Findings from the T2 transcript audit, end to end: a mesh `path` is
/// imported rather than parsed, an info-level `stl-precision` finding
/// carries no fix in a terse result, and a zero-volume intersection says
/// the parts only touch.
#[test]
fn mesh_paths_quiet_info_and_touching_parts() {
    let dir = scratch("t2-audit");
    let mut s = Mcp::start(&dir, &[]);
    // Export a mesh, then give each model tool its path.
    std::fs::create_dir_all(dir.join("out")).unwrap();
    let r = s.tool(
        "render",
        json!({"source": "cube([10, 20, 5]);", "export": "out/box.stl"}),
    );
    assert_eq!(r["isError"], false, "{r}");
    for tool in ["render", "check", "measure", "snapshot"] {
        let r = s.tool(tool, json!({"path": "out/box.stl"}));
        if tool == "snapshot" && no_gpu(&r) {
            continue;
        }
        assert_eq!(r["isError"], false, "{tool}: {r}");
        let t = text(&r);
        assert!(
            t.starts_with("path is a mesh file: rendered as `import(\"out/box.stl\");`"),
            "{tool}: {t}"
        );
        assert_eq!(
            r["structuredContent"]["imported"], "import(\"out/box.stl\");",
            "{tool}: {r}"
        );
    }
    let r = s.tool("render", json!({"path": "out/box.stl"}));
    assert_eq!(r["structuredContent"]["geometry"]["volume"], 1000.0, "{r}");
    // Upper case, and verbose's full JSON, are labelled too. Another name,
    // not `BOX.STL`: on a case-insensitive file system (Windows, macOS by
    // default) that is `box.stl` itself, and copying a file onto itself
    // fails on Windows with a sharing violation (os error 32).
    std::fs::copy(dir.join("out/box.stl"), dir.join("out/UPPER.STL")).unwrap();
    let r = s.tool("check", json!({"path": "out/UPPER.STL", "verbose": true}));
    assert_eq!(
        r["structuredContent"]["imported"],
        "import(\"out/UPPER.STL\");"
    );
    // A .scad path is still a model.
    std::fs::write(dir.join("m.scad"), "cube(1);\n").unwrap();
    let r = s.tool("render", json!({"path": "m.scad"}));
    assert!(r["structuredContent"].get("imported").is_none(), "{r}");

    // Faces that only collapse at STL precision: info, no action, no fix
    // in the terse result (verbose keeps it).
    let src = "cube(10); translate([0,0,10-1e-7]) cube([5,5,5]);";
    let r = s.tool("check", json!({"source": src}));
    let f = &r["structuredContent"]["findings"][0];
    assert_eq!(f["code"], "stl-precision", "{r}");
    assert_eq!(f["severity"], "info", "{r}");
    assert!(f.get("fix").is_none() && f.get("fix_as").is_none(), "{f}");
    let t = text(&r);
    assert!(t.contains("so no action is needed"), "{t}");
    assert!(!t.contains("Fix:"), "{t}");
    let r = s.tool("check", json!({"source": src, "verbose": true}));
    let f = &r["structuredContent"]["findings"][0];
    assert!(f["fix"].as_str().unwrap().contains("coincident"), "{f}");

    // A plug seated in its hole: the intersection is the faces where they
    // touch, with no volume. The pinch's fix says so, instead of "overlap
    // them".
    let plug = "intersection() {\n\
        difference() { cube([10, 10, 5]); translate([2, 2, 2]) cube([6, 6, 5]); }\n\
        translate([2, 2, 2]) cube([6, 6, 6]);\n}\n";
    let r = s.tool("render", json!({"source": plug}));
    let g = &r["structuredContent"]["geometry"];
    assert_eq!(g["volume"], 0.0, "{g}");
    let fix = g["pinched"]["fix"].as_str().unwrap();
    assert!(
        fix.starts_with("the parts only touch (no overlap)"),
        "{fix}"
    );
    assert!(text(&r).contains(fix), "{}", text(&r));
    // A real part's pinch keeps the usual advice.
    let r = s.tool(
        "render",
        json!({"source": "cube(10); translate([10,10,0]) cube(10);"}),
    );
    let fix = r["structuredContent"]["geometry"]["pinched"]["fix"]
        .as_str()
        .unwrap();
    assert!(fix.starts_with("two parts touch along an edge"), "{fix}");
}

#[test]
fn test_and_format_are_listed_only_when_asked_for() {
    let dir = scratch("optin");
    let mut s = Mcp::start(&dir, &[]);
    let r = s.call(
        "tools/call",
        json!({"name": "format", "arguments": {"source": "cube(1);"}}),
    );
    assert_eq!(r["error"]["code"], -32602, "{r}");
    let mut s = Mcp::start(&dir, &["--tool", "test"]);
    let r = s.call("tools/list", json!({}));
    let names: Vec<&str> = r["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(
        names.contains(&"test") && !names.contains(&"format"),
        "{names:?}"
    );
    // A name that is not an optional tool stops the server with the choices.
    let out = Command::new(BIN)
        .args(["mcp", "--tool", "evaluate"])
        .current_dir(&dir)
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("choose from test, format"), "{err}");
}

#[test]
fn an_export_is_read_back() {
    let dir = scratch("readback");
    std::fs::write(
        dir.join("b.scad"),
        "translate([0, 0, 2]) cube([10, 10, 5]);",
    )
    .unwrap();
    let mut s = Mcp::start(&dir, &[]);
    for (file, triangles) in [("out/b.stl", 12), ("out/b.3mf", 12), ("out/b.off", 12)] {
        let r = s.tool("render", json!({"path": "b.scad", "export": file}));
        let t = text(&r);
        assert!(
            t.contains(&format!(
                "; read back: {triangles} triangles, watertight, z 2 to 7"
            )),
            "{t}"
        );
        let b = &r["structuredContent"]["read_back"];
        assert_eq!(b["watertight"], true, "{b}");
        assert_eq!(b["z"][0].as_f64(), Some(2.0), "{b}");
        assert_eq!(b["z"][1].as_f64(), Some(7.0), "{b}");
    }
    // A file that does not close reads back as such: an STL of an open
    // polyhedron (one face of a cube left out).
    let r = s.tool(
        "render",
        json!({"source": "polyhedron([[0,0,0],[1,0,0],[1,1,0],[0,1,0],[0,0,1],[1,0,1],[1,1,1],[0,1,1]], [[0,1,2,3],[4,5,1,0],[7,6,5,4],[5,6,2,1],[6,7,3,2]]);",
               "export": "out/open.stl"}),
    );
    let t = text(&r);
    assert!(t.contains("NOT watertight (4 open edges)"), "{t}");
    assert_eq!(r["structuredContent"]["read_back"]["open_edges"], 4);
}

#[test]
fn check_can_export_and_measure_in_one_call() {
    let dir = scratch("compound");
    std::fs::write(
        dir.join("box.scad"),
        "difference() { cube([20, 20, 10]); translate([2, 2, 2]) cube([16, 16, 10]); }",
    )
    .unwrap();
    let mut s = Mcp::start(&dir, &[]);
    // Plain: as small as before, none of the extras.
    let plain = s.tool("check", json!({"path": "box.scad", "min_wall": 1.2}));
    let p = &plain["structuredContent"];
    for k in ["export", "sections"] {
        assert!(p.get(k).is_none(), "{p}");
    }
    assert_eq!(plain["content"].as_array().unwrap().len(), 1);
    let r = s.tool(
        "check",
        json!({"path": "box.scad", "min_wall": 1.2, "export": "out/box.stl",
               "sections": ["z=5", "z=50"]}),
    );
    assert_eq!(r["isError"], false, "{r}");
    let t = text(&r);
    // The plain check's text comes first, unchanged.
    assert!(t.starts_with(&text(&plain)), "{t}");
    assert!(t.contains("read back: "), "{t}");
    assert!(t.contains("section z=5: area 144 mm²"), "{t}");
    assert!(t.contains("section z=50: nothing to cut"), "{t}");
    let sc = &r["structuredContent"];
    assert_eq!(sc["export"]["read_back"]["watertight"], true, "{sc}");
    assert_eq!(sc["sections"][0]["area"].as_f64(), Some(144.0), "{sc}");
    assert!(dir.join("out/box.stl").exists());
    // A picture stays `snapshot`'s own call.
    assert_eq!(r["content"].as_array().unwrap().len(), 1, "{r}");
    // An export that would replace a file without `overwrite` is refused
    // before the check runs.
    let r = s.tool(
        "check",
        json!({"path": "box.scad", "export": "out/box.stl"}),
    );
    assert_eq!(r["isError"], true, "{r}");
    assert!(text(&r).contains("overwrite: true"), "{r}");
    let r = s.tool(
        "check",
        json!({"path": "box.scad", "export": "out/box.stl", "overwrite": true}),
    );
    assert!(text(&r).contains("wrote "), "{}", text(&r));
}

#[test]
fn a_program_that_stops_short_says_so() {
    let dir = scratch("eof");
    let mut s = Mcp::start(&dir, &[]);
    let r = s.tool("evaluate", json!({"source": "cube(1"}));
    let h = r["structuredContent"]["diagnostics"][0]["hint"]
        .as_str()
        .unwrap_or("");
    assert!(
        h.starts_with("unexpected end of input at line 1, column 7"),
        "{r}"
    );
}

#[test]
fn the_recipes_are_in_the_instructions_and_each_one_prints() {
    let dir = scratch("recipes");
    let mut s = Mcp::start(&dir, &[]);
    let r = s.call(
        "initialize",
        json!({"protocolVersion": "2025-11-25", "capabilities": {}}),
    );
    let instructions = r["result"]["instructions"].as_str().unwrap().to_string();
    let r = s.call("resources/read", json!({"uri": "neoscad://recipes"}));
    let recipes = r["result"]["contents"][0]["text"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(instructions.ends_with(&recipes), "{instructions}");
    assert!(recipes.contains("module thread("), "{recipes}");
    assert!(recipes.contains("module snap_hook("), "{recipes}");
    // Claude Code cuts a server's instructions after 2,048 characters,
    // which once dropped the last recipe: all of them must arrive.
    assert!(
        instructions.encode_utf16().count() <= 2048,
        "the instructions are {} UTF-16 units",
        instructions.encode_utf16().count()
    );
    // Every session pays for them.
    assert!(
        recipes.len() < 2000,
        "the recipes are {} bytes",
        recipes.len()
    );
    // Each module, used as its comment says, renders to one sound solid
    // that check passes with no warnings: what an agent copies must not
    // send it chasing findings. The thread's flanks are info.
    for (name, call) in [
        (
            "countersink",
            "difference() { cube([20, 20, 4]); translate([10, 10, 0]) countersink(4); }",
        ),
        ("rounded_plate", "rounded_plate(40, 30, 4);"),
        (
            "fillet",
            "cube([40, 30, 4]); cube([40, 4, 30]); translate([0, 4 - 0.01, 4 - 0.01]) fillet(4, 40);",
        ),
        (
            "thread",
            "cylinder(d = 30, h = 2, $fn = 6); translate([0, 0, 2 - 0.01]) thread(24, 2, 12);",
        ),
        (
            "snap_hook",
            "cube([20, 6, 2]); translate([7, 0, 2]) snap_hook();",
        ),
    ] {
        let src = format!("{recipes}\n{call}\n");
        let r = s.tool("check", json!({"source": src, "min_wall": 1.2}));
        let sc = &r["structuredContent"];
        assert_eq!(sc["model"]["manifold"], true, "{name}: {}", text(&r));
        assert_eq!(sc["model"]["components"], 1, "{name}: {}", text(&r));
        assert_eq!(sc["counts"]["errors"], 0, "{name}: {}", text(&r));
        assert_eq!(sc["counts"]["warnings"], 0, "{name}: {}", text(&r));
        assert!(
            sc["diagnostics"].as_array().unwrap().is_empty(),
            "{name}: {}",
            text(&r)
        );
        if name == "thread" {
            assert!(
                text(&r).contains("info overhang: thread flanks"),
                "{}",
                text(&r)
            );
        }
    }
}
