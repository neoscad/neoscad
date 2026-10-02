//! `check`'s `cut-away` and `cuts-nothing` findings: objects a
//! `difference()` removes entirely, and subtracted children that touch
//! nothing. The first is the enclosure mistake agents make: standoff posts
//! put in a `union()` with the shell, and the cavity subtracted through
//! them.

use std::path::PathBuf;
use std::sync::Arc;

use lang::loader::LibraryPath;
use lang::vfs::MemFs;
use serde_json::Value;
use session::check::{CheckRequest, CheckSettings};
use session::{Config, Run, Session};

/// Checks `/doc/m.scad`; `files` are written too, with paths from the root.
fn check_with(src: &str, files: &[(&str, &str)]) -> Value {
    let fs = Arc::new(MemFs::new());
    fs.insert("/doc/m.scad", src.as_bytes().to_vec());
    for (p, t) in files {
        fs.insert(*p, t.as_bytes().to_vec());
    }
    let mut cfg = Config::new(fs, LibraryPath(vec![PathBuf::from("/lib")]));
    cfg.work_dir = PathBuf::from("/doc");
    // Bounded like an agent's calls.
    cfg.limits = session::Limits::AGENT;
    let s = Session::new(cfg);
    let run = Run::new("m.scad");
    s.check(&CheckRequest {
        run,
        settings: CheckSettings::default(),
    })
    .unwrap()
    .summary
}

fn check(src: &str) -> Value {
    check_with(src, &[])
}

fn findings<'a>(v: &'a Value, code: &str) -> Vec<&'a Value> {
    v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["code"] == code)
        .collect()
}

fn text(f: &Value, k: &str) -> String {
    f[k].as_str().unwrap_or("").to_string()
}

/// The shell of a 54 x 30 x 14 base with a 2 mm floor and walls, and four
/// 4 mm posts 3 mm tall on the floor at `z0`.
const SHELL: &str = "cube([54, 30, 14]);";
const CAVITY: &str = "translate([2, 2, 2]) cube([50, 26, 13]);";
fn posts(z0: f64) -> String {
    format!(
        "for (x = [8, 46], y = [8, 22]) translate([x, y, {z0}]) cylinder(d = 4, h = {}, $fn = 24);",
        5.0 - z0
    )
}

/// The failure: the posts in a union with the shell, the cavity then
/// subtracted from both, so nothing of the posts is left.
#[test]
fn posts_unioned_with_the_shell_are_cut_away() {
    let src = format!(
        "difference() {{\n  union() {{\n    {SHELL}\n    {}\n  }}\n  {CAVITY}\n}}\n",
        posts(2.0)
    );
    let v = check(&src);
    let f = findings(&v, "cut-away");
    assert_eq!(f.len(), 1, "{v}");
    let m = text(f[0], "message");
    assert!(
        m.starts_with(
            "the difference() at m.scad:1 removes the 4 objects made by cylinder() at m.scad:4"
        ),
        "{m}"
    );
    assert!(m.ends_with("nothing of them is left in the result"), "{m}");
    let fix = text(f[0], "fix");
    assert!(fix.contains("(the cube() at m.scad:6)"), "{fix}");
    assert!(fix.contains("add them after the subtraction"), "{fix}");
    assert_eq!(f[0]["severity"], "warning");
    // Located at the posts: z 2 to 5, across all four.
    let b = &f[0]["location"]["bbox"];
    assert_eq!(b["min"][2], 2.0, "{v}");
    assert_eq!(b["max"][2], 5.0, "{v}");
    assert_eq!(v["counts"]["errors"], 0, "{v}");
}

/// Posts sunk into the floor keep a stub there, inside the floor: they
/// still add nothing, whether the shell is a floor and walls or a solid
/// block the cavity is cut from (an agent's base, posts from z = 0).
#[test]
fn posts_sunk_into_the_floor_are_still_cut_away() {
    let walls = "cube([54, 30, 2]); difference() { cube([54, 30, 14]); translate([2, 2, -1]) cube([50, 26, 20]); }";
    for (shell, z0) in [(walls, 1.0), (SHELL, 1.0), (SHELL, 0.0)] {
        let src = format!(
            "difference() {{ union() {{ {shell} {} }} {CAVITY} }}",
            posts(z0)
        );
        let v = check(&src);
        assert_eq!(findings(&v, "cut-away").len(), 1, "{src}: {v}");
    }
}

/// An object drilled through its core, its ring buried in another object,
/// is missing because of that object, not the cut: a lead-in taper drawn
/// inside the barb stem it should stick out of, with a bore through both
/// (two agents' hose adapters). The cut is not blamed.
#[test]
fn a_core_drilled_object_buried_in_another_is_not_blamed_on_the_cut() {
    let v = check(
        "difference() { union() { cylinder(d = 11.4, h = 25, $fn = 32); translate([0, 0, 21.5]) cylinder(d1 = 11.4, d2 = 10.4, h = 3.5, $fn = 32); } translate([0, 0, -1]) cylinder(d = 8, h = 27, $fn = 32); }",
    );
    assert!(findings(&v, "cut-away").is_empty(), "{v}");
}

/// The fix the finding gives: the posts added after the difference.
#[test]
fn a_correct_enclosure_has_no_finding() {
    let src = format!(
        "union() {{ difference() {{ {SHELL} {CAVITY} }} {} }}",
        posts(1.0)
    );
    let v = check(&src);
    assert!(findings(&v, "cut-away").is_empty(), "{v}");
    assert!(findings(&v, "cuts-nothing").is_empty(), "{v}");
}

/// A boss drilled through keeps its ring; a body with a pocket keeps its
/// walls; a block buried in the body before any cut is not the cut's
/// doing; none of these is a finding.
#[test]
fn ordinary_differences_have_no_finding() {
    let srcs = [
        // A drilled boss on a plate.
        "difference() { union() { cube([20, 20, 2]); translate([10, 10, 0]) cylinder(d = 8, h = 8, $fn = 24); } translate([10, 10, -1]) cylinder(d = 3, h = 10, $fn = 24); }",
        // A redundant block inside the body, and a hole elsewhere.
        "difference() { union() { cube(20); translate([5, 5, 5]) cube(4); } translate([15, 15, -1]) cylinder(d = 3, h = 30, $fn = 16); }",
        // A countersunk hole: cone and shaft overlap each other.
        "difference() { cube([20, 20, 4]); translate([10, 10, -1]) cylinder(d = 4, h = 6, $fn = 24); translate([10, 10, 2]) cylinder(d1 = 4, d2 = 8.1, h = 2.01, $fn = 24); }",
        // A 2D difference.
        "linear_extrude(2) difference() { square(10); translate([5, 5]) circle(2); }",
        // A background operand, which a difference leaves out.
        "difference() { sphere(10, $fn = 24); %cylinder(h = 30, r = 6, center = true); }",
    ];
    for src in srcs {
        let v = check(src);
        assert!(findings(&v, "cut-away").is_empty(), "{src}: {v}");
        assert!(findings(&v, "cuts-nothing").is_empty(), "{src}: {v}");
    }
}

/// A difference whose only object is removed: the result's volume, which
/// the render has, answers it without a boolean.
#[test]
fn a_lone_object_removed_whole_is_cut_away() {
    let v = check(
        "cube(10);\ntranslate([20, 0, 0]) difference() { cube(5); translate([-1, -1, -1]) cube(7); }",
    );
    let f = findings(&v, "cut-away");
    assert_eq!(f.len(), 1, "{v}");
    assert!(
        text(f[0], "message").contains("removes the cube() at m.scad:2"),
        "{v}"
    );
}

/// The 0.01 mm slivers that keep faces from being coplanar are not
/// objects a reader misses.
#[test]
fn slivers_are_not_reported() {
    let v = check(
        "difference() { union() { cube(10); translate([2, 2, 10]) cube([6, 6, 0.01]); } translate([1, 1, 9]) cube([8, 8, 5]); }",
    );
    assert!(findings(&v, "cut-away").is_empty(), "{v}");
}

/// A library's own differences are its business: BOSL2's `diff()` and
/// MCAD subtract what they like.
#[test]
fn differences_in_libraries_are_not_looked_at() {
    let lib = format!(
        "module base() difference() {{ union() {{ {SHELL} {} }} {CAVITY} }}",
        posts(2.0)
    );
    let v = check_with(
        "use <enclosure.scad>\nbase();",
        &[("/lib/enclosure.scad", &lib)],
    );
    assert!(findings(&v, "cut-away").is_empty(), "{v}");
    // The same file beside the model is the user's own.
    let v = check_with(
        "use <enclosure.scad>\nbase();",
        &[("/doc/enclosure.scad", &lib)],
    );
    assert_eq!(findings(&v, "cut-away").len(), 1, "{v}");
}

/// A hole that misses the part is reported; a loop of holes of which one
/// falls off the end is not.
#[test]
fn a_subtracted_child_that_touches_nothing() {
    let v = check(
        "difference() {\n  cube([20, 20, 4]);\n  translate([10, 10, -1]) cylinder(d = 3, h = 6, $fn = 16);\n  translate([40, 10, -1]) cylinder(d = 3, h = 6, $fn = 16);\n}",
    );
    let f = findings(&v, "cuts-nothing");
    assert_eq!(f.len(), 1, "{v}");
    let m = text(f[0], "message");
    assert!(m.contains("the cylinder() at m.scad:4"), "{m}");
    assert!(
        m.ends_with("removes nothing: it does not touch the first child"),
        "{m}"
    );
    // Inside the part's box, but in a pocket's empty space.
    let v = check(
        "difference() { cube([20, 20, 10]); translate([2, 2, 2]) cube([16, 16, 10]); translate([10, 10, 5]) sphere(2, $fn = 16); }",
    );
    let f = findings(&v, "cuts-nothing");
    assert_eq!(f.len(), 1, "{v}");
    assert!(
        text(f[0], "message").contains("the sphere() at m.scad:1")
            && text(f[0], "message").ends_with("removed by the other subtracted children already"),
        "{v}"
    );
    // One call, cutting at x = 5 and 15 and missing at 25.
    let v = check(
        "difference() { cube([20, 20, 4]); for (x = [5 : 10 : 25]) translate([x, 10, -1]) cylinder(d = 3, h = 6, $fn = 16); }",
    );
    assert!(findings(&v, "cuts-nothing").is_empty(), "{v}");
}
