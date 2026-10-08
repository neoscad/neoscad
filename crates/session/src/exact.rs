//! What a STEP export with exact surfaces (`--enable exact`;
//! `geom::exact`) reports to its host: one shape for the command line's
//! `--format json`, `neoscad serve`'s `export` reply, the MCP tools'
//! structured content, the apps and the web page, so none of them words
//! or counts the substitutions its own way.
//!
//! The report is built where the source locations can still be resolved
//! to file names (the session, or the command line's own run), because a
//! host only has the finished result: a faceted region it could not point
//! at a line is one the user cannot do anything about.

use geom::exact::{ExactStats, Substitution, SubstitutionKind};
use serde_json::{Value, json};

/// One substitution, its location resolved: what the export did with the
/// curves (or the lack of them) of one call in the source.
#[derive(Debug, Clone, PartialEq)]
pub struct Region {
    /// `exact` (a `$fa`/`$fs` curve written as the true surface),
    /// `polygon` (an explicit `$fn` kept as OpenSCAD's polygon) or
    /// `faceted` (written as the mesh's planar facets).
    pub kind: &'static str,
    /// The module, as OpenSCAD spells it (`sphere`, `hull`).
    pub module: &'static str,
    /// What happened, in words, without the location.
    pub detail: String,
    /// The file, relative to the main file's directory, and its line.
    pub file: Option<String>,
    pub line: Option<u32>,
    /// Instances at this location (a module called in a loop).
    pub count: u32,
}

impl Region {
    /// `sphere() at m.scad, line 3`, or `sphere()` with no location.
    pub fn place(&self) -> String {
        match (&self.file, self.line) {
            (Some(f), Some(l)) => format!("{}() at {f}, line {l}", self.module),
            _ => format!("{}()", self.module),
        }
    }
}

/// A STEP export's outcome and numbers.
#[derive(Debug, Clone, PartialEq)]
pub struct ExactReport {
    /// Whether a file was written.
    pub ok: bool,
    /// Why not, when it was refused.
    pub error: Option<String>,
    pub stats: ExactStats,
    /// Every substitution, in the export's order.
    pub regions: Vec<Region>,
    /// The normal (mesh) render's time, for comparison with the export's
    /// own stages; `None` when the host has no clock.
    pub normal_render_ms: Option<f64>,
}

impl ExactReport {
    /// The report of an export that measured `stats` and made `subs`,
    /// refused with `error` or not; `locate` turns a source location into
    /// a file name and line.
    pub fn new(
        stats: &ExactStats,
        subs: &[Substitution],
        error: Option<&str>,
        locate: &dyn Fn(&geom::MsgLoc) -> Option<(String, u32)>,
    ) -> ExactReport {
        let regions = subs
            .iter()
            .map(|s| {
                let at = s.loc.as_ref().and_then(locate);
                Region {
                    kind: kind_name(s.kind),
                    module: s.module,
                    detail: s.detail.clone(),
                    file: at.as_ref().map(|(f, _)| f.clone()),
                    line: at.map(|(_, l)| l),
                    count: s.count,
                }
            })
            .collect();
        ExactReport {
            ok: error.is_none(),
            error: error.map(str::to_string),
            stats: stats.clone(),
            regions,
            normal_render_ms: None,
        }
    }

    /// Instances of substitutions of `kind`.
    pub fn count(&self, kind: &str) -> u32 {
        self.regions
            .iter()
            .filter(|r| r.kind == kind)
            .map(|r| r.count)
            .sum()
    }

    /// The faceted regions: what falls short of an exact export.
    pub fn faceted(&self) -> impl Iterator<Item = &Region> {
        self.regions.iter().filter(|r| r.kind == "faceted")
    }

    /// The share of the B-rep's faces on an exact surface (planes
    /// included), in percent; `None` before any face was built.
    pub fn exact_percent(&self) -> Option<f64> {
        (self.stats.faces > 0)
            .then(|| 100.0 * self.stats.exact_faces as f64 / self.stats.faces as f64)
    }

    /// The report in words, a line each, as an app's alert or an agent's
    /// text shows it: the outcome and the share of exact faces, then each
    /// faceted region at its location. Exact curves and kept polygons are
    /// what the user asked for, so they are counted, not listed.
    pub fn summary(&self) -> String {
        let s = &self.stats;
        let mut out = Vec::new();
        if let Some(e) = &self.error {
            out.push(format!("STEP export refused: {e}. No file was written."));
        } else {
            let pct = self.exact_percent().unwrap_or(0.0);
            out.push(format!(
                "STEP: {} of {} faces exact ({}%).",
                s.exact_faces,
                s.faces,
                percent(pct)
            ));
        }
        let (exact, polygon) = (self.count("exact"), self.count("polygon"));
        if exact + polygon > 0 {
            let mut parts = Vec::new();
            if exact > 0 {
                parts.push(format!(
                    "{exact} curve{} made exact",
                    if exact == 1 { "" } else { "s" }
                ));
            }
            if polygon > 0 {
                parts.push(format!(
                    "{polygon} $fn polygon{} kept",
                    if polygon == 1 { "" } else { "s" }
                ));
            }
            out.push(format!("{}.", parts.join(", ")));
        }
        if let Some(p) = &s.partial {
            out.push(format!(
                "{} region{} written as facets where the model did not reconstruct exact: {}.",
                p.regions,
                if p.regions == 1 { "" } else { "s" },
                p.reason
            ));
        }
        if let Some(f) = &s.fallback {
            out.push(format!("Extrusions written as facets: {f}."));
        }
        for r in self.faceted() {
            let times = if r.count > 1 {
                format!(" ({} instances)", r.count)
            } else {
                String::new()
            };
            out.push(format!("Faceted: {} {}{times}", r.place(), r.detail));
        }
        out.join("\n")
    }

    /// The report as JSON: the command line's `--format json` `exact` key
    /// (`docs/cli-json.md`, "STEP with exact surfaces"), which
    /// `conformance exact` reads too, and `serve`'s and MCP's.
    pub fn json(&self) -> Value {
        let s = &self.stats;
        let faceted_modules: std::collections::BTreeSet<&str> =
            self.faceted().map(|r| r.module).collect();
        json!({
            "ok": self.ok,
            "error": self.error,
            "attempts": s.attempts,
            "retried_because": s.retried_because,
            // Why the extrusions were written as facets after all, and what
            // fell back in the exact attempts (`conformance exact` judges the
            // model's eligibility by those).
            "fallback": s.fallback,
            "exact_attempt_faceted": s.exact_attempt_faceted,
            // The regions written as facets where the exact attempts failed,
            // the rest exact (`conformance exact` counts these apart).
            "partial": s.partial.as_ref().map(|p| json!({
                "reason": p.reason,
                "regions": p.regions,
                "triangles": p.triangles,
                "exact_triangles": p.exact_triangles,
                "rounds": p.rounds,
            })),
            "triangles": s.triangles,
            "faces": s.faces,
            "exact_faces": s.exact_faces,
            "exact_percent": self.exact_percent(),
            "edges": s.edges,
            "bspline_edges": s.bspline_edges,
            "volume": s.volume,
            "corrected_mesh_volume": s.corrected_volume,
            "volume_error": s.volume_error,
            "volume_tolerance": s.volume_tolerance,
            "normal_volume": s.normal_volume,
            "normal_render_ms": self.normal_render_ms,
            "chain_deviation": s.chain_deviation,
            "max_cap": s.max_cap,
            "substitutions": {
                "exact": self.count("exact"),
                "polygon": self.count("polygon"),
                "faceted": self.count("faceted"),
                "faceted_modules": faceted_modules,
            },
            "faceted_regions": self.faceted().map(region_json).collect::<Vec<_>>(),
            "summary": self.summary(),
            "notes": s.notes,
            "timings_ms": {
                "export_render": s.timings.export_render_ms,
                "reconstruct": s.timings.reconstruct_ms,
                "check": s.timings.check_ms,
                "write": s.timings.write_ms,
            },
        })
    }
}

fn region_json(r: &Region) -> Value {
    json!({
        "module": r.module,
        "file": r.file,
        "line": r.line,
        "count": r.count,
        "detail": r.detail,
    })
}

fn kind_name(k: SubstitutionKind) -> &'static str {
    match k {
        SubstitutionKind::Exact => "exact",
        SubstitutionKind::Polygon => "polygon",
        SubstitutionKind::Faceted => "faceted",
    }
}

/// A share for people: whole numbers as such, a fraction to one place,
/// and never "100" for a share that is not all of it (99.96% of faces
/// exact still has a faceted one, which a rounded 100 would hide).
fn percent(p: f64) -> String {
    let r = (p * 10.0).round() / 10.0;
    if r >= 100.0 && p < 100.0 {
        "99.9".into()
    } else if r.fract() == 0.0 {
        format!("{r:.0}")
    } else {
        format!("{r:.1}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_never_rounds_up_to_all() {
        assert_eq!(percent(100.0), "100");
        assert_eq!(percent(99.97), "99.9");
        assert_eq!(percent(96.84), "96.8");
        assert_eq!(percent(50.0), "50");
    }

    #[test]
    fn summary_lists_faceted_regions_at_their_lines() {
        let stats = ExactStats {
            faces: 10,
            exact_faces: 8,
            ..Default::default()
        };
        let subs = vec![
            Substitution {
                kind: SubstitutionKind::Exact,
                module: "cylinder",
                detail: "is exported as an exact cylinder".into(),
                loc: None,
                count: 2,
            },
            Substitution {
                kind: SubstitutionKind::Faceted,
                module: "hull",
                detail: "is exported as planar facets".into(),
                loc: None,
                count: 1,
            },
        ];
        let r = ExactReport::new(&stats, &subs, None, &|_| None);
        assert_eq!(
            r.summary(),
            "STEP: 8 of 10 faces exact (80%).\n2 curves made exact.\n\
             Faceted: hull() is exported as planar facets"
        );
        let j = r.json();
        assert_eq!(j["substitutions"]["faceted_modules"], json!(["hull"]));
        assert_eq!(j["exact_percent"], 80.0);
        let refused = ExactReport::new(&stats, &[], Some("the shell is open"), &|_| None);
        assert!(!refused.ok);
        assert!(
            refused
                .summary()
                .starts_with("STEP export refused: the shell is open.")
        );
    }
}
