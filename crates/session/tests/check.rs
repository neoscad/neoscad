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
