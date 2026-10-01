//! The check and measure panels' logic without GTK: their requests, off
//! the main thread, and what the 3D view draws for them.
//!
//! Both requests run detached from the document loop (`Client::detached`):
//! a check does not cancel the live preview, and typing does not cancel a
//! check. A newer request of the same kind stops the older one through its
//! interrupt flag, as the macOS app's `Inspect.swift` cancels its task.
//!
//! The overlay is built by the core from the panels' state
//! (`client::view_overlay`), the same lines and markers the macOS app and
//! the web demo draw, and handed to the viewport as its annotations.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use client::{
    CheckFinding, CheckReport, Client, CoreError, FindingSeverity, MeasureReport, Measurement,
    OverlayState, PrinterSettings, RunOptions,
};
use render::viewport::{AnnotationLine, AnnotationMarker, Annotations};

/// `neoscad check` on the document at `path` (its text sent to the core
/// already) with the customizer's values and the window's parts toggle.
pub fn check(
    client: &Client,
    path: &str,
    run: &RunOptions,
    printer: &PrinterSettings,
    interrupt: Option<Arc<AtomicBool>>,
) -> Result<CheckReport, CoreError> {
    let r = client.detached(path, run, interrupt, None)?;
    client.check(r, &printer.check_options())
}

/// `neoscad measure` on the document: the report, and the solids to pick
/// points on (`None` when there is nothing to measure).
pub fn measure(
    client: &Client,
    path: &str,
    run: &RunOptions,
    interrupt: Option<Arc<AtomicBool>>,
) -> Result<(MeasureReport, Option<Measurement>), CoreError> {
    let r = client.detached(path, run, interrupt, None)?;
    client.measure(r)
}

/// The viewport's annotations for the panels' state. A colour with three
/// channels is opaque; a point that is not three numbers is dropped (the
/// core never makes one, but a marker at the origin would be a lie).
pub fn annotations(state: &OverlayState) -> Annotations {
    let o = client::view_overlay(state);
    let rgba = |c: &[f32]| match c {
        [r, g, b, a, ..] => [*r, *g, *b, *a],
        [r, g, b] => [*r, *g, *b, 1.0],
        _ => [1.0, 0.0, 1.0, 1.0],
    };
    Annotations {
        lines: o
            .lines
            .iter()
            .map(|l| AnnotationLine {
                // A trailing partial point is dropped.
                points: l.points.as_chunks::<3>().0.to_vec(),
                closed: l.closed,
                color: rgba(&l.color),
            })
            .collect(),
        markers: o
            .markers
            .iter()
            .filter_map(|m| {
                Some(AnnotationMarker {
                    point: client::point3(&m.point, "a marker").ok()?,
                    label: m.label.clone(),
                    color: rgba(&m.color),
                })
            })
            .collect(),
    }
}

/// A finding's severity as the panel names it.
pub fn severity_name(s: FindingSeverity) -> &'static str {
    match s {
        FindingSeverity::Error => "Error",
        FindingSeverity::Warning => "Warning",
        FindingSeverity::Info => "Info",
    }
}

/// The symbolic icon and style class of a severity (libadwaita's `error`,
/// `warning` and `accent` colours).
pub fn severity_style(s: FindingSeverity) -> (&'static str, &'static str) {
    match s {
        FindingSeverity::Error => ("dialog-error-symbolic", "error"),
        FindingSeverity::Warning => ("dialog-warning-symbolic", "warning"),
        FindingSeverity::Info => ("dialog-information-symbolic", "accent"),
    }
}

/// A finding's row: the title (its number, as the view's marker is
/// numbered, and the message) and the subtitle (code, part and the fix).
pub fn finding_text(f: &CheckFinding) -> (String, String) {
    let mut sub = format!("{} · {}", severity_name(f.severity), f.code);
    if let Some(p) = &f.part {
        sub += &format!(" · part {p}");
    }
    if !f.fix.is_empty() {
        sub += &format!("\n{}", f.fix);
    }
    (format!("{}. {}", f.id, f.message), sub)
}

/// The finding selected after a click on `id`: a click on the selected one
/// clears the selection; an id the report no longer has selects nothing.
pub fn toggle_selection(findings: &[CheckFinding], selected: Option<u32>, id: u32) -> Option<u32> {
    if selected == Some(id) || !findings.iter().any(|f| f.id == id) {
        None
    } else {
        Some(id)
    }
}

/// A new pick: two points measure, a third starts again.
pub fn add_pick(picks: &mut Vec<Vec<f64>>, hit: Vec<f64>) {
    if picks.len() >= 2 {
        picks.clear();
    }
    picks.push(hit);
}

fn point_text(p: &[f64]) -> String {
    let parts: Vec<String> = p.iter().map(|x| client::format_number(*x)).collect();
    format!("({})", parts.join(", "))
}

/// The measure panel's line about the picked points.
pub fn picks_text(picks: &[Vec<f64>]) -> String {
    match (picks, client::pick_distance(picks)) {
        ([], _) => "Click two points on the model.".into(),
        ([a], _) => format!("A {}\nClick a second point.", point_text(a)),
        ([a, b, ..], Some(d)) => format!(
            "A {}\nB {}\nDistance {} mm",
            point_text(a),
            point_text(b),
            client::format_number(d)
        ),
        _ => String::new(),
    }
}

/// The measure panel's summary of the model: volume, area and size, or
/// why there is nothing to measure.
pub fn measure_text(r: &MeasureReport) -> String {
    if r.exit_code != 0 {
        return match client::first_error(&r.console) {
            Some(e) => format!("The model did not render: {e}"),
            None => "The model did not render.".into(),
        };
    }
    let n = client::format_number;
    if let Some(s) = &r.model {
        let size: Vec<String> = s
            .bbox_max
            .iter()
            .zip(&s.bbox_min)
            .map(|(hi, lo)| n(hi - lo))
            .collect();
        let mut t = format!(
            "Volume {} mm³ · area {} mm²\nSize {} mm",
            n(s.volume),
            n(s.area),
            size.join(" × ")
        );
        if r.manifold == Some(false) {
            t += "\nNot manifold";
        }
        return t;
    }
    if r.model_2d.is_some() {
        return "A 2D model: picking points needs a solid.".into();
    }
    "Nothing to measure: the model is empty.".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn client() -> Client {
        let mut cfg = crate::host::config();
        cfg.gpu = None;
        cfg.limits = session::Limits::AGENT;
        Client::new(cfg)
    }

    /// A plate thinner than a 0.4 mm nozzle: `check` must report it, as the
    /// smoke test's model does.
    #[test]
    fn a_thin_plate_is_an_error_with_a_fix_and_a_marker() {
        let c = client();
        let path = "/nonexistent-neoscad-linux-app/thin.scad";
        c.open(path, Some("cube([30, 30, 0.2]);".into())).unwrap();
        let r = check(
            &c,
            path,
            &RunOptions::default(),
            &PrinterSettings::default(),
            None,
        )
        .unwrap();
        assert!(!r.failed, "{}", r.console);
        assert!(r.errors >= 1, "{}", r.text);
        let f = r
            .findings
            .iter()
            .find(|f| f.severity == FindingSeverity::Error)
            .unwrap();
        assert!(!f.fix.is_empty());
        let (title, sub) = finding_text(f);
        assert!(title.starts_with(&format!("{}. ", f.id)), "{title}");
        assert!(sub.starts_with("Error · "), "{sub}");
        // "1 error", "2 errors": the count and its noun, pluralised.
        assert!(client::check_summary(&r).starts_with(&format!("{} error", r.errors)));

        // Every located finding is a marker; the selected one has a box.
        let state = OverlayState {
            findings: r.findings.clone(),
            selected: Some(f.id),
            ..Default::default()
        };
        let a = annotations(&state);
        assert!(a.markers.iter().any(|m| m.label == f.id.to_string()));
        assert_eq!(a.lines.len(), 6, "the selected finding's box");
        assert_eq!(a.lines[0].points.len(), 4);
        assert!(annotations(&OverlayState::default()) == Annotations::default());

        assert_eq!(toggle_selection(&r.findings, None, f.id), Some(f.id));
        assert_eq!(toggle_selection(&r.findings, Some(f.id), f.id), None);
        assert_eq!(toggle_selection(&r.findings, None, 9999), None);
    }

    #[test]
    fn a_cancelled_check_says_so() {
        let c = client();
        let path = "/nonexistent-neoscad-linux-app/cancel.scad";
        c.open(path, Some("cube(10);".into())).unwrap();
        let stop = Arc::new(AtomicBool::new(true));
        let r = check(
            &c,
            path,
            &RunOptions::default(),
            &PrinterSettings::default(),
            Some(stop),
        );
        assert!(matches!(r, Err(CoreError::Cancelled)), "{r:?}");
    }

    #[test]
    fn two_picked_points_measure_their_distance() {
        let c = client();
        let path = "/nonexistent-neoscad-linux-app/measure.scad";
        c.open(path, Some("cube(10);".into())).unwrap();
        let (r, m) = measure(&c, path, &RunOptions::default(), None).unwrap();
        assert!(
            measure_text(&r).starts_with("Volume 1000 mm³"),
            "{}",
            measure_text(&r)
        );
        let m = m.unwrap();
        // Straight down onto the top face, then up onto the bottom one.
        let top = m
            .pick(&[5.0, 5.0, 50.0], &[0.0, 0.0, -1.0])
            .unwrap()
            .unwrap();
        let bottom = m
            .pick(&[5.0, 5.0, -50.0], &[0.0, 0.0, 1.0])
            .unwrap()
            .unwrap();
        assert!(
            m.pick(&[50.0, 50.0, 50.0], &[0.0, 0.0, -1.0])
                .unwrap()
                .is_none()
        );

        let mut picks = Vec::new();
        assert_eq!(picks_text(&picks), "Click two points on the model.");
        add_pick(&mut picks, top);
        assert!(picks_text(&picks).ends_with("Click a second point."));
        add_pick(&mut picks, bottom.clone());
        assert!(
            picks_text(&picks).ends_with("Distance 10 mm"),
            "{}",
            picks_text(&picks)
        );
        let a = annotations(&OverlayState {
            picks: picks.clone(),
            ..Default::default()
        });
        assert_eq!(a.markers.len(), 2);
        assert_eq!(a.lines.len(), 1);
        // A third point starts a new pair.
        add_pick(&mut picks, bottom);
        assert_eq!(picks.len(), 1);
    }

    #[test]
    fn an_empty_model_has_nothing_to_measure() {
        let c = client();
        let path = "/nonexistent-neoscad-linux-app/empty.scad";
        c.open(path, Some("x = 1;".into())).unwrap();
        let (r, m) = measure(&c, path, &RunOptions::default(), None).unwrap();
        assert!(m.is_none());
        assert_eq!(measure_text(&r), "Nothing to measure: the model is empty.");
    }
}
