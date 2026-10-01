//! `part()`, `check` and `measure` on synthetic models whose answers are
//! known: a wall of a given thickness, a slab overhanging a post, a
//! floating cube, parts at known distances.

use std::path::PathBuf;
use std::sync::Arc;

use lang::loader::LibraryPath;
use lang::vfs::MemFs;
use serde_json::Value;
use session::check::{CheckRequest, CheckSettings};
use session::measure::{MeasureRequest, Plane};
use session::{Config, Run, Session};

fn session(files: &[(&str, &str)]) -> Session {
    let fs = Arc::new(MemFs::new());
    for (p, t) in files {
        fs.insert(format!("/doc/{p}"), t.as_bytes().to_vec());
    }
    let mut cfg = Config::new(fs, LibraryPath(Vec::new()));
    cfg.work_dir = PathBuf::from("/doc");
    // Bounded like an agent's calls: a model here that ran away would
    // stop with a diagnostic, not fill the machine's memory.
    cfg.limits = session::Limits::AGENT;
    Session::new(cfg)
}

fn check(src: &str, parts: bool, settings: CheckSettings) -> Value {
    let s = session(&[("m.scad", src)]);
    let mut run = Run::new("m.scad");
    run.parts = parts;
    let c = s.check(&CheckRequest { run, settings }).unwrap();
    c.summary
}

fn findings<'a>(v: &'a Value, code: &str) -> Vec<&'a Value> {
    v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["code"] == code)
        .collect()
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

#[test]
fn thin_walls_are_found_and_measured_exactly() {
    // A 0.5 mm wall (under the 0.8 mm minimum) and a 0.3 mm one (under
    // the 0.4 mm nozzle) on a 3 mm base.
    let v = check(
        "cube([20,20,3]);\n\
         translate([0,0,3]) cube([20,0.5,10]);\n\
         translate([0,10,3]) cube([20,0.3,10]);\n",
        false,
        CheckSettings::default(),
    );
    let thin = findings(&v, "thin-wall");
    assert_eq!(thin.len(), 2, "{v}");
    assert_eq!(thin[0]["severity"], "error");
    assert!(close(thin[0]["value"].as_f64().unwrap(), 0.3, 1e-4), "{v}");
    assert_eq!(thin[1]["severity"], "warning");
    assert!(close(thin[1]["value"].as_f64().unwrap(), 0.5, 1e-4), "{v}");
    // Located on the wall.
    let bb = &thin[0]["location"]["bbox"];
    assert!(close(bb["min"][1].as_f64().unwrap(), 10.0, 1e-4), "{bb}");
    assert!(close(bb["max"][1].as_f64().unwrap(), 10.3, 1e-4), "{bb}");
    assert!(close(
        v["model"]["min_wall"]["thickness"].as_f64().unwrap(),
        0.3,
        1e-4
    ));
    assert_eq!(v["ok"], false);
    assert_eq!(v["exit_code"], 1);
    // A thick model has no thin walls.
    let ok = check("cube(10);", false, CheckSettings::default());
    assert!(findings(&ok, "thin-wall").is_empty(), "{ok}");
    assert_eq!(ok["ok"], true);
    assert_eq!(ok["counts"]["errors"], 0);
}

#[test]
fn overhangs_are_measured_and_bed_contact_is_not_one() {
    // A 30 x 10 slab on a 10 x 10 post: 20 x 10 of its underside hangs.
    let v = check(
        "cube([10,10,10]); translate([0,0,10]) cube([30,10,2]);",
        false,
        CheckSettings::default(),
    );
    let o = findings(&v, "overhang");
    assert_eq!(o.len(), 1, "{v}");
    assert!(close(o[0]["value"].as_f64().unwrap(), 200.0, 1e-6), "{v}");
    assert!(o[0]["message"].as_str().unwrap().contains("90°"), "{v}");
    assert!(close(
        v["model"]["overhang_area"].as_f64().unwrap(),
        200.0,
        1e-6
    ));
    // A 45° chamfer is printable at the default limit, and not at 30°.
    let wedge = "rotate([90,0,0]) linear_extrude(10) polygon([[0,0],[10,0],[20,10],[0,10]]);";
    let ok = check(wedge, false, CheckSettings::default());
    assert!(findings(&ok, "overhang").is_empty(), "{ok}");
    let strict = CheckSettings {
        max_overhang: 30.0,
        ..CheckSettings::default()
    };
    let bad = check(wedge, false, strict);
    let o = findings(&bad, "overhang");
    assert_eq!(o.len(), 1, "{bad}");
    // The slope is 10 * sqrt(2) by 10.
    assert!(
        close(o[0]["value"].as_f64().unwrap(), 100.0 * 2f64.sqrt(), 1e-4),
        "{bad}"
    );
}

#[test]
fn a_floating_island_is_an_error() {
    let v = check(
        "cube(10); translate([20,0,5]) cube(5);",
        false,
        CheckSettings::default(),
    );
    let f = findings(&v, "floating");
    assert_eq!(f.len(), 1, "{v}");
    assert!(close(f[0]["value"].as_f64().unwrap(), 5.0, 1e-9));
    assert_eq!(v["model"]["components"], 2);
    assert_eq!(v["model"]["floating"], 1);
    assert_eq!(v["counts"]["errors"], 1);
}

/// A sealed hollow's inner surface is a shell of its own: it used to be
/// reported as a piece floating 0.5 mm above the bed.
#[test]
fn a_sealed_cavity_is_not_a_floating_piece() {
    let v = check(
        "difference() { cube(20); translate([.5,.5,.5]) cube(19); }",
        false,
        CheckSettings::default(),
    );
    assert!(findings(&v, "floating").is_empty(), "{v}");
    assert!(findings(&v, "tiny-feature").is_empty(), "{v}");
    let c = findings(&v, "cavity");
    assert_eq!(c.len(), 1, "{v}");
    assert_eq!(c[0]["severity"], "info");
    assert!(close(
        c[0]["value"].as_f64().unwrap(),
        19.0 * 19.0 * 19.0,
        1e-6
    ));
    assert_eq!(
        c[0]["location"]["bbox"]["min"],
        serde_json::json!([0.5, 0.5, 0.5]),
        "{v}"
    );
    assert_eq!(v["model"]["components"], 2);
    assert_eq!(v["model"]["floating"], 0);
    assert_eq!(v["model"]["cavities"], 1);
    assert_eq!(v["counts"]["errors"], 0, "{v}");
    // The model's volume is the shell's: the void is subtracted.
    let vol = v["model"]["volume"].as_f64().unwrap();
    assert!(close(vol, 8000.0 - 6859.0, 1e-6), "{vol}");
}

/// A piece inside a hollow is still a piece: a ball sealed in a box floats
/// (it rests on nothing it is joined to), and the void around it is a
/// cavity.
#[test]
fn a_piece_inside_a_cavity_still_floats() {
    let v = check(
        "difference() { cube(20); translate([1,1,1]) cube(18); }
         translate([10,10,8]) cube(4, center = true);",
        false,
        CheckSettings::default(),
    );
    assert_eq!(findings(&v, "cavity").len(), 1, "{v}");
    let f = findings(&v, "floating");
    assert_eq!(f.len(), 1, "{v}");
    assert!(close(f[0]["value"].as_f64().unwrap(), 6.0, 1e-9), "{v}");
    // What is under it is the cavity's floor, 5 mm down.
    assert!(
        f[0]["message"]
            .as_str()
            .unwrap()
            .contains("5 mm above the piece under it"),
        "{v}"
    );
    assert_eq!(v["model"]["components"], 3);
}

/// Two closed boxes as one polyhedron (no boolean merges them) whose
/// facing sides overlap by `overlap` mm along x: coils of a spring that
/// fuse within Manifold's tolerance keep both surfaces the same way.
fn two_boxes(overlap: f64) -> String {
    let b = |x0: f64, x1: f64| {
        [
            [x0, 0.0, 0.0],
            [x1, 0.0, 0.0],
            [x1, 10.0, 0.0],
            [x0, 10.0, 0.0],
            [x0, 0.0, 10.0],
            [x1, 0.0, 10.0],
            [x1, 10.0, 10.0],
            [x0, 10.0, 10.0],
        ]
    };
    let pts: Vec<String> = b(0.0, 10.0)
        .iter()
        .chain(b(10.0 - overlap, 20.0).iter())
        .map(|p| format!("[{},{},{}]", p[0], p[1], p[2]))
        .collect();
    let cube = [
        [0, 1, 2, 3],
        [4, 5, 1, 0],
        [7, 6, 5, 4],
        [5, 6, 2, 1],
        [6, 7, 3, 2],
        [7, 4, 0, 3],
    ];
    let faces: Vec<String> = [0, 8]
        .iter()
        .flat_map(|o| {
            cube.iter()
                .map(move |f| format!("[{}]", f.map(|i| (i + o).to_string()).join(",")))
        })
        .collect();
    format!(
        "polyhedron(points=[{}], faces=[{}]);",
        pts.join(","),
        faces.join(",")
    )
}

#[test]
fn touching_surfaces_are_a_contact_not_a_thin_wall() {
    // The agent-surface audit's spring_handle: coincident opposing faces
    // measured as "0 mm" thin-wall errors with the fix "thicken it".
    let v = check(&two_boxes(0.0002), false, CheckSettings::default());
    assert!(findings(&v, "thin-wall").is_empty(), "{v}");
    let t = findings(&v, "touching-surfaces");
    assert_eq!(t.len(), 1, "{v}");
    assert_eq!(t[0]["severity"], "info");
    assert!(t[0]["fix"].as_str().unwrap().contains("gap"), "{v}");
    // The walls measure what they are: 10 mm through each box.
    assert!(
        v["model"]["min_wall"]["thickness"].as_f64().unwrap() > 9.0,
        "{v}"
    );
    assert_eq!(v["counts"]["errors"], 0, "{v}");
}

#[test]
fn a_resting_piece_is_not_said_to_have_nothing_under_it() {
    let lid = |z: f64| {
        format!(
            "difference() {{ cube([20,20,10]); translate([2,2,2]) cube([16,16,10]); }}\n\
             translate([0,0,{z}]) cube([20,20,2]);"
        )
    };
    let resting = check(&lid(10.01), false, CheckSettings::default());
    let f = findings(&resting, "floating");
    assert_eq!(f.len(), 1, "{resting}");
    let m = f[0]["message"].as_str().unwrap();
    assert!(m.contains("resting on another piece"), "{m}");
    let above = check(&lid(13.0), false, CheckSettings::default());
    let m = findings(&above, "floating")[0]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(m.contains("3 mm above the piece under it"), "{m}");
    let alone = check(
        "cube(5); translate([20,0,5]) cube(5);",
        false,
        CheckSettings::default(),
    );
    let m = findings(&alone, "floating")[0]["message"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(m.contains("nothing under it"), "{m}");
}

#[test]
fn bed_fit_and_tiny_features() {
    let s = CheckSettings {
        bed: Some([100.0, 50.0, 50.0]),
        ..CheckSettings::default()
    };
    // Fits only turned 90° about z.
    let v = check("cube([40,80,10]);", false, s);
    let b = findings(&v, "bed-fit");
    assert_eq!(b.len(), 1, "{v}");
    assert_eq!(b[0]["severity"], "warning");
    let v = check("cube([40,80,60]);", false, s);
    assert_eq!(findings(&v, "bed-fit")[0]["severity"], "error");
    let v = check("cube(10); translate([20,0,0]) cube(0.3);", false, s);
    assert_eq!(findings(&v, "tiny-feature").len(), 1, "{v}");
}

#[test]
fn open_meshes_and_2d_models_are_reported() {
    let v = check(
        "polyhedron([[0,0,0],[10,0,0],[0,10,0],[0,0,10]], [[0,1,2],[0,3,1],[0,2,3]]);",
        false,
        CheckSettings::default(),
    );
    assert_eq!(findings(&v, "not-closed").len(), 1, "{v}");
    let v = check("square(10);", false, CheckSettings::default());
    assert_eq!(findings(&v, "not-3d").len(), 1, "{v}");
    let v = check("cube(0);", false, CheckSettings::default());
    assert_eq!(findings(&v, "empty").len(), 1, "{v}");
}

#[test]
fn findings_name_their_parts_and_parts_intersect() {
    let src = "part(\"body\") cube([20,20,10]);\n\
               part(\"fin\") translate([25,0,0]) cube([10,0.5,10]);\n\
               part(\"pin\") translate([18,5,2]) cube([4,2,2]);\n";
    let v = check(src, true, CheckSettings::default());
    let thin = findings(&v, "thin-wall");
    assert_eq!(thin.len(), 1, "{v}");
    assert_eq!(thin[0]["part"], "fin");
    let x = findings(&v, "parts-intersect");
    assert_eq!(x.len(), 1, "{v}");
    // The pin overlaps the body by 2 x 2 x 2.
    assert!(close(x[0]["value"].as_f64().unwrap(), 8.0, 1e-6), "{v}");
    let names: Vec<&str> = v["parts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["body", "fin", "pin"]);
    // The same model with parts off: an unknown module, and no geometry.
    let off = check(src, false, CheckSettings::default());
    assert_eq!(findings(&off, "empty").len(), 1, "{off}");
    assert_eq!(off["diagnostics"]["warnings"], 3, "{off}");
}

fn measure(src: &str, f: impl FnOnce(&mut MeasureRequest)) -> Value {
    let s = session(&[("m.scad", src)]);
    let mut run = Run::new("m.scad");
    run.parts = true;
    let mut req = MeasureRequest::new(run);
    f(&mut req);
    let m = s.measure(&req).unwrap();
    m.summary
}

#[test]
fn parts_are_measured_on_their_own() {
    let src = "part(\"a\") cube(10);\n\
               translate([13,0,0]) part(\"b\") cube([2,4,6]);\n\
               part(\"c\") translate([5,5,9]) cube(2);\n";
    let v = measure(src, |r| r.between = Some(("a".into(), "b".into())));
    let parts = v["parts"].as_array().unwrap();
    assert!(close(parts[0]["volume"].as_f64().unwrap(), 1000.0, 1e-9));
    let b = &parts[1];
    assert!(close(b["volume"].as_f64().unwrap(), 48.0, 1e-9));
    assert_eq!(b["centroid"], serde_json::json!([14.0, 2.0, 3.0]));
    assert!(
        close(v["between"]["distance"].as_f64().unwrap(), 3.0, 1e-9),
        "{v}"
    );
    assert_eq!(v["between"]["touching"], false);
    // c sits 1 mm into a: an overlap of 2 x 2 x 1.
    let v = measure(src, |r| r.between = Some(("a".into(), "c".into())));
    assert_eq!(v["between"]["overlapping"], true, "{v}");
    assert!(close(
        v["between"]["overlap_volume"].as_f64().unwrap(),
        4.0,
        1e-6
    ));
    // Touching faces.
    let v = measure(
        "part(\"a\") cube(1); part(\"b\") translate([1,0,0]) cube(1);",
        |r| r.between = Some(("a".into(), "b".into())),
    );
    assert_eq!(v["between"]["touching"], true, "{v}");
    assert_eq!(v["between"]["overlapping"], false, "{v}");
    // An unknown part is an error that lists the parts.
    let v = measure(src, |r| r.part = Some("z".into()));
    assert_eq!(v["failed"], true);
    assert!(v["error"].as_str().unwrap().contains("a, b, c"), "{v}");
}

#[test]
fn sections_cut_at_axis_planes() {
    // A 20 x 10 x 5 box with a 4 x 4 hole through it in z.
    let src = "difference() { cube([20,10,5]); translate([8,3,-1]) cube([4,4,7]); }";
    let v = measure(src, |r| r.section = Some(Plane::Z(2.0)));
    let s = &v["section"];
    assert!(
        close(s["area"].as_f64().unwrap(), 200.0 - 16.0, 1e-9),
        "{s}"
    );
    assert!(
        close(s["perimeter"].as_f64().unwrap(), 60.0 + 16.0, 1e-9),
        "{s}"
    );
    assert_eq!(s["contours"], 2);
    let v = measure(src, |r| r.section = Some(Plane::X(10.0)));
    let s = &v["section"];
    // Across the hole: two 3 x 5 strips.
    assert!(close(s["area"].as_f64().unwrap(), 30.0, 1e-9), "{s}");
    assert_eq!(s["contours"], 2);
    assert_eq!(s["bbox"]["min"][0], 10.0);
    let v = measure(src, |r| r.section = Some(Plane::Y(5.0)));
    assert!(close(v["section"]["area"].as_f64().unwrap(), 80.0, 1e-9));
    let v = measure(src, |r| r.section = Some(Plane::Z(50.0)));
    assert_eq!(v["section"]["contours"], 0);
}

#[test]
fn checks_are_deterministic() {
    let src = "part(\"a\") sphere(10, $fn=40); part(\"b\") translate([15,0,0]) cylinder(r=3,h=0.6,$fn=30);";
    let strip = |mut v: Value| {
        v.as_object_mut().unwrap().remove("timings_ms");
        v
    };
    let a = strip(check(src, true, CheckSettings::default()));
    let b = strip(check(src, true, CheckSettings::default()));
    assert_eq!(a, b);
}

/// The agent-eval pilot's T3 part (an M24x2 hose-barb adapter; the final
/// source of `cad-20260928T202850Z/T3-neoscad-1`): a swept thread rib whose
/// end touches the rim where the core cylinder meets the flange's cone.
const T3: &str = include_str!("data/pilot_t3_adapter.scad");

/// The T3 part of agent-eval run `cad-20260929T031249Z/T3-neoscad-1`:
/// an intermediate source with a 41 mm² ledge at z = 11.9 among 60°
/// thread flanks, and the final one, whose barb tapers to a 1.2 mm rim.
const T3_LEDGE: &str = include_str!("data/t3_ledge.scad");
const T3_ADAPTER: &str = include_str!("data/t3_adapter.scad");

#[test]
fn an_overhang_points_at_its_steepest_faces() {
    // "553.74 mm² at up to 90°" pointed at a 60° flank at z = 6.7; the
    // agent swept `max_overhang` to find the ledge.
    let spec = CheckSettings {
        min_wall: 1.2,
        ..CheckSettings::default()
    };
    let v = check(T3_LEDGE, false, spec);
    let o = findings(&v, "overhang");
    assert_eq!(o.len(), 1, "{v}");
    let z = o[0]["location"]["point"][2].as_f64().unwrap();
    assert!(close(z, 11.9, 1e-3), "{v}");
    let m = o[0]["message"].as_str().unwrap();
    assert!(m.contains("up to 90°"), "{m}");
    assert!(m.contains("z 0 to 11.94"), "{m}");
    assert!(
        m.contains("41.1 mm² of it steeper than 60° (z 11.9)"),
        "{m}"
    );
    // The final part's flanks are 60°, none steeper: no steep note.
    let v = check(T3_ADAPTER, false, spec);
    let o = findings(&v, "overhang");
    assert_eq!(o.len(), 1, "{v}");
    let m = o[0]["message"].as_str().unwrap();
    assert!(m.contains("up to 60°") && !m.contains("steeper"), "{m}");
    // The pilot's part said "up to 88°" of 0.06 mm² of slivers where its
    // thread meets the chamfer; its flanks are 60°.
    let v = check(T3, false, spec);
    let o = findings(&v, "overhang");
    assert_eq!(o.len(), 1, "{v}");
    let m = o[0]["message"].as_str().unwrap();
    assert!(m.contains("up to 60°") && !m.contains("steeper"), "{m}");
}

#[test]
fn a_tapered_rim_is_measured_near_its_edge() {
    let wall = |src: &str, spec: CheckSettings| {
        let v = check(src, false, spec);
        assert_eq!(v["model"]["min_wall"]["sampled"], true, "{v}");
        v["model"]["min_wall"]["thickness"].as_f64().unwrap()
    };
    // A cone's rim 1.2 mm thick around a bore: the faces' centroids,
    // a third of the way down, read 2.13.
    let cone = "difference() { cylinder(h=10, r1=5, r2=2.2, $fn=64); \
                translate([0,0,-1]) cylinder(r=1, h=12, $fn=64); }";
    let t = wall(cone, CheckSettings::default());
    assert!(close(t, 1.2, 0.05), "{t}");
    // The run's barb tip, 1.2 mm at its rim, read 1.39.
    let t = wall(
        T3_ADAPTER,
        CheckSettings {
            min_wall: 1.2,
            ..CheckSettings::default()
        },
    );
    assert!(close(t, 1.2, 0.05), "{t}");
}

#[test]
fn twisted_extrusions_have_no_false_thin_walls() {
    // Slivers of a fast twist tilt their normals up to 76°, and rays
    // along them ran into the end caps: 8 "walls" 0.25 mm thick on a
    // solid 20 mm square. Every layer of these is solid across.
    for src in [
        "linear_extrude(height=12, twist=90, slices=100) square(20,center=true);",
        "linear_extrude(height=12, twist=360, slices=100) square(20,center=true);",
        "linear_extrude(height=30, twist=-2160) square(20,center=true);",
        "linear_extrude(height=12, twist=360, slices=100) circle(10, $fn=64);",
        "linear_extrude(height=12, twist=2160, slices=200) circle(10, $fn=48);",
        // A tube, twisted: the ray across the wall leaves through the bore
        // at a slant.
        "linear_extrude(height=50, twist=720, slices=400) difference() { square(20, center=true); circle(6, $fn=64); }",
        // Threads made by twisting an offset circle.
        "linear_extrude(height=20, twist=-360*10, slices=400, $fn=48) translate([0.6,0]) circle(r=6);",
        "cylinder(r=5, h=20, $fn=48); linear_extrude(height=20, twist=-360*8, slices=640) translate([4.2,0]) circle(r=1.6, $fn=24);",
    ] {
        let v = check(src, false, CheckSettings::default());
        assert!(findings(&v, "thin-wall").is_empty(), "{src}\n{v}");
        // The thinnest wall is a measured one, not a sliver's reading
        // just over the minimum.
        let t = v["model"]["min_wall"]["thickness"].as_f64().unwrap();
        assert!(t > 3.0, "{src}: thinnest {t}");
    }
    // The pilot's final part: walls of 1.5 mm and more.
    let v = check(
        T3,
        false,
        CheckSettings {
            min_wall: 1.2,
            ..CheckSettings::default()
        },
    );
    assert!(findings(&v, "thin-wall").is_empty(), "{v}");
}

#[test]
fn real_thin_walls_are_still_found() {
    let wall = |src: &str| -> Vec<f64> {
        let v = check(src, false, CheckSettings::default());
        findings(&v, "thin-wall")
            .iter()
            .map(|f| f["value"].as_f64().unwrap())
            .collect()
    };
    // A 0.3 mm fin on a base.
    let fin = wall("cube([20,20,3]); translate([0,10,3]) cube([20,0.3,10]);");
    assert_eq!(fin.len(), 1, "{fin:?}");
    assert!(close(fin[0], 0.3, 1e-4), "{fin:?}");
    // An open box with 0.5 mm walls and floor.
    let open = wall("difference() { cube(20); translate([0.5,0.5,0.5]) cube([19,19,20]); }");
    assert!(!open.is_empty() && close(open[0], 0.5, 1e-4), "{open:?}");
    // A thin fin, twisted: 0.4 mm across in every layer.
    let twisted =
        wall("linear_extrude(height=10, twist=90, slices=50) square([0.4,10], center=true);");
    assert!(
        !twisted.is_empty() && twisted[0] > 0.3 && twisted[0] < 0.5,
        "{twisted:?}"
    );
    let v = check(
        "cylinder(r=8,h=2); linear_extrude(height=12, twist=360, slices=120) square([0.5,14], center=true);",
        false,
        CheckSettings::default(),
    );
    let t = findings(&v, "thin-wall");
    assert!(!t.is_empty(), "{v}");
    assert!(close(t[0]["value"].as_f64().unwrap(), 0.5, 0.05), "{v}");
    // A flat plate is as thick as its layers.
    let plate = wall("cube([20,20,0.3]);");
    assert!(!plate.is_empty() && close(plate[0], 0.3, 1e-4), "{plate:?}");
    // A leaning 0.5 mm plate: 0.5 / cos 30° = 0.577 mm across in a layer.
    let leaning = wall("rotate([30,0,0]) cube([20,0.5,10]);");
    assert!(
        !leaning.is_empty() && close(leaning[0], 0.5 / 30f64.to_radians().cos(), 1e-3),
        "{leaning:?}"
    );
}

/// A wall modelled at exactly the minimum is not under it (the T2
/// transcript audit: "a wall 1.2 mm thick, under the 1.2 mm minimum"),
/// and one a few thousandths under still is, with its thickness given to
/// enough digits to read as under.
#[test]
fn a_wall_at_exactly_the_minimum_passes() {
    let at = |min_wall: f64, src: &str| {
        check(
            src,
            false,
            CheckSettings {
                min_wall,
                ..CheckSettings::default()
            },
        )
    };
    let v = at(
        1.2,
        "difference() { cube([20,20,10]); translate([1.2,1.2,1.2]) cube([17.6,17.6,10]); }",
    );
    assert!(findings(&v, "thin-wall").is_empty(), "{v}");
    // A wall exactly at the nozzle width is a warning, not an error.
    let v = at(
        0.8,
        "difference() { cube([20,20,10]); translate([0.4,0.4,0.4]) cube([19.2,19.2,10]); }",
    );
    let t = findings(&v, "thin-wall");
    assert_eq!(t.len(), 1, "{v}");
    assert_eq!(t[0]["severity"], "warning", "{v}");
    let v = at(
        1.2,
        "difference() { cube([20,20,10]); translate([1.197,1.197,1.197]) cube([17.606,17.606,10]); }",
    );
    let t = findings(&v, "thin-wall");
    assert_eq!(t.len(), 1, "{v}");
    let m = t[0]["message"].as_str().unwrap();
    assert!(
        m.contains("1.197 mm thick, under the 1.2 mm minimum"),
        "{m}"
    );
}

#[test]
fn pinched_edges_are_not_manifold() {
    // Two cubes sharing an edge: Manifold keeps a vertex for each and
    // calls it valid; an STL of it has an edge with four faces.
    let v = check(
        "cube(10); translate([10,10,0]) cube(10);",
        false,
        CheckSettings::default(),
    );
    assert_eq!(v["model"]["manifold"], false, "{v}");
    let f = findings(&v, "not-manifold");
    assert_eq!(f.len(), 1, "{v}");
    assert_eq!(f[0]["severity"], "error");
    assert_eq!(f[0]["value"], 1.0);
    assert_eq!(
        f[0]["location"]["point"],
        serde_json::json!([10.0, 10.0, 5.0])
    );
    assert!(
        f[0]["fix"]
            .as_str()
            .unwrap()
            .contains("overlap them by at least 0.01")
    );
    // Overlapped a little, it is one solid.
    let ok = check(
        "cube(10); translate([9.99,9.99,0]) cube(10);",
        false,
        CheckSettings::default(),
    );
    assert_eq!(ok["model"]["manifold"], true, "{ok}");
    assert!(findings(&ok, "not-manifold").is_empty(), "{ok}");
    // The pilot's part: one pinched edge (the grader's count), where the
    // rib's end meets the rim at r = 10.64, z = 12.
    let v = check(T3, false, CheckSettings::default());
    let f = findings(&v, "not-manifold");
    assert_eq!(f.len(), 1, "{v}");
    assert_eq!(f[0]["value"], 1.0, "{v}");
    let p: Vec<f64> = f[0]["location"]["point"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect();
    assert!(close(p[2], 12.0, 1e-3), "{p:?}");
    assert!(close(p[0].hypot(p[1]), 10.64, 0.01), "{p:?}");
    // Render's geometry says so too, with where.
    let s = session(&[("m.scad", T3)]);
    let scheme = render::ColorScheme::cornfield();
    let r = s
        .render(&Run::new("m.scad"), session::Mode::Render, &scheme)
        .unwrap();
    let g = r.geometry_json(&scheme.geometry_scheme());
    assert_eq!(g["manifold"], false, "{g}");
    assert_eq!(g["pinched"]["edges"], 1, "{g}");
    // And measure's model.
    let m = measure(T3, |_| {});
    assert_eq!(m["model"]["manifold"], false, "{m}");
    // A real solid's pinch is not parts that only touch.
    assert!(g["pinched"].get("touch_only").is_none(), "{g}");
}

/// A plug seated in its hole touches it on five faces: their intersection
/// is those faces, with no volume, pinched where they fold. That is parts
/// that only touch, not a solid to repair (the T2 transcript audit: "NOT
/// manifold, pinched ... overlap them" inside an interference probe).
#[test]
fn a_zero_volume_intersection_says_the_parts_only_touch() {
    const PLUG: &str = "intersection() {\n\
        difference() { cube([10, 10, 5]); translate([2, 2, 2]) cube([6, 6, 5]); }\n\
        translate([2, 2, 2]) cube([6, 6, 6]);\n}\n";
    let v = check(PLUG, false, CheckSettings::default());
    let f = findings(&v, "not-manifold");
    assert_eq!(f.len(), 1, "{v}");
    assert_eq!(f[0]["fix"], session::check::TOUCH_FIX, "{v}");
    assert!(
        session::check::TOUCH_FIX.starts_with("the parts only touch (no overlap)"),
        "{}",
        session::check::TOUCH_FIX
    );
    let s = session(&[("m.scad", PLUG)]);
    let scheme = render::ColorScheme::cornfield();
    let r = s
        .render(&Run::new("m.scad"), session::Mode::Render, &scheme)
        .unwrap();
    let g = r.geometry_json(&scheme.geometry_scheme());
    assert_eq!(g["volume"], 0.0, "{g}");
    assert_eq!(g["pinched"]["touch_only"], true, "{g}");
    let m = measure(PLUG, |_| {});
    assert_eq!(m["model"]["pinched"]["touch_only"], true, "{m}");
}

#[test]
fn sections_describe_each_contour_and_its_radii() {
    // A tube: outer radius 10, bore 4, cut across.
    let v = measure(
        "difference() { cylinder(r=10, h=10, $fn=360); translate([0,0,-1]) cylinder(r=4, h=12, $fn=360); }",
        |r| r.section = Some(Plane::Z(5.0)),
    );
    let o = v["section"]["outlines"].as_array().unwrap();
    assert_eq!(o.len(), 2, "{v}");
    assert_eq!(o[0]["hole"], false);
    assert_eq!(o[1]["hole"], true);
    let r = |x: &Value, k: usize| x["radius"][k].as_f64().unwrap();
    assert!(
        close(r(&o[0], 1), 10.0, 1e-6) && close(r(&o[0], 0), 10.0, 1e-3),
        "{v}"
    );
    assert!(close(r(&o[1], 1), 4.0, 1e-6), "{v}");
    assert_eq!(v["section"]["axis"], "z");
    // About another axis: a 10 mm cube's section, about its own centre.
    let v = measure("cube(10);", |r| {
        r.section = Some(Plane::Z(5.0));
        r.axis = session::measure::Axis::parse("z", Some([5.0, 5.0])).unwrap();
    });
    let o = &v["section"]["outlines"][0];
    assert!(
        close(r(o, 0), 5.0, 1e-9) && close(r(o, 1), 50f64.sqrt(), 1e-6),
        "{v}"
    );
}

#[test]
fn profiles_give_a_threads_diameters_and_pitch() {
    let v = measure(T3, |r| {
        r.profile = Some(session::measure::Profile::new(0.0, 49.0, 0.1).unwrap());
    });
    let p = &v["profile"];
    // Pitch 2 from the crests along +x, over the thread.
    assert!(close(p["pitch"].as_f64().unwrap(), 2.0, 1e-4), "{p}");
    assert!(
        close(p["pitch_span"][0].as_f64().unwrap(), 2.0, 1e-3),
        "{p}"
    );
    assert!(
        close(p["pitch_span"][1].as_f64().unwrap(), 10.0, 1e-3),
        "{p}"
    );
    // In the thread, the outer contour spans minor to major radius: the
    // grader's 23.28 major diameter.
    let band = p["bands"][60].as_array().unwrap();
    assert!(close(band[0].as_f64().unwrap(), 6.0, 1e-9), "{band:?}");
    assert!(
        close(2.0 * band[2].as_f64().unwrap(), 23.29, 0.01),
        "{band:?}"
    );
    assert!(
        close(2.0 * band[1].as_f64().unwrap(), 21.28, 0.01),
        "{band:?}"
    );
    // The hex flange above its chamfer: 30 across flats, 34.64 across
    // corners.
    let band = p["bands"][220].as_array().unwrap();
    assert!(close(band[1].as_f64().unwrap(), 15.0, 1e-6), "{band:?}");
    assert!(close(band[2].as_f64().unwrap(), 17.3205, 1e-3), "{band:?}");
    assert_eq!(p["bands"].as_array().unwrap().len(), 491);
    // Too many samples is refused.
    assert!(session::measure::Profile::new(0.0, 100.0, 0.01).is_err());
    assert!(session::measure::Profile::new(1.0, 0.0, 0.1).is_err());
}

#[test]
fn crests_are_refined_between_samples_and_cut_ends_left_out() {
    let crests = |from: f64, to: f64, step: f64| {
        let v = measure(T3_ADAPTER, |r| {
            r.profile = Some(session::measure::Profile::new(from, to, step).unwrap());
        });
        v["profile"].clone()
    };
    // The run's barb: flat tops 0.4 wide at 27.6, 35.0 and 42.4, which a
    // 0.4 step put at 27.6, 35.2 and 42.4.
    let p = crests(24.4, 49.4, 0.4);
    let at: Vec<f64> = p["crests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect();
    assert_eq!(at.len(), 3, "{p}");
    for (x, want) in at.iter().zip([27.6, 35.0, 42.4]) {
        assert!(close(*x, want, 0.05), "{at:?}");
    }
    // The thread: its last crest is half as wide where the flange starts,
    // and fitted in, the pitch was 1.98.
    for (from, to, step) in [(0.0, 12.0, 0.1), (0.0, 49.0, 0.1), (0.05, 12.0, 0.3)] {
        let p = crests(from, to, step);
        assert!(close(p["pitch"].as_f64().unwrap(), 2.0, 1e-3), "{p}");
        assert!(
            close(p["pitch_span"][1].as_f64().unwrap(), 10.0, 1e-3),
            "{p}"
        );
    }
}

#[test]
fn overlaps_are_listed_piece_by_piece() {
    // b pokes into a in two places: 2 x 2 x 1 and 1 x 1 x 1.
    let v = measure(
        "part(\"a\") cube([20,10,10]);\n\
         part(\"b\") { translate([2,2,9]) cube([2,2,5]); translate([15,2,9]) cube([1,1,5]); translate([2,2,13]) cube([14,1,1]); }\n",
        |r| r.between = Some(("a".into(), "b".into())),
    );
    let b = &v["between"];
    assert_eq!(b["overlapping"], true, "{v}");
    assert_eq!(b["overlap_pieces"], 2, "{b}");
    let pieces = b["pieces"].as_array().unwrap();
    assert!(
        close(pieces[0]["volume"].as_f64().unwrap(), 4.0, 1e-6),
        "{b}"
    );
    assert!(
        close(pieces[1]["volume"].as_f64().unwrap(), 1.0, 1e-6),
        "{b}"
    );
    assert_eq!(
        pieces[1]["bbox"]["min"],
        serde_json::json!([15.0, 2.0, 9.0]),
        "{b}"
    );
}

/// A unit cube as a polyhedron, faces clockwise seen from outside (the
/// OpenSCAD manual's order), with the faces listed in `faces`.
fn cube_poly(faces: &str) -> String {
    format!(
        "polyhedron([[0,0,0],[1,0,0],[1,1,0],[0,1,0],[0,0,1],[1,0,1],[1,1,1],[0,1,1]],\n  {faces});\n"
    )
}

const CUBE_FACES: &str = "[[0,1,2,3],[4,5,1,0],[7,6,5,4],[5,6,2,1],[6,7,3,2],[7,4,0,3]]";
const CUBE_INSIDE_OUT: &str = "[[3,2,1,0],[0,1,5,4],[4,5,6,7],[1,2,6,5],[2,3,7,6],[3,0,4,7]]";

/// The NeoSCAD-only diagnostics about input meshes of `src`, from
/// evaluation and from a render, and the render's console text.
fn mesh_diags(src: &str) -> (Vec<Value>, Vec<Value>, String) {
    let s = session(&[("m.scad", src)]);
    let pick = |v: Vec<Value>| -> Vec<Value> {
        v.into_iter()
            .filter(|d| d["code"].as_str().unwrap().starts_with("polyhedron-"))
            .collect()
    };
    let e = s.evaluate(&Run::new("m.scad"), false).unwrap();
    let scheme = render::ColorScheme::cornfield();
    let r = s
        .render(&Run::new("m.scad"), session::Mode::Render, &scheme)
        .unwrap();
    (
        pick(e.log.diagnostics_json()),
        pick(r.log.diagnostics_json()),
        String::from_utf8(r.log.stderr).unwrap(),
    )
}

#[test]
fn a_correct_polyhedron_has_no_mesh_findings() {
    let src = cube_poly(CUBE_FACES);
    let (e, r, _) = mesh_diags(&src);
    assert!(e.is_empty() && r.is_empty(), "{e:?} {r:?}");
    let v = check(&src, false, CheckSettings::default());
    assert!(
        v["findings"]
            .as_array()
            .unwrap()
            .iter()
            .all(|f| !f["code"].as_str().unwrap().starts_with("polyhedron-")),
        "{v}"
    );
    // Mirrored, OpenSCAD reverses the faces with the points: still right.
    let (e, _, _) = mesh_diags(&format!("mirror([1,0,0]) {src}"));
    assert!(e.is_empty(), "{e:?}");
}

#[test]
fn an_inside_out_polyhedron_is_reported_with_its_fix() {
    let src = cube_poly(CUBE_INSIDE_OUT);
    let (e, r, stderr) = mesh_diags(&src);
    assert_eq!(e.len(), 1, "{e:?}");
    assert_eq!(e, r);
    let d = &e[0];
    assert_eq!(d["code"], "polyhedron-inside-out");
    assert_eq!(d["severity"], "warning");
    assert_eq!(d["line"], 1);
    assert!(
        d["message"]
            .as_str()
            .unwrap()
            .contains("inside out: all 6 faces point inward"),
        "{d}"
    );
    let h = &d["hints"][0];
    assert!(h["message"].as_str().unwrap().contains("clockwise"), "{h}");
    // The faces are written out, so the fix is an exact edit: each face's
    // indices reversed, in place.
    assert_eq!(h["replace"]["text"], CUBE_FACES);
    assert_eq!(h["replace"]["span"]["start"]["line"], 2);
    // Never on the console: OpenSCAD prints nothing about it.
    assert!(!stderr.contains("inside out"), "{stderr}");
    // `check` lists it first, as a warning.
    let v = check(&src, false, CheckSettings::default());
    assert_eq!(v["findings"][0]["code"], "polyhedron-inside-out", "{v}");
    assert_eq!(v["findings"][0]["severity"], "warning");
    assert!(
        v["findings"][0]["message"]
            .as_str()
            .unwrap()
            .ends_with("(m.scad:1)"),
        "{v}"
    );
    // Reversing the faces with a list comprehension is the fix the hint
    // gives for faces that are computed; no edit is offered for those.
    let computed = format!(
        "f = {CUBE_INSIDE_OUT};\npolyhedron([[0,0,0],[1,0,0],[1,1,0],[0,1,0],[0,0,1],[1,0,1],[1,1,1],[0,1,1]], [for (x = f) x]);\n"
    );
    let (e, _, _) = mesh_diags(&computed);
    assert_eq!(e[0]["code"], "polyhedron-inside-out");
    assert!(e[0]["hints"][0].get("replace").is_none(), "{e:?}");
    let fixed = format!(
        "f = {CUBE_INSIDE_OUT};\npolyhedron([[0,0,0],[1,0,0],[1,1,0],[0,1,0],[0,0,1],[1,0,1],[1,1,1],[0,1,1]], [for (x = f) [for (i = [len(x) - 1:-1:0]) x[i]]]);\n"
    );
    assert!(mesh_diags(&fixed).0.is_empty());
    // Mirroring twice does not turn it outward: each mirror reverses the
    // faces with the points (checked against OpenSCAD's export too).
    let (e, _, _) = mesh_diags(&format!("mirror([1,0,0]) mirror([1,0,0]) {src}"));
    assert_eq!(e[0]["code"], "polyhedron-inside-out");
}

#[test]
fn a_flipped_face_is_located_in_the_model() {
    // The fourth face (x = 1) reversed, and the cube moved.
    let faces = "[[0,1,2,3],[4,5,1,0],[7,6,5,4],[1,2,6,5],[6,7,3,2],[7,4,0,3]]";
    let src = format!("translate([3,0,0]) {}", cube_poly(faces));
    let (e, r, _) = mesh_diags(&src);
    assert_eq!(e, r);
    assert_eq!(e.len(), 1, "{e:?}");
    assert_eq!(e[0]["code"], "polyhedron-flipped-faces");
    let m = e[0]["message"].as_str().unwrap();
    assert!(
        m.contains("1 of this polyhedron's 6 faces points inward") && m.contains("[4, 0.5, 0.5]"),
        "{m}"
    );
    assert_eq!(
        e[0]["hints"][0]["replace"]["text"],
        "[[0,1,2,3],[4,5,1,0],[7,6,5,4],[5,6,2,1],[6,7,3,2],[7,4,0,3]]"
    );
    let v = check(&src, false, CheckSettings::default());
    let f = findings(&v, "polyhedron-flipped-faces");
    assert_eq!(
        f[0]["location"]["point"],
        serde_json::json!([4.0, 0.5, 0.5])
    );
}

#[test]
fn an_open_polyhedron_is_reported() {
    let src = cube_poly("[[0,1,2,3],[4,5,1,0],[7,6,5,4],[5,6,2,1],[6,7,3,2]]");
    let (e, _, _) = mesh_diags(&src);
    assert_eq!(e.len(), 1, "{e:?}");
    assert_eq!(e[0]["code"], "polyhedron-open");
    assert!(
        e[0]["message"]
            .as_str()
            .unwrap()
            .contains("4 edges are used by only one face"),
        "{e:?}"
    );
    let v = check(&src, false, CheckSettings::default());
    assert_eq!(findings(&v, "polyhedron-open").len(), 1, "{v}");
}

#[test]
fn an_inside_out_import_is_reported() {
    // An ASCII STL of the unit cube with every facet's corners reversed.
    let mut stl = String::from("solid c\n");
    let p = [
        [0, 0, 0],
        [1, 0, 0],
        [1, 1, 0],
        [0, 1, 0],
        [0, 0, 1],
        [1, 0, 1],
        [1, 1, 1],
        [0, 1, 1],
    ];
    // Clockwise from outside: inside out as an STL.
    for f in [
        [0, 1, 2, 3],
        [4, 5, 1, 0],
        [7, 6, 5, 4],
        [5, 6, 2, 1],
        [6, 7, 3, 2],
        [7, 4, 0, 3],
    ] {
        for t in [[f[0], f[1], f[2]], [f[0], f[2], f[3]]] {
            stl.push_str("facet normal 0 0 0\nouter loop\n");
            for v in t {
                let q = p[v];
                stl.push_str(&format!("vertex {} {} {}\n", q[0], q[1], q[2]));
            }
            stl.push_str("endloop\nendfacet\n");
        }
    }
    stl.push_str("endsolid c\n");
    let s = session(&[("m.scad", "import(\"c.stl\");\n"), ("c.stl", &stl)]);
    let scheme = render::ColorScheme::cornfield();
    let r = s
        .render(&Run::new("m.scad"), session::Mode::Render, &scheme)
        .unwrap();
    let d: Vec<Value> = r
        .log
        .diagnostics_json()
        .into_iter()
        .filter(|d| d["code"] == "polyhedron-inside-out")
        .collect();
    assert_eq!(d.len(), 1, "{:?}", r.log.diagnostics_json());
    assert!(
        d[0]["message"]
            .as_str()
            .unwrap()
            .starts_with("the mesh imported from 'c.stl' is inside out"),
        "{d:?}"
    );
}

const T3_INSIDE_OUT: &str = include_str!("data/pilot_t3_inside_out.scad");

#[test]
fn the_pilots_inside_out_thread_is_named_before_the_pinch() {
    let (e, r, stderr) = mesh_diags(T3_INSIDE_OUT);
    assert_eq!(e.len(), 1, "{e:?}");
    assert_eq!(e, r);
    assert_eq!(e[0]["code"], "polyhedron-inside-out");
    // The polyhedron() call in thread_groove().
    assert_eq!(e[0]["line"], 99);
    assert!(
        e[0]["message"]
            .as_str()
            .unwrap()
            .contains("signed volume is -1716.14 mm³"),
        "{e:?}"
    );
    // Computed faces: the hint's text, no edit.
    assert!(e[0]["hints"][0].get("replace").is_none());
    assert!(!stderr.contains("polyhedron"), "{stderr}");
    // `check`: the inside-out polyhedron first; the pinched edges it
    // caused point back to it instead of saying to overlap the parts.
    let v = check(T3_INSIDE_OUT, false, CheckSettings::default());
    assert_eq!(v["findings"][0]["code"], "polyhedron-inside-out", "{v}");
    let pinch = findings(&v, "not-manifold");
    assert_eq!(pinch.len(), 1, "{v}");
    let fix = pinch[0]["fix"].as_str().unwrap();
    assert!(fix.starts_with("fix #1 first: an inside-out"), "{fix}");
}

/// The CAD pilot's twisted thread (run cad-20260929T024448Z): a 360-point
/// section in a twisted `linear_extrude`, unioned with a core cylinder at
/// exactly the thread's root radius.
const T3_TWISTED: &str = include_str!("data/pilot_t3_twisted_thread.scad");

#[test]
fn edges_that_break_only_at_stl_precision_are_a_warning() {
    // Manifold by exact position; the grader's weld of its STL found 738
    // non-manifold edges (2998 collapsed triangles), and so does the f32
    // weld on aarch64, where the grader ran. The counts come from slivers
    // a few f32 steps wide, so the mesh's last bits decide them, and those
    // follow the platform's multiply-add rounding (`eval::fma`): on x86_64
    // the mesh is as manifold but welds to 714 edges and 3094 triangles
    // (Linux CI and Rosetta alike). So the exact counts are checked on
    // aarch64, and what holds everywhere is checked everywhere: hundreds
    // of broken edges, more collapsed triangles than that, and `check`
    // and `render` reporting the same counts.
    let v = check(T3_TWISTED, false, CheckSettings::default());
    assert_eq!(v["model"]["manifold"], true, "{v}");
    let f = findings(&v, "stl-precision");
    assert_eq!(f.len(), 1, "{v}");
    assert_eq!(f[0]["severity"], "warning");
    let edges = f[0]["value"].as_f64().unwrap() as u64;
    let msg = f[0]["message"].as_str().unwrap();
    let collapsed: u64 = msg
        .split(" triangles collapse")
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("{msg}"));
    if cfg!(target_arch = "aarch64") {
        assert_eq!((edges, collapsed), (738, 2998), "{msg}");
    }
    assert!((100..2000).contains(&edges), "{msg}");
    assert!(collapsed > edges, "{msg}");
    assert!(msg.contains(&format!("leaving {edges} edges")), "{msg}");
    // 48 mm is the largest coordinate: f32 spacing 2^-18 there.
    assert!(msg.contains("3.8e-6 mm"), "{msg}");
    assert!(
        f[0]["fix"].as_str().unwrap().contains("coincident"),
        "{}",
        f[0]
    );
    // The first bad edge is on the thread's root radius.
    let p: Vec<f64> = f[0]["location"]["point"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_f64().unwrap())
        .collect();
    assert!(close(p[0].hypot(p[1]), 10.5732, 0.01), "{p:?}");
    // Render's geometry carries it.
    let s = session(&[("m.scad", T3_TWISTED)]);
    let scheme = render::ColorScheme::cornfield();
    let r = s
        .render(&Run::new("m.scad"), session::Mode::Render, &scheme)
        .unwrap();
    let g = r.geometry_json(&scheme.geometry_scheme());
    assert_eq!(g["manifold"], true, "{g}");
    assert_eq!(g["stl_precision"]["nonmanifold_edges"], edges, "{g}");
    assert_eq!(g["stl_precision"]["collapsed_faces"], collapsed, "{g}");
    assert_eq!(
        g["stl_precision"]["spacing"],
        serde_json::json!(session::stats::round6(2f64.powi(-18))),
        "{g}"
    );
}

/// Faces that only collapse, every edge still paired, are info that needs
/// no action: the message says so, and short reports leave the fix out
/// (an agent in the T2 transcript audit read it as an instruction).
#[test]
fn collapsed_faces_alone_need_no_action() {
    let v = check(
        "cube(10); translate([0,0,10-1e-7]) cube([5,5,5]);",
        false,
        CheckSettings::default(),
    );
    let f = findings(&v, "stl-precision");
    assert_eq!(f.len(), 1, "{v}");
    assert_eq!(f[0]["severity"], "info");
    let msg = f[0]["message"].as_str().unwrap();
    assert!(
        msg.contains("the mesh stays closed, so no action is needed"),
        "{msg}"
    );
    // The full JSON keeps the fix; the text report does not print it.
    assert!(f[0]["fix"].as_str().unwrap().contains("coincident"));
    assert!(!session::check::fix_shown(f[0]));
    let text = session::check::text(&v);
    assert!(text.contains("no action is needed"), "{text}");
    assert!(!text.contains("coincident"), "{text}");
}

#[test]
fn faces_a_hair_apart_merge_at_stl_precision() {
    // Two 100 mm cubes 1e-7 apart: two pieces here, one face with four
    // triangles on each edge in an STL (f32 spacing at 200 is 1.5e-5).
    let v = check(
        "cube(100); translate([100 + 1e-7, 0, 0]) cube(100);",
        false,
        CheckSettings::default(),
    );
    let f = findings(&v, "stl-precision");
    assert_eq!(f.len(), 1, "{v}");
    assert_eq!(f[0]["severity"], "warning");
    assert!(
        f[0]["message"]
            .as_str()
            .unwrap()
            .starts_with("vertices a hair apart merge"),
        "{}",
        f[0]
    );
    // A normal model, curved and booleaned, has nothing to report.
    let ok = check(
        "difference() { sphere(20, $fn = 96); cylinder(r = 5, h = 50, center = true, $fn = 64); }",
        false,
        CheckSettings::default(),
    );
    assert!(findings(&ok, "stl-precision").is_empty(), "{ok}");
    let s = session(&[("m.scad", "cube(10); sphere(7);")]);
    let scheme = render::ColorScheme::cornfield();
    let r = s
        .render(&Run::new("m.scad"), session::Mode::Render, &scheme)
        .unwrap();
    let g = r.geometry_json(&scheme.geometry_scheme());
    assert!(g.get("stl_precision").is_none(), "{g}");
}
