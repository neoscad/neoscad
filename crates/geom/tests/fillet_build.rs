//! Fillet and chamfer blends, stage F2 of `docs/fillets.md`: the golden
//! models of `conformance/extensions/fillet` against their closed-form
//! volumes, as meshes and as exact STEP (read back by OCCT when
//! `MESHBREP_OCCT_CHECK` names the oracle); the same bytes at 1, 2 and 8
//! threads and with a warm cache; and the checks that refuse a call
//! before any boolean, with the fixes their hints carry.

use std::path::{Path, PathBuf};

use geom::exact::{ExactOptions, meshbrep};
use geom::fillet::{self, Fix, Status};
use geom::{RenderOptions, Renderer};
use lang::diag::DiagCode;

fn cases() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../conformance/extensions/fillet")
}

fn evaluate(src: &str) -> eval::Evaluation {
    let path = PathBuf::from("/nonexistent/test.scad");
    let mut text = src.as_bytes().to_vec();
    text.extend_from_slice(b"\n\x03\n");
    let program = lang::parse_file(path, text);
    assert!(!program.has_syntax_errors(), "syntax error in {src}");
    let mut out = eval::Collect::default();
    let opts = eval::Options {
        extensions: eval::Extensions::NONE
            .with(eval::Extension::Fillet)
            .with(eval::Extension::Exact),
        ..eval::Options::default()
    };
    eval::with_stack(eval::DEFAULT_THREAD_STACK, || {
        eval::evaluate(
            &program,
            &[],
            &[],
            PathBuf::from("/nonexistent"),
            &opts,
            &mut out,
        )
    })
}

/// The normal render's volume and the exact export (STEP text and its
/// B-rep volume) of `src`, with `renderer`.
fn render(renderer: &Renderer, src: &str) -> (f64, Result<(String, f64), String>) {
    let ev = evaluate(src);
    let keys = eval::dump::Keys::new(&ev.root, &lang::loader::StdFs);
    let opts = RenderOptions::default();
    let normal = renderer
        .render(&ev.root, &keys, opts.clone())
        .expect("renders")
        .geometry
        .expect("geometry");
    let volume = match &normal {
        geom::Geometry::Manifold(m) => m.manifold.volume(),
        other => panic!("not a solid: {other:?}"),
    };
    let x = ExactOptions {
        step: meshbrep::StepOptions {
            product_name: "test".into(),
            file_name: "test.step".into(),
            originating_system: "NeoSCAD".into(),
            ..Default::default()
        },
        clock: None,
    };
    let exact = geom::exact::export_step(renderer, &ev.root, &keys, &opts, &normal, &x)
        .map(|e| (e.step, e.stats.volume))
        .map_err(|f| f.message);
    (volume, exact)
}

/// Every golden case: (name, source, closed-form volume).
fn golden() -> Vec<(String, String, f64)> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(cases())
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "scad"))
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|f| {
            let text = std::fs::read_to_string(&f).unwrap();
            let v = text
                .lines()
                .find_map(|l| l.strip_prefix("// volume: "))
                .and_then(|v| v.trim().parse().ok())
                .unwrap_or_else(|| panic!("{}: no volume", f.display()));
            let name = f.file_stem().unwrap().to_string_lossy().into_owned();
            (name, text, v)
        })
        .collect()
}

/// The exact export holds the closed form; the mesh is within what its
/// polygonal arcs account for, and closes in on it at a fine `$fs`.
#[test]
fn golden_cases_match_their_closed_forms() {
    let mut worst: f64 = 0.0;
    for (name, src, want) in golden() {
        let (mesh, exact) = render(&Renderer::new(), &src);
        let (step, got) = exact.unwrap_or_else(|e| panic!("{name}: export failed: {e}"));
        let rel = (got - want).abs() / want;
        // A sphere patch's volume is integrated over a trimmed sphere,
        // the one face whose quadrature leaves more than rounding.
        let tol = if step.contains("SPHERICAL_SURFACE") {
            1e-7
        } else {
            1e-9
        };
        assert!(rel < tol, "{name}: exact {got} vs {want} ({rel:.1e})");
        worst = worst.max(rel);
        // At OpenSCAD's defaults a small radius gets few segments (r = 2:
        // two per quarter circle, as `circle(2)` has 7 sides), and the
        // inscribed arcs take up to a few percent more of a small part.
        assert!(
            (mesh - want).abs() / want < 0.05,
            "{name}: mesh {mesh} vs {want}"
        );
        // Finer arcs close in on the closed form.
        let (fine, _) = render(&Renderer::new(), &format!("$fa = 2; $fs = 0.05;\n{src}"));
        assert!(
            (fine - want).abs() / want < 2e-4,
            "{name}: fine mesh {fine} vs {want}"
        );
    }
    eprintln!("worst relative exact-volume error {worst:.1e}");
}

fn field<'a>(json: &'a str, key: &str) -> Option<&'a str> {
    let k = format!("\"{key}\":");
    let rest = &json[json.find(&k)? + k.len()..];
    let end = rest.find([',', '}']).unwrap_or(rest.len());
    Some(&rest[..end])
}

/// OCCT reads every golden STEP file back as one valid closed solid with
/// the closed form's volume (skipped without the oracle).
#[test]
fn occt_reads_the_golden_cases_back() {
    let Some(check) = std::env::var_os("MESHBREP_OCCT_CHECK") else {
        eprintln!("skipped: set MESHBREP_OCCT_CHECK to crates/meshbrep/oracle/build.sh's check");
        return;
    };
    let dir = std::env::temp_dir().join(format!("neoscad-fillet-occt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut files = Vec::new();
    let mut wants = Vec::new();
    for (name, src, want) in golden() {
        let (_, exact) = render(&Renderer::new(), &src);
        let (step, _) = exact.unwrap_or_else(|e| panic!("{name}: {e}"));
        let path = dir.join(format!("{name}.step"));
        std::fs::write(&path, step).unwrap();
        files.push(path);
        wants.push((name, want));
    }
    let out = std::process::Command::new(&check)
        .args(&files)
        .output()
        .expect("run the oracle");
    let text = String::from_utf8_lossy(&out.stdout);
    let lines: Vec<&str> = text.lines().filter(|l| l.starts_with('{')).collect();
    assert_eq!(lines.len(), files.len(), "{text}");
    let mut failures = Vec::new();
    for (line, (name, want)) in lines.iter().zip(&wants) {
        let num = |k: &str| {
            field(line, k)
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(f64::NAN)
        };
        let rel = (num("volume") - want).abs() / want;
        let ok = field(line, "valid") == Some("true")
            && num("solids") == 1.0
            && num("free_edges") == 0.0
            && num("shells") == num("closed_shells")
            && rel < 1e-6;
        eprintln!(
            "{name:18} {} occt volume rel {rel:.1e}",
            if ok { "ok" } else { "FAIL" }
        );
        if !ok {
            failures.push(format!("{name}: {line}"));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    assert!(failures.is_empty(), "{failures:#?}");
}

/// The same STEP bytes and the same mesh at 1, 2 and 8 threads, and from
/// a cold cache, a warm one, and one warmed by other models first.
#[test]
fn bytes_are_the_same_at_any_thread_count_and_cache_state() {
    let models: Vec<String> = golden()
        .into_iter()
        .filter(|(n, _, _)| {
            [
                "l_bracket",
                "cube_all",
                "lid_lip_lines",
                "boss_base_mitre",
                "plane_cylinder",
            ]
            .contains(&n.as_str())
        })
        .map(|(_, s, _)| s)
        .collect();
    assert_eq!(models.len(), 5);
    let both = |r: &Renderer, m: &String| {
        let (mesh, exact) = render(r, m);
        (mesh.to_bits(), exact.unwrap().0)
    };
    let first: Vec<_> = models.iter().map(|m| both(&Renderer::new(), m)).collect();
    for threads in [1, 2, 8] {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .stack_size(eval::DEFAULT_THREAD_STACK)
            .build()
            .unwrap();
        let again: Vec<_> =
            pool.install(|| models.iter().map(|m| both(&Renderer::new(), m)).collect());
        assert!(again == first, "output differs on {threads} threads");
    }
    let r = Renderer::new();
    for (m, want) in models.iter().zip(&first).rev() {
        assert!(&both(&r, m) == want, "warm differs");
    }
    for (m, want) in models.iter().zip(&first) {
        assert!(&both(&r, m) == want, "warm differs, second pass");
    }
}

/// The plan of the one fillet call in `src`, with the diagnostics after
/// the boolean appended as the hosts report them.
fn plan(src: &str) -> fillet::Plan {
    let ev = evaluate(src);
    let keys = eval::dump::Keys::new(&ev.root, &lang::loader::StdFs);
    let r = Renderer::new();
    let opts = RenderOptions::default();
    r.render(&ev.root, &keys, opts.clone()).expect("renders");
    let mut stack = vec![&ev.root];
    let mut found = None;
    while let Some(n) = stack.pop() {
        if matches!(n.kind, eval::node::NodeKind::Fillet(_)) {
            found = Some(n);
            break;
        }
        stack.extend(n.children.iter());
    }
    let n = found.expect("a fillet call");
    let mut p = fillet::plan(&r, n, &keys, &opts).unwrap();
    let after = fillet::blend_diags(&r, n, &keys, &opts, &p);
    p.diags.extend(after);
    p
}

fn codes(p: &fillet::Plan) -> Vec<DiagCode> {
    p.diags.iter().map(|d| d.code).collect()
}

/// The size checks refuse a call before any boolean, and the size their
/// hint's edit writes is one the checks pass: the edit applied, the call
/// builds.
#[test]
fn every_size_fix_makes_the_call_build() {
    let cases = [
        // Two fillets on a plate 4 thick need 3 + 3.
        (
            "fillet_edges(r = R, edges = \"|y\") cube([20, 10, 4]);",
            3.0,
            DiagCode::FilletOverlap,
        ),
        // One fillet wider than the face beside it.
        (
            "fillet_edges(r = R, edges = \"|y and >z and <x\") cube([20, 10, 4]);",
            5.0,
            DiagCode::FilletTooLarge,
        ),
        // A convex fillet larger than the boss it runs along.
        (
            "fillet_edges(r = R, edges = \"convex and |z\") intersection() { cylinder(r = 3, h = 10); translate([-5, 0, 0]) cube([10, 5, 10]); }",
            4.0,
            DiagCode::FilletTooLarge,
        ),
        // A chamfer longer than a face.
        (
            "chamfer_edges(d = R, edges = \"|z and <x and <y\") cube([2, 10, 5]);",
            3.0,
            DiagCode::FilletTooLarge,
        ),
    ];
    for (src, size, code) in cases {
        let p = plan(&src.replace('R', &size.to_string()));
        let d = p
            .diags
            .iter()
            .find(|d| d.code == code)
            .unwrap_or_else(|| panic!("{src}: no {code:?} in {:?}", p.diags));
        assert!(p.build.is_none(), "{src}: built anyway");
        let Some(Fix::Size(fix)) = d.fix else {
            panic!("{src}: no size fix in {d:?}");
        };
        assert!(fix > 0.0 && fix < size, "{src}: fix {fix}");
        let text = fillet::number_text(fix);
        assert!(d.hints[0].contains(&text), "{:?}", d.hints);
        let fixed = plan(&src.replace('R', &text));
        assert_eq!(
            fixed.status,
            Status::Built,
            "{src} with {text}: {:?}",
            fixed.diags
        );
        assert!(
            !fixed
                .diags
                .iter()
                .any(|d| d.severity == lang::diag::Severity::Error),
            "{src} with {text}: {:?}",
            fixed.diags
        );
    }
}

/// Convex and concave edges at one vertex: refused, with the nested
/// rewrite; and the nested calls, concave first, build.
#[test]
fn a_mixed_corner_is_refused_and_two_passes_build() {
    let block = "union() { cube([20, 20, 5]); translate([5, 5, 0]) cube([10, 10, 15]); }";
    let p = plan(&format!("fillet_edges(r = 1) {block}"));
    assert_eq!(p.status, Status::UnsupportedVertex);
    let d = &p.diags[p.diags.len() - 1];
    assert_eq!(d.code, DiagCode::FilletUnsupportedVertex);
    assert_eq!(d.fix, Some(Fix::Nested));
    assert!(d.hints[0].contains("concave"), "{:?}", d.hints);
    // The rewrite's two calls: the concave edges first.
    let p = plan(&format!(
        "fillet_edges(r = 1, edges = \"(all) and concave\") {block}"
    ));
    assert_eq!(p.status, Status::Built, "{:?}", p.diags);
}

/// Circles are F3's: a call that selects one says so and leaves the child
/// as it is; selecting only the lines builds.
#[test]
fn circles_are_not_built_yet_and_lines_are() {
    let part = "difference() { cube(20); translate([10, 10, -1]) cylinder(r = 4, h = 22); }";
    let p = plan(&format!("fillet_edges(r = 1, edges = \">z\") {part}"));
    assert_eq!(p.status, Status::NotBuilt);
    assert_eq!(codes(&p), [DiagCode::FilletNotBuilt]);
    let p = plan(&format!(
        "fillet_edges(r = 1, edges = \">z and %line\") {part}"
    ));
    assert_eq!(p.status, Status::Built);
    assert!(p.diags.is_empty(), "{:?}", p.diags);
}

/// The check after the boolean finds every blend whole where nothing cuts
/// it, including blends that cut each other at a corner (7.3) and blends
/// along a polygonal cylinder (conformed to its facets).
#[test]
fn whole_blends_report_nothing() {
    for src in [
        "fillet_edges(r = 2, edges = \"|z\") cube([20, 10, 5]);",
        "fillet_edges(r = 3, edges = \"<z\") cube([30, 20, 10]);",
        "fillet_edges(r = 1, edges = \"convex and |z\") intersection() { cylinder(r = 10, h = 10); translate([-20, 0, 0]) cube([40, 20, 10]); }",
    ] {
        let p = plan(src);
        assert_eq!(p.status, Status::Built, "{src}");
        assert!(p.diags.is_empty(), "{src}: {:?}", p.diags);
    }
}
