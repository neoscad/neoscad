//! Solves every case of the validation corpus and prints one JSON object
//! per line: the solution, the diagnosis and the time taken. The
//! differential oracle (scripts/sketch-oracle.py) compares this with
//! SolveSpace's answers.
//!
//!     cargo run --release -p sketch-solver --example corpus [-- FILTER]
//!
//! With FILTER, only cases whose name contains it. CORPUS_FILE=PATH reads
//! one corpus file instead of tests/corpus. Set CORPUS_START=perturbed
//! to start from the perturbed drawing instead.

#[path = "../tests/support/corpus.rs"]
mod corpus;

use std::path::Path;
use std::time::Instant;

use corpus::{Start, build, check, distance_to_solved, load, load_files};
use serde_json::{Value, json};
use sketch_solver::{Entity, Source, Status};

fn source(s: &Source) -> Value {
    match s {
        Source::Constraint(c) => json!(c.index()),
        Source::Arc(e) => json!(format!("arc {}", e.index())),
    }
}

fn main() {
    let filter = std::env::args().nth(1).unwrap_or_default();
    let start = match std::env::var("CORPUS_START").as_deref() {
        Ok("perturbed") => Start::Perturbed,
        _ => Start::Drawing,
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let files = match std::env::var_os("CORPUS_FILE") {
        Some(p) => load_files(vec![p.into()]),
        None => load(&root),
    };
    for f in files {
        for case in &f.cases {
            let name = case["name"].as_str().unwrap_or("?");
            if !name.contains(&filter) {
                continue;
            }
            let b = match build(case, start, &[]) {
                Ok(b) => b,
                Err(e) => {
                    println!("{}", json!({"name": name, "error": e}));
                    continue;
                }
            };
            // Best of several runs, for a stable time.
            let mut best = f64::INFINITY;
            let mut sol = b.sketch.solve();
            for _ in 0..20 {
                let t = Instant::now();
                sol = b.sketch.solve();
                best = best.min(t.elapsed().as_secs_f64());
            }
            let points: serde_json::Map<String, Value> = b
                .names
                .iter()
                .filter_map(|(n, id)| {
                    sol.point(*id)
                        .map(|p| (n.clone(), json!(p)))
                        .or_else(|| sol.radius(*id).map(|r| (n.clone(), json!(r))))
                })
                .collect();
            // Where each point started, so the oracle starts SolveSpace
            // from the same drawing.
            let start: serde_json::Map<String, Value> = b
                .names
                .iter()
                .filter_map(|(n, id)| match b.sketch.entity(*id) {
                    Some(Entity::Point { guess: Some(g) }) => Some((n.clone(), json!(g))),
                    _ => None,
                })
                .collect();
            let failures = check(&b, &sol, &case["expect"]);
            println!(
                "{}",
                json!({
                    "name": name,
                    "status": if sol.status == Status::Solved { "solved" } else { "not_converged" },
                    "unknowns": sol.unknowns,
                    "equations": sol.equations,
                    "dof": sol.dof,
                    "redundant": sol.redundant.iter().map(|d| source(&d.source)).collect::<Vec<_>>(),
                    "conflicts": sol.conflicts.iter().map(|d| source(&d.source)).collect::<Vec<_>>(),
                    "flipped": format!("{:?}", sol.flipped),
                    "iterations": sol.iterations,
                    "continuation": sol.continuation,
                    "residual": sol.residual,
                    "seconds": best,
                    "points": points,
                    "start": start,
                    "expect_failures": failures,
                    "distance_to_upstream": distance_to_solved(&b, &sol, case),
                })
            );
        }
    }
}
