//! OCCT read-back of every case at every resolution, when the oracle is
//! available (`oracle/build.sh`; set `MESHBREP_OCCT_CHECK` to the built
//! `check`). Skipped otherwise: OCCT is a test tool, never a dependency.
//!
//! Each file must read back as one valid solid (`BRepCheck_Analyzer`)
//! with closed shells, no free edges, tolerances no larger than 1e-6 after
//! reading, and OCCT's own volume within 1e-6 of the reference.

mod common;

use common::*;
use meshbrep::{StepOptions, measure, write_step};
use std::path::PathBuf;
use std::process::Command;

fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let k = format!("\"{key}\":");
    let rest = &json[json.find(&k)? + k.len()..];
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    Some(&rest[..end])
}

#[test]
fn occt_reads_every_case_back() {
    let Some(check) = std::env::var_os("MESHBREP_OCCT_CHECK") else {
        eprintln!("skipped: set MESHBREP_OCCT_CHECK to oracle/build.sh's check");
        return;
    };
    let dir: PathBuf = std::env::temp_dir().join(format!("meshbrep-occt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut files = Vec::new();
    let mut expect = Vec::new();
    for name in FIFTEEN.iter().chain(&IDIOMS).chain(&MORE).chain(&TORI) {
        for (ri, res) in RESOLUTIONS.iter().enumerate() {
            let (_, _, reference, brep, _) = build(name, *res);
            let brep = brep.expect("reconstruct");
            let ours = measure(&brep).expect("measure").volume;
            let path = dir.join(format!("{name}-{ri}.step"));
            std::fs::write(&path, write_step(&brep, &StepOptions::default())).unwrap();
            files.push(path);
            expect.push((format!("{name} {}", res.name()), reference.unwrap_or(ours)));
        }
    }
    let out = Command::new(&check)
        .args(&files)
        .output()
        .expect("run the oracle");
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().filter(|l| l.starts_with('{')).collect();
    assert_eq!(lines.len(), files.len(), "oracle output:\n{text}");
    let mut failures = Vec::new();
    let mut worst = 0.0f64;
    for (line, (label, reference)) in lines.iter().zip(&expect) {
        let num = |k: &str| {
            field(line, k)
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(f64::NAN)
        };
        let volume = num("volume");
        let rel = (volume - reference).abs() / reference;
        worst = worst.max(rel);
        let ok = field(line, "valid") == Some("true")
            && num("solids") == 1.0
            && num("free_edges") == 0.0
            && num("shells") == num("closed_shells")
            && num("max_tol") <= 1e-6
            && rel < 1e-6;
        eprintln!(
            "{label:12} {} vol {volume:.10} rel {rel:.1e} tol {}",
            if ok { "ok  " } else { "FAIL" },
            num("max_tol")
        );
        if !ok {
            failures.push(format!("{label}: {line}"));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!(
        "{} files, worst relative volume error {worst:.1e}",
        lines.len()
    );
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
