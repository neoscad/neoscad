//! The session as a long-lived host uses it: warm renders match cold
//! ones, edits on disk invalidate what they must, and a stale request
//! stops when a newer one supersedes it.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use lang::loader::LibraryPath;
use lang::vfs::MemFs;
use session::{Cancelled, Config, Mode, Run, Session, Stage};

fn session(fs: &Arc<MemFs>) -> Session {
    let mut cfg = Config::new(fs.clone(), LibraryPath(vec![PathBuf::from("/lib")]));
    cfg.work_dir = PathBuf::from("/doc");
    Session::new(cfg)
}

fn volume(s: &Session, input: &str) -> f64 {
    let r = s
        .render(
            &Run::new(input),
            Mode::Render,
            &render::ColorScheme::cornfield(),
        )
        .expect("not cancelled");
    assert_eq!(r.exit_code, 0, "{}", String::from_utf8_lossy(&r.log.stderr));
    let g = r.geometry_json(&render::ColorScheme::cornfield().geometry_scheme());
    g["volume"].as_f64().expect("a 3D result")
}

#[test]
fn a_changed_include_use_or_import_is_read_again() {
    let fs = Arc::new(MemFs::new());
    fs.insert(
        "/doc/main.scad",
        b"include <part.scad>\nuse <lib.scad>\npart(); libcube(); translate([10, 0, 0]) import(\"tetra.off\");\n".to_vec(),
    );
    fs.insert("/doc/part.scad", b"module part() cube(2);\n".to_vec());
    fs.insert(
        "/lib/lib.scad",
        b"module libcube() translate([5,0,0]) cube(1);\n".to_vec(),
    );
    let tetra = |s: f64| {
        format!(
            "OFF\n4 4 0\n0 0 {s}\n{s} 0 0\n0 {s} 0\n0 0 0\n3 0 1 2\n3 0 3 1\n3 0 2 3\n3 1 3 2\n"
        )
    };
    fs.insert("/doc/tetra.off", tetra(1.0).into_bytes());
    let s = session(&fs);
    let v0 = volume(&s, "main.scad");
    assert!((v0 - (8.0 + 1.0 + 1.0 / 6.0)).abs() < 1e-6, "{v0}");
    // Warm and unchanged: the parse and the geometry come from the caches.
    let before = s.stats();
    assert_eq!(volume(&s, "main.scad"), v0);
    let after = s.stats();
    assert!(after.parse.hits > before.parse.hits, "{after:?}");
    assert_eq!(after.geometry.misses, before.geometry.misses, "{after:?}");
    // Each file changed on disk: the result follows, as a cold session's.
    fs.insert("/doc/part.scad", b"module part() cube(3);\n".to_vec());
    assert!((volume(&s, "main.scad") - (27.0 + 1.0 + 1.0 / 6.0)).abs() < 1e-6);
    fs.insert(
        "/lib/lib.scad",
        b"module libcube() translate([5,0,0]) cube(2);\n".to_vec(),
    );
    assert!((volume(&s, "main.scad") - (27.0 + 8.0 + 1.0 / 6.0)).abs() < 1e-6);
    fs.insert("/doc/tetra.off", tetra(2.0).into_bytes());
    let v = volume(&s, "main.scad");
    assert!((v - (27.0 + 8.0 + 8.0 / 6.0)).abs() < 1e-6, "{v}");
    assert_eq!(v, volume(&session(&fs), "main.scad"));
}

#[test]
fn a_missing_include_is_found_once_it_exists() {
    let fs = Arc::new(MemFs::new());
    fs.insert(
        "/doc/main.scad",
        b"include <later.scad>\ncube(1);\n".to_vec(),
    );
    let s = session(&fs);
    let r = s.evaluate(&Run::new("main.scad"), false).unwrap();
    assert_eq!(r.log.diagnostics_json()[0]["code"], "include-not-found");
    fs.insert("/doc/later.scad", b"echo(\"here\");\n".to_vec());
    let r = s.evaluate(&Run::new("main.scad"), false).unwrap();
    assert!(r.log.diagnostics_json().is_empty());
    assert_eq!(r.log.echo(), ["ECHO: \"here\""]);
}

#[test]
fn unsaved_buffers_are_what_every_read_sees() {
    let fs = Arc::new(MemFs::new());
    fs.insert("/doc/main.scad", b"include <part.scad>\npart();\n".to_vec());
    fs.insert("/doc/part.scad", b"module part() cube(1);\n".to_vec());
    let s = session(&fs);
    assert_eq!(volume(&s, "main.scad"), 1.0);
    // An editor's unsaved change to the included file.
    s.update(Path::new("part.scad"), b"module part() cube(2);\n".to_vec());
    assert_eq!(volume(&s, "main.scad"), 8.0);
    assert!(s.close(Path::new("part.scad")));
    assert_eq!(volume(&s, "main.scad"), 1.0);
}

#[test]
fn warm_messages_match_a_cold_session() {
    let fs = Arc::new(MemFs::new());
    let src = "echo(1);\nunion() { cube(1); square(1); }\ncub(1);\n";
    fs.insert("/doc/m.scad", src.as_bytes().to_vec());
    let run = || {
        let s = session(&fs);
        let r = s
            .render(
                &Run::new("m.scad"),
                Mode::Render,
                &render::ColorScheme::cornfield(),
            )
            .unwrap();
        String::from_utf8(r.log.stderr).unwrap()
    };
    let cold = run();
    assert!(cold.contains("WARNING: Mixing 2D and 3D"), "{cold}");
    let s = session(&fs);
    for _ in 0..3 {
        let r = s
            .render(
                &Run::new("m.scad"),
                Mode::Render,
                &render::ColorScheme::cornfield(),
            )
            .unwrap();
        assert_eq!(String::from_utf8(r.log.stderr).unwrap(), cold);
    }
    // After an edit that moves the lines, the locations follow.
    s.update(Path::new("m.scad"), format!("\n{src}").into_bytes());
    let r = s
        .render(
            &Run::new("m.scad"),
            Mode::Render,
            &render::ColorScheme::cornfield(),
        )
        .unwrap();
    let moved = String::from_utf8(r.log.stderr).unwrap();
    assert!(moved.contains("line 3"), "{moved}");
    assert!(!moved.contains("line 2\n"), "{moved}");
}

/// Wait for `stage` of the request on `rx`, then run `then`.
fn when(rx: &mpsc::Receiver<Stage>, stage: Stage, then: impl FnOnce()) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(s) if s == stage => break,
            Ok(_) => {}
            Err(e) => panic!("no {stage:?} stage: {e}"),
        }
        assert!(Instant::now() < deadline);
    }
    then();
}

/// A run that reports its stages on a channel.
fn watched(input: &str) -> (Run, mpsc::Receiver<Stage>) {
    let (tx, rx) = mpsc::channel();
    let tx = std::sync::Mutex::new(tx);
    let mut run = Run::new(input);
    run.progress = Some(Arc::new(move |s| {
        let _ = tx.lock().unwrap().send(s);
    }));
    (run, rx)
}

#[test]
fn an_edit_cancels_a_stale_evaluation() {
    let fs = Arc::new(MemFs::new());
    // Tens of millions of loop iterations: seconds, unless interrupted.
    fs.insert(
        "/doc/slow.scad",
        b"n = 6000;\necho(len([for (i = [0:n]) for (j = [0:n]) if (i * j < 0) 1]));\n".to_vec(),
    );
    let s = Arc::new(session(&fs));
    let (run, rx) = watched("slow.scad");
    let t = Instant::now();
    let worker = {
        let s = s.clone();
        std::thread::spawn(move || s.evaluate(&run, false).map(|_| ()))
    };
    when(&rx, Stage::Evaluate, || {
        std::thread::sleep(Duration::from_millis(20));
        s.update(Path::new("slow.scad"), b"cube(1);\n".to_vec());
    });
    assert_eq!(worker.join().unwrap(), Err(Cancelled));
    assert!(t.elapsed() < Duration::from_secs(5), "{:?}", t.elapsed());
    assert_eq!(s.stats().cancelled, 1);
    // The new text renders normally.
    assert_eq!(volume(&s, "slow.scad"), 1.0);
}

#[test]
fn a_newer_request_cancels_a_stale_render_between_operations() {
    let fs = Arc::new(MemFs::new());
    // Quick to evaluate, slow to build: many fine spheres, unioned.
    fs.insert(
        "/doc/heavy.scad",
        b"for (i = [0:59]) translate([i * 1.5, i % 7, 0]) sphere(1, $fn = 96);\n".to_vec(),
    );
    let s = Arc::new(session(&fs));
    let (run, rx) = watched("heavy.scad");
    let worker = {
        let s = s.clone();
        std::thread::spawn(move || {
            s.render(&run, Mode::Render, &render::ColorScheme::cornfield())
                .map(|_| ())
        })
    };
    when(&rx, Stage::Geometry, || {
        // Any newer request on the document supersedes the render.
        let _ = s.evaluate(&Run::new("heavy.scad"), false).unwrap();
    });
    assert_eq!(worker.join().unwrap(), Err(Cancelled));
    // What the stale render finished stays cached and correct.
    let fresh = volume(&session(&fs), "heavy.scad");
    assert_eq!(volume(&s, "heavy.scad"), fresh);
}

#[test]
fn explicit_cancel_and_one_shot_requests() {
    let fs = Arc::new(MemFs::new());
    fs.insert(
        "/doc/slow.scad",
        b"n = 6000;\necho(len([for (i = [0:n]) for (j = [0:n]) if (i * j < 0) 1]));\n".to_vec(),
    );
    let s = Arc::new(session(&fs));
    let (mut run, rx) = watched("slow.scad");
    // One-shot requests (the command line's) are not superseded by
    // others, only cancelled explicitly.
    run.supersede = false;
    let worker = {
        let s = s.clone();
        std::thread::spawn(move || s.evaluate(&run, false).map(|_| ()))
    };
    when(&rx, Stage::Evaluate, || {
        assert_eq!(s.cancel(Path::new("slow.scad")), 1);
    });
    assert_eq!(worker.join().unwrap(), Err(Cancelled));
    assert_eq!(s.stats().running, 0);
}

#[test]
fn limits_stop_a_request_with_a_located_diagnostic() {
    // The app embeds the session: its limits are the configuration's,
    // and a request can carry its own.
    let fs = Arc::new(MemFs::new());
    fs.insert(
        "/doc/big.scad",
        b"cube(1);\nsphere(10, $fn = 100000);\n".to_vec(),
    );
    fs.insert("/doc/ok.scad", b"circle(r = 1, $fn = 20000);\n".to_vec());
    let mut cfg = Config::new(fs.clone(), LibraryPath(Vec::new()));
    cfg.work_dir = PathBuf::from("/doc");
    cfg.limits = session::Limits::AGENT;
    let s = Session::new(cfg);
    let scheme = render::ColorScheme::cornfield();
    let r = s
        .render(&Run::new("big.scad"), Mode::Render, &scheme)
        .expect("a limit is not a cancellation");
    assert_eq!(r.exit_code, 1);
    let d = r.log.diagnostics_json();
    assert_eq!(d[0]["code"], "resource-limit", "{d:?}");
    assert_eq!(d[0]["line"], 2, "located at the sphere: {d:?}");
    assert_eq!(s.stats().cancelled, 0);
    // Over the fragment limit by default; unlimited for this request.
    let r = s
        .render(&Run::new("ok.scad"), Mode::Render, &scheme)
        .unwrap();
    assert_eq!(r.exit_code, 1);
    let mut run = Run::new("ok.scad");
    run.limits = Some(session::Limits::NONE);
    let r = s.render(&run, Mode::Render, &scheme).unwrap();
    assert_eq!(r.exit_code, 0, "{}", String::from_utf8_lossy(&r.log.stderr));
    // The same cached subtree is not charged again or refused on a warm
    // render.
    let r = s
        .render(&Run::new("big.scad"), Mode::Render, &scheme)
        .unwrap();
    assert_eq!(r.exit_code, 1);
}

#[test]
fn cancel_all_stops_every_document() {
    // What `neoscad mcp` does when its client goes away.
    let fs = Arc::new(MemFs::new());
    fs.insert(
        "/doc/slow.scad",
        b"n = 6000;\necho(len([for (i = [0:n]) for (j = [0:n]) if (i * j < 0) 1]));\n".to_vec(),
    );
    let s = Arc::new(session(&fs));
    let (mut run, rx) = watched("slow.scad");
    run.supersede = false;
    let worker = {
        let s = s.clone();
        std::thread::spawn(move || s.evaluate(&run, false).map(|_| ()))
    };
    when(&rx, Stage::Evaluate, || {
        assert_eq!(s.cancel_all(), 1);
    });
    assert_eq!(worker.join().unwrap(), Err(Cancelled));
    assert_eq!(s.cancel_all(), 0);
}

#[test]
fn a_limit_in_parallel_geometry_reports_the_same_every_time() {
    // Siblings render in parallel; the one over the limit is named the
    // same way whatever the scheduling (the determinism rule).
    let fs = Arc::new(MemFs::new());
    fs.insert(
        "/doc/m.scad",
        b"for (i = [0:15]) translate([i * 3, 0, 0]) sphere(1, $fn = 48);\ntranslate([0, 10, 0]) cylinder(h = 1, r = 1, $fn = 50000);\n".to_vec(),
    );
    let mut first: Option<Vec<u8>> = None;
    for _ in 0..5 {
        let mut cfg = Config::new(fs.clone(), LibraryPath(Vec::new()));
        cfg.work_dir = PathBuf::from("/doc");
        cfg.limits = session::Limits::AGENT;
        let s = Session::new(cfg);
        let r = s
            .render(
                &Run::new("m.scad"),
                Mode::Render,
                &render::ColorScheme::cornfield(),
            )
            .unwrap();
        assert_eq!(r.exit_code, 1);
        match &first {
            None => first = Some(r.log.stderr.clone()),
            Some(f) => assert_eq!(f, &r.log.stderr),
        }
    }
    let text = String::from_utf8(first.unwrap()).unwrap();
    assert_eq!(
        text,
        "ERROR: Resource limit exceeded: cylinder() would make 50,000 fragments, over the fragments limit of 10,000 in file m.scad, line 2\n"
    );
}

#[test]
fn a_request_reports_the_files_it_read_warm_or_cold() {
    // The app watches these to re-preview when an include, a used
    // library or an imported file changes on disk. A warm request (every
    // parse from the cache) must name the same files as a cold one.
    let fs = Arc::new(MemFs::new());
    fs.insert(
        "/doc/main.scad",
        b"include <parts.scad>\nuse <lib/util.scad>\npeg();\nimport(\"shape.off\");\n".to_vec(),
    );
    fs.insert(
        "/doc/parts.scad",
        b"module peg() cylinder(h = 5, r = 1);\n".to_vec(),
    );
    fs.insert("/lib/lib/util.scad", b"function f() = 1;\n".to_vec());
    fs.insert(
        "/doc/shape.off",
        b"OFF\n4 4 0\n0 0 0\n1 0 0\n0 1 0\n0 0 1\n3 0 2 1\n3 0 1 3\n3 1 2 3\n3 0 3 2\n".to_vec(),
    );
    let s = session(&fs);
    let want: Vec<PathBuf> = [
        "/doc/main.scad",
        "/doc/parts.scad",
        "/doc/shape.off",
        "/lib/lib/util.scad",
    ]
    .iter()
    .map(PathBuf::from)
    .collect();
    for round in ["cold", "warm"] {
        let r = s
            .render(
                &Run::new("main.scad"),
                Mode::Preview,
                &render::ColorScheme::cornfield(),
            )
            .unwrap();
        assert_eq!(r.exit_code, 0, "{}", String::from_utf8_lossy(&r.log.stderr));
        assert_eq!(r.files, want, "{round}");
        let e = s.evaluate(&Run::new("main.scad"), false).unwrap();
        assert!(e.files.starts_with(&want[..2]), "{round}: {:?}", e.files);
    }
}

#[test]
fn a_warm_cache_answers_as_a_cold_one_under_lowered_limits() {
    // A cache hit does no work, but after the limits are lowered it must
    // not pass a model that a cold render refuses; and results computed
    // without limits (whose demand is unknown) are checked too.
    let fs = Arc::new(MemFs::new());
    fs.insert(
        "/doc/m.scad",
        b"cube(1);\ntranslate([3, 0, 0]) sphere(1, $fn = 400);\n".to_vec(),
    );
    let scheme = render::ColorScheme::cornfield();
    let tight = session::Limits {
        fragments: Some(100),
        ..session::Limits::AGENT
    };
    let render = |s: &Session, limits: session::Limits| {
        let mut run = Run::new("m.scad");
        run.limits = Some(limits);
        s.render(&run, Mode::Render, &scheme).unwrap()
    };
    let cold = render(&session(&fs), tight);
    assert_eq!(cold.exit_code, 1);
    for first in [session::Limits::AGENT, session::Limits::NONE] {
        let s = session(&fs);
        assert_eq!(render(&s, first).exit_code, 0);
        let warm = render(&s, tight);
        assert_eq!(warm.exit_code, 1, "after {first:?}");
        assert_eq!(warm.log.stderr, cold.log.stderr);
        // And back: the looser limits reuse the cache again.
        let again = render(&s, session::Limits::AGENT);
        assert_eq!(again.exit_code, 0);
    }
    // The triangle limit, on a result that passed the fragment limit.
    let s = session(&fs);
    assert_eq!(render(&s, session::Limits::AGENT).exit_code, 0);
    let few = session::Limits {
        triangles: Some(1000),
        ..session::Limits::AGENT
    };
    let r = render(&s, few);
    assert_eq!(r.exit_code, 1);
    assert_eq!(r.log.diagnostics_json()[0]["code"], "resource-limit");
    assert_eq!(r.log.stderr, render(&session(&fs), few).log.stderr);
}

#[test]
fn a_render_says_which_view_variables_the_file_assigned() {
    let fs = Arc::new(MemFs::new());
    fs.insert(
        "/doc/v.scad",
        b"$vpr = [10, 20, 30];\n$vpd = 50;\ncube(1);\n".to_vec(),
    );
    fs.insert("/doc/n.scad", b"cube(1);\n".to_vec());
    let s = session(&fs);
    let scheme = render::ColorScheme::cornfield();
    let mut run = Run::new("v.scad");
    run.camera.auto = false;
    let r = s.render(&run, Mode::Preview, &scheme).unwrap();
    assert!(r.camera_assigned.vpr && r.camera_assigned.vpd);
    assert!(!r.camera_assigned.vpt && !r.camera_assigned.vpf);
    assert_eq!(r.camera.vpr, [10.0, 20.0, 30.0]);
    assert_eq!(r.camera.vpd, 50.0);
    // Without `auto` (a GUI's view), no "Viewall and autocenter" warning.
    assert!(
        r.log.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&r.log.stderr)
    );
    let r = s
        .render(&Run::new("n.scad"), Mode::Preview, &scheme)
        .unwrap();
    assert!(!r.camera_assigned.any());
}
