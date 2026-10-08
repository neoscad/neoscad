//! `conformance fillet-corpus`: the stop rule of `docs/fillets.md`
//! (section 15), measured on section 16's generated corpus.
//!
//! Each model is a plate, a box or a bracket with holes, blind holes,
//! countersinks, bosses, slots and pockets at sizes drawn from a seeded
//! generator (the same models for the same seed and count, on every
//! machine), one `fillet_edges()` or `chamfer_edges()` call around it with
//! a selector built from the atoms, and a size from small to past what
//! fits. Every model is exported with `neoscad --enable fillet --enable
//! exact --format json -o x.step` in its own process, under `--limit`s and
//! a resident-size guard, and its report is classified:
//!
//! - **built**: the call built its blends (no `fillet-*` error). Built is
//!   **valid** when the export passed its checks with every face exact
//!   and, with `--occt`, OCCT reads the file back as valid closed solids
//!   with no free edges and our volume to 1e-6.
//! - **fixed**: refused as `fillet-too-large` or `fillet-overlap` with a
//!   `replace` edit in its hint. The edit is applied and the model run
//!   again; the fixed model must be built and valid.
//! - **refused**: another diagnostic of a class v1 does not build
//!   (`fillet-unsupported-vertex`, `fillet-unsupported-edge`), or a size
//!   problem with no edit to offer.
//! - **failed**: `fillet-failed`, or a built call whose export failed or
//!   was not all exact: the failures the stop rule counts, by class.
//! - **killed**: over the time limit or the memory guard.
//! - **none**: the selector matched no edge (not counted).
//!
//! The stop rule's figure is valid over the supported class: built,
//! fixed and failed models. A **mesh** failure is a built model whose
//! normal render is missing, or whose exact export disagrees with the mesh
//! by more than the arcs' sagitta allows (the export's volume and box
//! cross-checks).
//!
//! Results go to `target/conformance/fillet/` (never `progress/`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::Duration;

use serde_json::{Value, json};

use crate::ctx::Ctx;
use crate::exact::guarded_under;

/// What `conformance fillet-corpus` was asked to do.
#[derive(Debug)]
pub struct FilletOptions {
    pub count: usize,
    pub seed: u64,
    pub jobs: usize,
    pub timeout: Duration,
    pub binary: Option<PathBuf>,
    pub occt: Option<PathBuf>,
    pub filter: Option<String>,
    pub set: Set,
}

/// The most hint edits one model gets: nested calls can need one each.
const FIX_ROUNDS: usize = 3;

/// The most models one run makes (`docs/fillets.md`, section 16).
pub const MAX_COUNT: usize = 2000;

/// The whole sweep's resident-size budget: each of the parallel exports
/// is held under its share, so the sweep stays under it whatever `jobs`
/// is. Two runaway sweeps have filled the owner's swap before (CLAUDE.md).
const BUDGET_KIB: u64 = 2 * 1024 * 1024;

/// `Limits::AGENT`'s counts as `--limit` flags (its time and memory are
/// set per run).
const AGENT_LIMITS: [&str; 8] = [
    "fragments=10000",
    "slices=10000",
    "list=10000000",
    "string=67108864",
    "rands=10000000",
    "triangles=10000000",
    "sketch_unknowns=5000",
    "queries=10000",
];

/// A seeded generator (SplitMix64): the same models on every platform.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    /// In `[0, 1)`.
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, a: f64, b: f64) -> f64 {
        a + (b - a) * self.unit()
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn pick<'a, T>(&mut self, xs: &'a [T]) -> &'a T {
        &xs[self.below(xs.len())]
    }
    fn chance(&mut self, p: f64) -> bool {
        self.unit() < p
    }
}

/// A number as the models print it: two decimals, no trailing zeros.
fn n(x: f64) -> String {
    let s = format!("{x:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if s == "-0" { "0".into() } else { s }
}

/// One generated model.
#[derive(Debug, Clone)]
struct Case {
    id: String,
    family: &'static str,
    text: String,
}

/// A round feature's footprint on the top face, for keeping features
/// apart: centre and radius.
type Spot = (f64, f64, f64);

/// Places a footprint of radius `r` on an `l` by `w` face, `gap` clear of
/// its edges and of `spots`; `None` after a few tries.
fn place(g: &mut Rng, l: f64, w: f64, r: f64, gap: f64, spots: &[Spot]) -> Option<(f64, f64)> {
    for _ in 0..20 {
        let m = r + gap;
        if 2.0 * m >= l || 2.0 * m >= w {
            return None;
        }
        let x = g.range(m, l - m);
        let y = g.range(m, w - m);
        if spots
            .iter()
            .all(|&(a, b, s)| ((a - x).powi(2) + (b - y).powi(2)).sqrt() > r + s + gap)
        {
            return Some((x, y));
        }
    }
    None
}

/// The features cut into or added on a top face at height `t` of an `l`
/// by `w` body: (added solids, cut solids).
fn features(g: &mut Rng, l: f64, w: f64, t: f64) -> (Vec<String>, Vec<String>) {
    let mut add = Vec::new();
    let mut cut = Vec::new();
    let mut spots: Vec<Spot> = Vec::new();
    let k = g.below(4) + 1;
    for _ in 0..k {
        // Walls between features and edges from thin to generous: thin
        // ones are where sizes stop fitting.
        let gap = *g.pick(&[0.6, 1.0, 2.0, 3.0, 5.0]);
        match g.below(6) {
            0 => {
                let r = g.range(1.5, 6.0);
                if let Some((x, y)) = place(g, l, w, r, gap, &spots) {
                    cut.push(format!(
                        "translate([{}, {}, -1]) cylinder(r = {}, h = {});",
                        n(x),
                        n(y),
                        n(r),
                        n(t + 2.0)
                    ));
                    spots.push((x, y, r));
                }
            }
            1 => {
                let r = g.range(1.5, 5.0);
                let d = g.range(0.3, 0.8) * t;
                if let Some((x, y)) = place(g, l, w, r, gap, &spots) {
                    cut.push(format!(
                        "translate([{}, {}, {}]) cylinder(r = {}, h = {});",
                        n(x),
                        n(y),
                        n(t - d),
                        n(r),
                        n(d + 1.0)
                    ));
                    spots.push((x, y, r));
                }
            }
            2 => {
                // A countersink: a hole and a cone opening to the top.
                let r = g.range(1.5, 3.5);
                let c = g.range(0.8, 0.45 * t);
                if let Some((x, y)) = place(g, l, w, r + c + 1.0, gap, &spots) {
                    cut.push(format!(
                        "translate([{x}, {y}, -1]) cylinder(r = {r}, h = {h}); translate([{x}, {y}, {z}]) cylinder(r1 = {r}, r2 = {r2}, h = {hc});",
                        x = n(x),
                        y = n(y),
                        r = n(r),
                        h = n(t + 2.0),
                        z = n(t - c),
                        r2 = n(r + c + 1.0),
                        hc = n(c + 1.0)
                    ));
                    spots.push((x, y, r + c + 1.0));
                }
            }
            3 => {
                let r = g.range(2.0, 6.0);
                let h = g.range(2.0, 10.0);
                if let Some((x, y)) = place(g, l, w, r, gap, &spots) {
                    add.push(format!(
                        "translate([{}, {}, 0]) cylinder(r = {}, h = {});",
                        n(x),
                        n(y),
                        n(r),
                        n(t + h)
                    ));
                    spots.push((x, y, r));
                }
            }
            4 => {
                // A slot through: a rounded rectangle, long and thin.
                let rw = g.range(1.0, 3.0);
                let len = g.range(4.0, 14.0);
                let half = 0.5 * len + rw;
                if let Some((x, y)) = place(g, l, w, half, gap, &spots) {
                    let rot = if g.chance(0.5) { 0.0 } else { 90.0 };
                    cut.push(format!(
                        "translate([{}, {}, -1]) rotate([0, 0, {}]) linear_extrude({}) offset(r = {}) square([{}, 0.6], center = true);",
                        n(x),
                        n(y),
                        n(rot),
                        n(t + 2.0),
                        n(rw),
                        n(len)
                    ));
                    spots.push((x, y, half));
                }
            }
            _ => {
                // A pocket: a rounded rectangle cut part way down.
                let a = g.range(5.0, 14.0);
                let b = g.range(5.0, 14.0);
                let rc = g.range(0.8, 0.45 * a.min(b));
                let d = g.range(0.3, 0.7) * t;
                let half = 0.5 * (a * a + b * b).sqrt();
                if let Some((x, y)) = place(g, l, w, half, gap, &spots) {
                    cut.push(format!(
                        "translate([{}, {}, {}]) linear_extrude({}) offset(r = {}) offset(delta = -{}) square([{}, {}], center = true);",
                        n(x),
                        n(y),
                        n(t - d),
                        n(d + 1.0),
                        n(rc),
                        n(rc),
                        n(a),
                        n(b)
                    ));
                    spots.push((x, y, half));
                }
            }
        }
    }
    (add, cut)
}

/// Which models a run makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Set {
    /// Stage F3's corpus, model for model (the stop rule's numbers).
    F3,
    /// F3's models, with about two in five replaced by stage F5a's
    /// kinds: mixed corners (one call over convex and concave edges,
    /// built in two passes), spheres, rotations, nested calls of unequal
    /// sizes and large boss rims (spindle tori).
    All,
}

/// Model `i` of the corpus seeded with `seed`.
fn case(seed: u64, i: usize, set: Set) -> Case {
    // A stream of its own decides, so the models it keeps are F3's.
    let mut pick = Rng(seed ^ (i as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ 0x5f5a);
    if set == Set::All && pick.chance(0.4) {
        return case_f5(seed, i);
    }
    let mut g = Rng(seed ^ (i as u64).wrapping_mul(0x2545_f491_4f6c_dd1d));
    let (family, solid) = solid(&mut g);
    call(&mut g, i, family, solid)
}

/// A body of F3's families with its features: (family, solid).
fn solid(g: &mut Rng) -> (&'static str, String) {
    let family = *g.pick(&["plate", "plate", "box", "rounded", "bracket"]);
    let l = g.range(25.0, 60.0);
    let w = g.range(20.0, 50.0);
    let (body, t) = match family {
        "plate" => {
            let t = g.range(3.0, 12.0);
            (format!("cube([{}, {}, {}]);", n(l), n(w), n(t)), t)
        }
        "box" => {
            let t = g.range(10.0, 30.0);
            (format!("cube([{}, {}, {}]);", n(l), n(w), n(t)), t)
        }
        "rounded" => {
            let t = g.range(5.0, 25.0);
            let r = g.range(2.0, 0.3 * l.min(w));
            let body = if g.chance(0.5) {
                format!(
                    "fillet_edges(r = {}, edges = \"|z\") cube([{}, {}, {}]);",
                    n(r),
                    n(l),
                    n(w),
                    n(t)
                )
            } else {
                format!(
                    "linear_extrude({}) offset(r = {}) offset(delta = -{}) square([{}, {}]);",
                    n(t),
                    n(r),
                    n(r),
                    n(l),
                    n(w)
                )
            };
            (body, t)
        }
        _ => {
            // An L bracket: a base and an upright leg, with round holes
            // through the leg along x as well.
            let t = g.range(3.0, 8.0);
            let h = g.range(15.0, 35.0);
            let mut body = format!(
                "cube([{}, {}, {}]); cube([{}, {}, {}]);",
                n(l),
                n(w),
                n(t),
                n(t),
                n(w),
                n(h)
            );
            let holes = g.below(3);
            for _ in 0..holes {
                let r = g.range(1.5, 0.2 * w.min(h));
                let y = g.range(r + 1.0, w - r - 1.0);
                let z = g.range(t + r + 1.0, (h - r - 1.0).max(t + r + 1.5));
                body = format!(
                    "difference() {{ union() {{ {body} }} translate([-1, {}, {}]) rotate([0, 90, 0]) cylinder(r = {}, h = {}); }}",
                    n(y),
                    n(z),
                    n(r),
                    n(t + 2.0)
                );
            }
            (body, t)
        }
    };
    let (add, cut) = features(g, l, w, t);
    let mut solid = if add.is_empty() {
        body
    } else {
        format!("union() {{ {body} {} }}", add.join(" "))
    };
    if !cut.is_empty() {
        solid = format!("difference() {{ {solid} {} }}", cut.join(" "));
    }
    (family, solid)
}

/// F3's call around `solid`: a selector from the atoms and a size.
fn call(g: &mut Rng, i: usize, family: &'static str, solid: String) -> Case {
    let chamfer = g.chance(0.25);
    let selector = *g.pick(&[
        "",
        ">z",
        "<z",
        "%circle",
        "%circle and >z",
        "%circle and convex",
        "%circle and concave",
        "%line and >z",
        "convex",
        "concave",
        ">z and convex",
        "|z",
        "#z and convex",
        ">x",
    ]);
    // Log-uniform from small to past what fits.
    let size = (g.range(0.25f64.ln(), 6.0f64.ln())).exp();
    let (module, arg) = if chamfer {
        ("chamfer_edges", "d")
    } else {
        ("fillet_edges", "r")
    };
    let edges = if selector.is_empty() {
        String::new()
    } else {
        format!(", edges = \"{selector}\"")
    };
    let text = format!("{module}({arg} = {}{edges})\n  {solid}\n", n(size));
    Case {
        id: format!("{i:04}"),
        family,
        text,
    }
}

/// A size drawn log-uniformly from `a` to `b`.
fn size(g: &mut Rng, a: f64, b: f64) -> f64 {
    g.range(a.ln(), b.ln()).exp()
}

/// A model of stage F5a's kinds (`docs/fillets.md`, section 15.6): one
/// call over convex and concave edges that meet (two passes), a sphere
/// on or in a plate, a rotated body, nested calls of unequal sizes, and
/// boss rims past half the boss's radius (spindle tori).
fn case_f5(seed: u64, i: usize) -> Case {
    let mut g = Rng(seed ^ (i as u64).wrapping_mul(0xd1b5_4a32_d192_ed03) ^ 0xf5a);
    let family = *g.pick(&["mixed", "mixed", "sphere", "rotated", "nested", "spindle"]);
    let chamfer = matches!(family, "mixed" | "rotated") && g.chance(0.2);
    let (module, arg) = if chamfer {
        ("chamfer_edges", "d")
    } else {
        ("fillet_edges", "r")
    };
    let edges = |s: &str| {
        if s.is_empty() {
            String::new()
        } else {
            format!(", edges = \"{s}\"")
        }
    };
    let text = match family {
        "mixed" => {
            let l = g.range(25.0, 50.0);
            let w = g.range(20.0, 40.0);
            let t = g.range(3.0, 10.0);
            let solid = match g.below(4) {
                0 => {
                    // A block on a plate.
                    let bx = g.range(5.0, 0.6 * l);
                    let by = g.range(5.0, 0.6 * w);
                    let x = g.range(1.0, l - bx - 1.0);
                    let y = g.range(1.0, w - by - 1.0);
                    format!(
                        "union() {{ cube([{}, {}, {}]); translate([{}, {}, 0]) cube([{}, {}, {}]); }}",
                        n(l),
                        n(w),
                        n(t),
                        n(x),
                        n(y),
                        n(bx),
                        n(by),
                        n(t + g.range(3.0, 15.0))
                    )
                }
                1 => {
                    // An L bracket.
                    let t2 = g.range(3.0, 10.0);
                    format!(
                        "union() {{ cube([{}, {}, {}]); cube([{}, {}, {}]); }}",
                        n(l),
                        n(w),
                        n(t),
                        n(l),
                        n(t2),
                        n(t + g.range(10.0, 30.0))
                    )
                }
                2 => {
                    // A rib along a plate, flush with its end faces.
                    let rw = g.range(2.0, 0.4 * w);
                    let y = g.range(1.0, w - rw - 1.0);
                    format!(
                        "union() {{ cube([{}, {}, {}]); translate([0, {}, 0]) cube([{}, {}, {}]); }}",
                        n(l),
                        n(w),
                        n(t),
                        n(y),
                        n(l),
                        n(rw),
                        n(t + g.range(2.0, 12.0))
                    )
                }
                _ => {
                    // A rectangular pocket: its rim convex, its walls and
                    // floor concave.
                    let a = g.range(5.0, 0.6 * l);
                    let b = g.range(5.0, 0.6 * w);
                    let x = g.range(2.0, l - a - 2.0);
                    let y = g.range(2.0, w - b - 2.0);
                    let d = g.range(0.3, 0.8) * t;
                    format!(
                        "difference() {{ cube([{}, {}, {}]); translate([{}, {}, {}]) cube([{}, {}, {}]); }}",
                        n(l),
                        n(w),
                        n(t),
                        n(x),
                        n(y),
                        n(t - d),
                        n(a),
                        n(b),
                        n(d + 1.0)
                    )
                }
            };
            let sel = *g.pick(&["", "", "not <z", "convex or concave", ">z or concave"]);
            format!(
                "{module}({arg} = {}{})\n  {solid}\n",
                n(size(&mut g, 0.25, 4.0)),
                edges(sel)
            )
        }
        "sphere" => {
            let l = g.range(25.0, 50.0);
            let w = g.range(20.0, 40.0);
            let t = g.range(3.0, 10.0);
            // The ball stays inside the plate's bottom and sides: only its
            // rim on the top face is an edge.
            let u = g.range(-0.6, 0.6);
            let room = 0.4 * l.min(w);
            let solid = if g.chance(0.5) {
                // A ball sunk in the plate, its centre `u` radii from the
                // top, its bottom at least 0.5 above the plate's.
                let rs = g.range(1.0, room).min((t - 0.5) / (1.0 - u));
                let x = g.range(rs + 1.0, l - rs - 1.0);
                let y = g.range(rs + 1.0, w - rs - 1.0);
                format!(
                    "union() {{ cube([{}, {}, {}]); translate([{}, {}, {}]) sphere(r = {}); }}",
                    n(l),
                    n(w),
                    n(t),
                    n(x),
                    n(y),
                    n(t + u * rs),
                    n(rs)
                )
            } else {
                // A spherical dimple, part way into the plate.
                let rs = g.range(2.0, room);
                let d = g.range(0.2, 0.8) * t.min(rs);
                let x = g.range(rs + 1.0, l - rs - 1.0);
                let y = g.range(rs + 1.0, w - rs - 1.0);
                format!(
                    "difference() {{ cube([{}, {}, {}]); translate([{}, {}, {}]) sphere(r = {}); }}",
                    n(l),
                    n(w),
                    n(t),
                    n(x),
                    n(y),
                    n(t - d + rs),
                    n(rs)
                )
            };
            let sel = *g.pick(&["", "%circle", "%circle and convex", "concave", ">z"]);
            format!(
                "{module}({arg} = {}{})\n  {solid}\n",
                n(size(&mut g, 0.25, 3.0)),
                edges(sel)
            )
        }
        "rotated" => {
            let (_, solid) = solid(&mut g);
            let a = [g.range(0.0, 90.0), g.range(0.0, 90.0), g.range(0.0, 90.0)];
            let sel = *g.pick(&["", "convex", "concave", "%circle", "%line and convex"]);
            format!(
                "{module}({arg} = {}{})\n  rotate([{}, {}, {}]) {solid}\n",
                n(size(&mut g, 0.25, 4.0)),
                edges(sel),
                n(a[0]),
                n(a[1]),
                n(a[2])
            )
        }
        "nested" => {
            let (_, solid) = solid(&mut g);
            let inner = *g.pick(&["|z", "concave", "%circle", "%line and convex"]);
            let outer = *g.pick(&["", ">z", "convex", "%circle and >z", "<z"]);
            format!(
                "fillet_edges(r = {}{})\n  fillet_edges(r = {}{})\n  {solid}\n",
                n(size(&mut g, 0.25, 3.0)),
                edges(outer),
                n(size(&mut g, 0.25, 4.0)),
                edges(inner)
            )
        }
        _ => {
            // Rims past half a boss's (or a blind hole's) radius.
            let rb = g.range(2.0, 7.0);
            let t = g.range(3.0, 8.0);
            let r = g.range(0.5, 0.97) * rb;
            if g.chance(0.6) {
                let h = g.range(r + 0.5, 3.0 * rb);
                let sel = *g.pick(&["%circle and >z", "%circle and convex", "%circle"]);
                format!(
                    "fillet_edges(r = {}{})\n  {{ translate([{}, {}, 0]) cube([{}, {}, {}]); cylinder(r = {}, h = {}); }}\n",
                    n(r),
                    edges(sel),
                    n(-rb - 6.0),
                    n(-rb - 6.0),
                    n(2.0 * rb + 12.0),
                    n(2.0 * rb + 12.0),
                    n(t),
                    n(rb),
                    n(t + h)
                )
            } else {
                let d = g.range(r + 0.5, r + 8.0);
                format!(
                    "fillet_edges(r = {}, edges = \"%circle and concave\")\n  difference() {{ translate([{}, {}, 0]) cube([{}, {}, {}]); translate([0, 0, {}]) cylinder(r = {}, h = {}); }}\n",
                    n(r),
                    n(-rb - 6.0),
                    n(-rb - 6.0),
                    n(2.0 * rb + 12.0),
                    n(2.0 * rb + 12.0),
                    n(d + 3.0),
                    n(3.0),
                    n(rb),
                    n(d + 1.0)
                )
            }
        }
    };
    Case {
        id: format!("{i:04}"),
        family,
        text,
    }
}

#[derive(Debug, Clone, Default)]
struct Run {
    killed: Option<String>,
    report: Option<Value>,
    step: Option<PathBuf>,
    occt: Option<Value>,
}

fn export(bin: &Path, opts: &FilletOptions, file: &Path, step: &Path) -> Run {
    let _ = std::fs::remove_file(step);
    let guard = BUDGET_KIB / opts.jobs.max(1) as u64;
    let mut cmd = Command::new(bin);
    cmd.args([
        "--enable", "fillet", "--enable", "exact", "--format", "json",
    ]);
    // `Limits::AGENT` (`crates/eval/src/limits.rs`), what a served or
    // agent's run gets; the memory limit a little under the guard, which
    // backs it up from outside, and the time limit the corpus's own.
    for l in AGENT_LIMITS {
        cmd.arg("--limit").arg(l);
    }
    cmd.arg("--limit")
        .arg(format!("memory={}", (guard * 7 / 8 / 1024).min(4096)))
        .arg("--limit")
        .arg(format!("time={}", opts.timeout.as_secs().clamp(1, 60)))
        .arg("-o")
        .arg(step)
        .arg(file)
        .current_dir(file.parent().unwrap_or(Path::new(".")))
        .stdin(Stdio::null())
        .stderr(Stdio::null());
    let (text, _, killed) = guarded_under(&mut cmd, opts.timeout + Duration::from_secs(10), guard);
    if killed.is_some() {
        return Run {
            killed,
            ..Run::default()
        };
    }
    let report = text
        .lines()
        .rev()
        .find(|l| l.starts_with('{'))
        .and_then(|l| serde_json::from_str(l).ok());
    if report.is_none() {
        return Run {
            killed: Some("no report (crashed?)".into()),
            ..Run::default()
        };
    }
    Run {
        killed: None,
        report,
        step: step.is_file().then(|| step.to_path_buf()),
        occt: None,
    }
}

fn occt(check: &Path, step: &Path, guard: u64) -> Option<Value> {
    let (text, _, killed) = guarded_under(
        Command::new(check).arg(step).stderr(Stdio::null()),
        Duration::from_secs(120),
        guard,
    );
    if let Some(k) = killed {
        return Some(json!({ "valid": false, "killed": k }));
    }
    text.lines()
        .find(|l| l.starts_with('{'))
        .and_then(|l| serde_json::from_str(l).ok())
}

/// The `fillet-*` diagnostics of a report.
fn fillet_diags(r: &Value) -> Vec<&Value> {
    r["diagnostics"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|d| d["code"].as_str().is_some_and(|c| c.starts_with("fillet-")))
        .collect()
}

/// `text` with the `replace` edit of a hint applied (1-based lines and
/// columns, end exclusive, in characters).
fn apply_edit(text: &str, edit: &Value) -> Option<String> {
    let pos = |p: &Value| -> Option<usize> {
        let line = p["line"].as_u64()? as usize;
        let col = p["column"].as_u64()? as usize;
        let mut off = 0;
        for (k, l) in text.split_inclusive('\n').enumerate() {
            if k + 1 == line {
                let byte = l.char_indices().nth(col - 1).map_or(l.len(), |(b, _)| b);
                return Some(off + byte);
            }
            off += l.len();
        }
        None
    };
    let a = pos(&edit["span"]["start"])?;
    let b = pos(&edit["span"]["end"])?;
    let new = edit["text"].as_str()?;
    (a <= b && b <= text.len()).then(|| format!("{}{new}{}", &text[..a], &text[b..]))
}

/// What a built model's export says: `None` when valid, else the failure
/// class and its message.
fn judge(run: &Run, occt_on: bool) -> Option<(String, String)> {
    let r = run.report.as_ref()?;
    let Some(e) = r.get("exact") else {
        return Some(("mesh".into(), "no 3D result to export".into()));
    };
    if !e["ok"].as_bool().unwrap_or(false) {
        let msg = e["error"].as_str().unwrap_or("").to_string();
        let class = if msg.contains("volume") || msg.contains("bounding box") {
            "mesh"
        } else if msg.contains("reconstruct") || msg.contains("topology") {
            "reconstruction"
        } else if msg.contains("invalid") {
            "validation"
        } else {
            "export"
        };
        return Some((class.into(), msg));
    }
    if e["exact_percent"].as_f64().unwrap_or(0.0) < 100.0 {
        let why = e["exact_attempt_faceted"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        return Some((
            "faceted".into(),
            format!("{}% exact ({why})", e["exact_percent"]),
        ));
    }
    if occt_on {
        let Some(o) = &run.occt else {
            return Some(("occt".into(), "no OCCT report".into()));
        };
        let num = |k: &str| o[k].as_f64().unwrap_or(f64::NAN);
        let ours = e["volume"].as_f64().unwrap_or(f64::NAN);
        let rel = (num("volume") - ours).abs() / ours.abs().max(1e-300);
        let ok = o["valid"].as_bool() == Some(true)
            && num("free_edges") == 0.0
            && num("shells") == num("closed_shells")
            && rel < 1e-6;
        if !ok {
            return Some(("occt".into(), o.to_string()));
        }
    }
    None
}

pub fn command(ctx: &Ctx, opts: &FilletOptions) -> Result<u8, String> {
    if opts.count == 0 || opts.count > MAX_COUNT {
        return Err(format!("--count must be 1 to {MAX_COUNT}"));
    }
    let work = ctx.repo.join("target/conformance/fillet");
    let cases_dir = work.join("cases");
    std::fs::create_dir_all(&cases_dir).map_err(|e| e.to_string())?;
    let bin = opts.binary.clone().unwrap_or_else(|| ctx.default_binary());
    if !bin.is_file() {
        return Err(format!(
            "no binary at {} (cargo build --release)",
            bin.display()
        ));
    }
    let occt_bin = opts
        .occt
        .clone()
        .or_else(|| std::env::var_os("MESHBREP_OCCT_CHECK").map(PathBuf::from))
        .filter(|p| p.is_file());
    let guard = BUDGET_KIB / opts.jobs.max(1) as u64;
    let list: Vec<Case> = (0..opts.count)
        .map(|i| case(opts.seed, i, opts.set))
        .filter(|c| {
            opts.filter
                .as_ref()
                .is_none_or(|f| c.id.contains(f.as_str()))
        })
        .collect();
    eprintln!(
        "fillet-corpus: {} models (seed {}), {} jobs, {} MB each{}",
        list.len(),
        opts.seed,
        opts.jobs,
        guard / 1024,
        if occt_bin.is_some() {
            ", OCCT read-back"
        } else {
            ""
        }
    );
    let next = Mutex::new(0usize);
    let results: Mutex<Vec<Option<Value>>> = Mutex::new(vec![None; list.len()]);
    let one = |c: &Case| -> Value {
        let file = cases_dir.join(format!("{}.scad", c.id));
        let step = cases_dir.join(format!("{}.step", c.id));
        if std::fs::write(&file, &c.text).is_err() {
            return json!({"outcome": "killed", "message": "could not write the model"});
        }
        let run_checked = |file: &Path, step: &Path| {
            let mut r = export(&bin, opts, file, step);
            if let (Some(chk), Some(s)) = (&occt_bin, &r.step) {
                r.occt = occt(chk, s, guard);
            }
            r
        };
        let r = run_checked(&file, &step);
        if let Some(k) = &r.killed {
            return json!({"outcome": "killed", "message": k});
        }
        let report = r.report.clone().unwrap_or(Value::Null);
        let diags = fillet_diags(&report);
        let errors: Vec<&&Value> = diags.iter().filter(|d| d["severity"] == "error").collect();
        let codes: Vec<String> = diags
            .iter()
            .filter_map(|d| d["code"].as_str().map(String::from))
            .collect();
        if errors.is_empty() {
            if codes.iter().any(|c| c == "fillet-no-edges") {
                return json!({"outcome": "none", "codes": codes});
            }
            let fail = judge(&r, occt_bin.is_some());
            return match fail {
                None => json!({"outcome": "valid", "codes": codes}),
                Some((class, msg)) => {
                    json!({"outcome": "failed", "class": class, "message": msg, "codes": codes})
                }
            };
        }
        let first = errors[0];
        let code = first["code"].as_str().unwrap_or("").to_string();
        if code == "fillet-failed" {
            return json!({"outcome": "failed", "class": "fillet-failed", "message": first["message"], "codes": codes});
        }
        let edit = first["hints"]
            .as_array()
            .into_iter()
            .flatten()
            .find_map(|h| h.get("replace"));
        if !(code == "fillet-too-large" || code == "fillet-overlap") || edit.is_none() {
            return json!({"outcome": "refused", "code": code, "message": first["message"], "codes": codes});
        }
        // Apply the fix and run again: it must build, and be valid. Each
        // hint fixes its own call; with nested calls a fix of one can
        // leave (or make) a size problem in another, whose own hint is
        // then applied, up to `FIX_ROUNDS` edits in all.
        let mut text = c.text.clone();
        let mut edit = edit.expect("checked").clone();
        let mut round = 0;
        loop {
            round += 1;
            let Some(fixed) = apply_edit(&text, &edit) else {
                return json!({"outcome": "failed", "class": "fix", "message": "the hint's edit does not apply"});
            };
            text = fixed;
            let ffile = cases_dir.join(format!("{}-fixed.scad", c.id));
            let fstep = cases_dir.join(format!("{}-fixed.step", c.id));
            if std::fs::write(&ffile, &text).is_err() {
                return json!({"outcome": "killed", "message": "could not write the model"});
            }
            let fr = run_checked(&ffile, &fstep);
            if let Some(k) = &fr.killed {
                return json!({"outcome": "killed", "message": k, "fixed": true});
            }
            let freport = fr.report.clone().unwrap_or(Value::Null);
            let fdiags = fillet_diags(&freport);
            let ferr: Vec<&&Value> = fdiags.iter().filter(|d| d["severity"] == "error").collect();
            if let Some(first) = ferr.first() {
                let next = first["hints"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find_map(|h| h.get("replace"));
                let size = matches!(
                    first["code"].as_str(),
                    Some("fillet-too-large" | "fillet-overlap")
                );
                let nested = c.text.matches("_edges(").count() > 1;
                match next {
                    Some(e) if size && round < FIX_ROUNDS && nested => {
                        edit = e.clone();
                        continue;
                    }
                    // The fixed call builds, and another call of the
                    // model is refused for a class this version does not
                    // build: the model is refused, as it would have been
                    // had that call's error come first.
                    _ if nested && !size && first["code"] != "fillet-failed" => {
                        return json!({"outcome": "refused", "code": first["code"], "message": first["message"], "fixed": true});
                    }
                    _ => {
                        return json!({"outcome": "failed", "class": "fix", "message": first["message"], "refused": code});
                    }
                }
            }
            return match judge(&fr, occt_bin.is_some()) {
                None => json!({"outcome": "fixed", "refused": code, "rounds": round}),
                Some((class, msg)) => {
                    json!({"outcome": "failed", "class": class, "message": msg, "refused": code, "fixed": true})
                }
            };
        }
    };
    std::thread::scope(|s| {
        for _ in 0..opts.jobs.max(1) {
            s.spawn(|| {
                loop {
                    let i = {
                        let mut n = next.lock().expect("next");
                        let i = *n;
                        *n += 1;
                        i
                    };
                    let Some(c) = list.get(i) else { break };
                    let mut v = one(c);
                    v["id"] = json!(c.id);
                    v["family"] = json!(c.family);
                    if v["outcome"] == "failed" || v["outcome"] == "killed" {
                        eprintln!(
                            "  {} {}: {} {} {}",
                            c.id,
                            c.family,
                            v["outcome"].as_str().unwrap_or(""),
                            v["class"].as_str().unwrap_or(""),
                            v["message"].as_str().unwrap_or("")
                        );
                    }
                    results.lock().expect("results")[i] = Some(v);
                }
            });
        }
    });
    let rows: Vec<Value> = results
        .into_inner()
        .expect("results")
        .into_iter()
        .map(|v| v.unwrap_or(Value::Null))
        .collect();
    let count = |o: &str| rows.iter().filter(|v| v["outcome"] == o).count();
    let (valid, fixed, failed, refused, killed, none) = (
        count("valid"),
        count("fixed"),
        count("failed"),
        count("refused"),
        count("killed"),
        count("none"),
    );
    let supported = valid + fixed + failed;
    let mesh = rows
        .iter()
        .filter(|v| v["outcome"] == "failed" && v["class"] == "mesh")
        .count();
    let mut classes: BTreeMap<String, usize> = BTreeMap::new();
    for v in rows.iter().filter(|v| v["outcome"] == "failed") {
        *classes
            .entry(v["class"].as_str().unwrap_or("?").to_string())
            .or_default() += 1;
    }
    let mut refusals: BTreeMap<String, usize> = BTreeMap::new();
    for v in rows.iter().filter(|v| v["outcome"] == "refused") {
        *refusals
            .entry(v["code"].as_str().unwrap_or("?").to_string())
            .or_default() += 1;
    }
    let pct = |a: usize, b: usize| {
        if b == 0 {
            "-".to_string()
        } else {
            format!("{:.1}%", 100.0 * a as f64 / b as f64)
        }
    };
    println!(
        "models {}  valid {valid}  fixed {fixed}  failed {failed}  refused {refused}  no edges {none}  killed {killed}",
        rows.len()
    );
    println!(
        "supported class: {supported}; valid, all exact{}: {} ({})",
        if occt_bin.is_some() {
            ", read back by OCCT"
        } else {
            ""
        },
        valid + fixed,
        pct(valid + fixed, supported)
    );
    println!(
        "mesh failures: {mesh} ({} of the supported class)",
        pct(mesh, supported)
    );
    println!("failure classes: {classes:?}");
    println!("refused: {refusals:?}");
    let out = json!({
        "seed": opts.seed,
        "count": opts.count,
        "occt": occt_bin.is_some(),
        "summary": {
            "valid": valid, "fixed": fixed, "failed": failed, "refused": refused,
            "none": none, "killed": killed, "supported": supported, "mesh": mesh,
            "classes": classes, "refusals": refusals,
        },
        "cases": rows,
    });
    let path = work.join(format!("results-{}-{}.json", opts.seed, opts.count));
    std::fs::write(
        &path,
        serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    eprintln!("results: {}", path.display());
    Ok(u8::from(failed + killed > 0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_corpus_is_the_same_for_the_same_seed() {
        let a: Vec<String> = (0..50).map(|i| case(7, i, Set::All).text).collect();
        let b: Vec<String> = (0..50).map(|i| case(7, i, Set::All).text).collect();
        assert_eq!(a, b);
        let c: Vec<String> = (0..50).map(|i| case(8, i, Set::All).text).collect();
        assert_ne!(a, c);
        // Every family turns up.
        let fams: std::collections::BTreeSet<&str> =
            (0..200).map(|i| case(1, i, Set::F3).family).collect();
        assert_eq!(fams.len(), 4, "{fams:?}");
        let fams: std::collections::BTreeSet<&str> =
            (0..400).map(|i| case(1, i, Set::All).family).collect();
        assert_eq!(fams.len(), 9, "{fams:?}");
        // The models the full set keeps are F3's.
        let kept = (0..400)
            .filter(|&i| case(1, i, Set::All).text == case(1, i, Set::F3).text)
            .count();
        assert!((200..300).contains(&kept), "{kept}");
    }

    #[test]
    fn a_hint_edit_applies_by_line_and_column() {
        let text = "fillet_edges(r = 4, edges = \"%circle\")\n  cylinder(r = 6, h = 20);\n";
        let edit = json!({"span": {"start": {"line": 1, "column": 18}, "end": {"line": 1, "column": 19}}, "text": "2.99"});
        assert_eq!(
            apply_edit(text, &edit).unwrap(),
            "fillet_edges(r = 2.99, edges = \"%circle\")\n  cylinder(r = 6, h = 20);\n"
        );
    }
}
