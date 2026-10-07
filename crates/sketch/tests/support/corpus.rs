//! Reading the validation corpus (tests/corpus/README.md) and checking a
//! solution against a case's expectations. Shared by the corpus test and
//! the `corpus` example that the differential oracle runs.

#![allow(dead_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use sketch_solver::{Along, Constraint, EntityId, Pair, Sketch, Solution, Status};

/// One corpus file.
pub struct File {
    pub path: PathBuf,
    pub origin: String,
    pub commit: String,
    pub licence: String,
    pub cases: Vec<Value>,
}

/// Every `*.json` under `root`'s subdirectories, sorted by path.
pub fn load(root: &Path) -> Vec<File> {
    let mut paths = Vec::new();
    for dir in std::fs::read_dir(root).expect("corpus directory") {
        let dir = dir.expect("entry").path();
        if !dir.is_dir() {
            continue;
        }
        for f in std::fs::read_dir(&dir).expect("origin directory") {
            let f = f.expect("entry").path();
            if f.extension().is_some_and(|e| e == "json") {
                paths.push(f);
            }
        }
    }
    paths.sort();
    load_files(paths)
}

/// The given corpus files.
pub fn load_files(paths: Vec<PathBuf>) -> Vec<File> {
    paths
        .into_iter()
        .map(|path| {
            let v: Value =
                serde_json::from_slice(&std::fs::read(&path).expect("read")).expect("json");
            let s = |k: &str| v[k].as_str().unwrap_or_default().to_string();
            File {
                origin: s("origin"),
                commit: s("commit"),
                licence: s("licence"),
                cases: v["cases"].as_array().cloned().unwrap_or_default(),
                path,
            }
        })
        .collect()
}

/// A case built into a sketch, with its names.
pub struct Built {
    pub sketch: Sketch,
    pub names: BTreeMap<String, EntityId>,
}

/// Where to start the points.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Start {
    /// The case's drawing.
    Drawing,
    /// The drawing moved by a deterministic amount (up to 5% of its size)
    /// for every point that is not fixed, so a sketch saved at its
    /// solution has something to solve.
    Perturbed,
}

fn num(v: &Value) -> f64 {
    v.as_f64().expect("number")
}

fn xy(v: &Value) -> [f64; 2] {
    [num(&v[0]), num(&v[1])]
}

/// SplitMix64, for the perturbation.
fn mix(mut z: u64) -> f64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^= z >> 31;
    (z >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
}

/// Build `case` with the constraint values changed by `set` (index,
/// value).
pub fn build(case: &Value, start: Start, set: &[(usize, f64)]) -> Result<Built, String> {
    let mut s = Sketch::new();
    let mut names: BTreeMap<String, EntityId> = BTreeMap::new();
    let constraints = case["constraints"].as_array().cloned().unwrap_or_default();
    // Points named in a `fix` stay where drawn when perturbing.
    let mut fixed: Vec<String> = Vec::new();
    for c in &constraints {
        if c[0] == "fix" {
            fixed.push(c[1].as_str().unwrap().to_string());
        }
    }
    let entities = case["entities"].as_array().cloned().unwrap_or_default();
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for e in &entities {
        if e[1] == "point" {
            let p = xy(&e[2]);
            for k in 0..2 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
    }
    let size = ((hi[0] - lo[0]).powi(2) + (hi[1] - lo[1]).powi(2))
        .sqrt()
        .max(1.0);
    // Lines that are fixed fix their points too.
    for e in &entities {
        if e[1] == "line" && fixed.iter().any(|f| f == e[0].as_str().unwrap()) {
            fixed.push(e[2].as_str().unwrap().to_string());
            fixed.push(e[3].as_str().unwrap().to_string());
        }
    }
    let get = |names: &BTreeMap<String, EntityId>, v: &Value| -> Result<EntityId, String> {
        let n = v.as_str().ok_or("entity name")?;
        names
            .get(n)
            .copied()
            .ok_or_else(|| format!("no entity {n}"))
    };
    for (i, e) in entities.iter().enumerate() {
        let name = e[0].as_str().unwrap().to_string();
        let id = match e[1].as_str().unwrap() {
            "point" => {
                let mut p = xy(&e[2]);
                if start == Start::Perturbed && !fixed.contains(&name) {
                    p[0] += 0.05 * size * mix(2 * i as u64);
                    p[1] += 0.05 * size * mix(2 * i as u64 + 1);
                }
                s.point(Some(p))
            }
            "line" => s.line(get(&names, &e[2])?, get(&names, &e[3])?),
            "arc" => s.arc(
                get(&names, &e[2])?,
                get(&names, &e[3])?,
                get(&names, &e[4])?,
                e.get(5).is_some_and(|v| v == "cw"),
            ),
            "circle" => s.circle(get(&names, &e[2])?, Some(num(&e[3]))),
            k => return Err(format!("unknown entity kind {k}")),
        }
        .map_err(|e| format!("{name}: {e}"))?;
        names.insert(name, id);
    }
    for c in case["construction"].as_array().into_iter().flatten() {
        let id = get(&names, c)?;
        s.set_construction(id, true).map_err(|e| e.to_string())?;
    }
    for (i, c) in constraints.iter().enumerate() {
        let a = c.as_array().unwrap();
        let e = |k: usize| get(&names, &a[k]);
        // A constraint's value, or the one `set` gives it.
        let v = |k: usize| {
            set.iter()
                .find(|(j, _)| *j == i)
                .map(|(_, v)| *v)
                .unwrap_or_else(|| num(&a[k]))
        };
        let con = match a[0].as_str().unwrap() {
            "fix" => Constraint::Fix {
                entity: e(1)?,
                at: a.get(2).map(xy),
            },
            "coincident" => Constraint::Coincident(e(1)?, e(2)?),
            "on" => Constraint::On {
                point: e(1)?,
                curve: e(2)?,
            },
            "horizontal" | "vertical" => {
                let pair = if a.len() == 3 {
                    Pair::Points(e(1)?, e(2)?)
                } else {
                    Pair::Line(e(1)?)
                };
                if a[0] == "horizontal" {
                    Constraint::Horizontal(pair)
                } else {
                    Constraint::Vertical(pair)
                }
            }
            "parallel" => Constraint::Parallel(e(1)?, e(2)?),
            "perpendicular" => Constraint::Perpendicular(e(1)?, e(2)?),
            "tangent" => Constraint::Tangent(e(1)?, e(2)?),
            k @ ("distance" | "distance_x" | "distance_y") => Constraint::Distance {
                a: e(1)?,
                b: e(2)?,
                value: v(3),
                along: match k {
                    "distance" => Along::Direct,
                    "distance_x" => Along::X,
                    _ => Along::Y,
                },
            },
            "length" => Constraint::Length {
                line: e(1)?,
                value: v(2),
            },
            "radius" => Constraint::Radius {
                curve: e(1)?,
                value: v(2),
            },
            "diameter" => Constraint::Diameter {
                curve: e(1)?,
                value: v(2),
            },
            "angle" => Constraint::Angle {
                from: e(1)?,
                to: e(2)?,
                degrees: v(3),
            },
            "sweep" => Constraint::Sweep {
                arc: e(1)?,
                degrees: v(2),
            },
            "equal" => Constraint::Equal(e(1)?, e(2)?),
            "midpoint" => Constraint::Midpoint {
                point: e(1)?,
                line: e(2)?,
            },
            "symmetric" => Constraint::Symmetric {
                a: e(1)?,
                b: e(2)?,
                about: e(3)?,
            },
            k => return Err(format!("unknown constraint kind {k}")),
        };
        s.add(con)
            .map_err(|err| format!("constraint {i} ({}): {err}", a[0]))?;
    }
    Ok(Built { sketch: s, names })
}

/// The ways `sol` differs from `expect`.
pub fn check(b: &Built, sol: &Solution, expect: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let tol = expect["tol"].as_f64().unwrap_or(1e-9);
    let pt = |n: &str| b.names.get(n).and_then(|id| sol.point(*id));
    match expect["status"].as_str().unwrap_or("solved") {
        "solved" if sol.status != Status::Solved => out.push(format!(
            "status {:?}, residual {:e}",
            sol.status, sol.residual
        )),
        "not_converged" if sol.status != Status::NotConverged => {
            out.push("expected no convergence".into())
        }
        _ => {}
    }
    if let Some(d) = expect["dof"].as_u64()
        && sol.dof as u64 != d
    {
        out.push(format!("dof {} (expected {d})", sol.dof));
    }
    // No flips, except of kinds a case allows (with its reason in the
    // corpus file).
    let allowed: Vec<&str> = expect["allow_flipped"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let flipped: Vec<_> = sol
        .flipped
        .iter()
        .filter(|f| !allowed.iter().any(|a| format!("{f:?}").starts_with(a)))
        .collect();
    if !flipped.is_empty() {
        out.push(format!("flipped {flipped:?}"));
    }
    for (name, want) in expect["points"].as_object().into_iter().flatten() {
        let w = xy(want);
        match pt(name) {
            Some(p) if (p[0] - w[0]).abs() <= tol && (p[1] - w[1]).abs() <= tol => {}
            got => out.push(format!("{name} at {got:?}, expected {w:?}")),
        }
    }
    for (name, want) in expect["radii"].as_object().into_iter().flatten() {
        let got = b.names.get(name).and_then(|id| sol.radius(*id));
        if !got.is_some_and(|r| (r - num(want)).abs() <= tol) {
            out.push(format!("{name} radius {got:?}, expected {want}"));
        }
    }
    for c in expect["checks"].as_array().into_iter().flatten() {
        let a = c.as_array().unwrap();
        let kind = a[0].as_str().unwrap();
        let p = |k: usize| pt(a[k].as_str().unwrap()).unwrap_or([f64::NAN; 2]);
        let ok = match kind {
            "x" | "y" => {
                let t = a.get(3).and_then(Value::as_f64).unwrap_or(1e-9);
                (p(1)[usize::from(kind == "y")] - num(&a[2])).abs() <= t
            }
            "x<" => p(1)[0] < num(&a[2]),
            "x>" => p(1)[0] > num(&a[2]),
            "y<" => p(1)[1] < num(&a[2]),
            "y>" => p(1)[1] > num(&a[2]),
            "dx" | "dy" => {
                let k = usize::from(kind == "dy");
                (p(2)[k] - p(1)[k] - num(&a[3])).abs() <= 1e-9
            }
            "cw" | "ccw" => {
                let (pa, pb, pc) = (p(1), p(2), p(3));
                let cr = (pb[0] - pa[0]) * (pc[1] - pa[1]) - (pb[1] - pa[1]) * (pc[0] - pa[0]);
                if kind == "cw" { cr < 0.0 } else { cr > 0.0 }
            }
            k => panic!("unknown check {k}"),
        };
        if !ok {
            out.push(format!("check {c} failed"));
        }
    }
    out
}

/// The largest distance between our solution and upstream's at the
/// points both have (the `solved` map), or `None` without one.
pub fn distance_to_solved(b: &Built, sol: &Solution, case: &Value) -> Option<f64> {
    let solved = case["solved"].as_object()?;
    let mut worst = 0.0f64;
    for (name, v) in solved {
        let id = b.names[name];
        let d = match v {
            Value::Array(_) => {
                let (p, w) = (sol.point(id)?, xy(v));
                (p[0] - w[0]).abs().max((p[1] - w[1]).abs())
            }
            _ => (sol.radius(id)? - num(v)).abs(),
        };
        worst = worst.max(d);
    }
    Some(worst)
}
