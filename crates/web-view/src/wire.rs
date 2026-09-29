//! The plain objects the viewer's JavaScript API takes and returns, as
//! serde types. They mirror the macOS app's `Viewport` records in
//! `crates/ffi` (`ViewportSettings`, `CameraState`, `ViewLine`,
//! `ViewMarker`, `PickRay`) so the two front ends describe a view the same
//! way. Unknown fields are ignored and missing ones take the defaults, so
//! a page can send only what it changes.

use serde::{Deserialize, Serialize};

use render::viewport::{AnnotationLine, AnnotationMarker, Annotations, ViewSettings};
use render::{Camera, Lighting, Projection};

/// What the View menu toggles (`ViewportSettings`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub axes: bool,
    /// Scale markers along the axes (drawn only with the axes).
    pub scales: bool,
    /// The ground grid (NeoSCAD's; OpenSCAD has none).
    pub grid: bool,
    pub edges: bool,
    pub crosshairs: bool,
    /// `"openscad"` (OpenSCAD's two lights) or `"headlight"`.
    pub lighting: LightingStyle,
    pub orthographic: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LightingStyle {
    #[default]
    OpenScad,
    Headlight,
}

impl Default for Settings {
    fn default() -> Self {
        Settings::from_view(ViewSettings::default(), Projection::default())
    }
}

impl Settings {
    pub fn from_view(s: ViewSettings, projection: Projection) -> Settings {
        Settings {
            axes: s.axes,
            scales: s.scales,
            grid: s.grid,
            edges: s.edges,
            crosshairs: s.crosshairs,
            lighting: match s.lighting {
                Lighting::OpenScad => LightingStyle::OpenScad,
                Lighting::Headlight => LightingStyle::Headlight,
            },
            orthographic: projection == Projection::Orthogonal,
        }
    }

    pub fn view(&self) -> ViewSettings {
        ViewSettings {
            axes: self.axes,
            scales: self.scales,
            grid: self.grid,
            edges: self.edges,
            crosshairs: self.crosshairs,
            lighting: match self.lighting {
                LightingStyle::OpenScad => Lighting::OpenScad,
                LightingStyle::Headlight => Lighting::Headlight,
            },
        }
    }

    pub fn projection(&self) -> Projection {
        if self.orthographic {
            Projection::Orthogonal
        } else {
            Projection::Perspective
        }
    }
}

/// The camera as OpenSCAD's `$vp*` variables (`CameraState`). As input
/// (`setFileView`), each is optional: a program's view sets only those it
/// assigned.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct CameraState {
    pub vpt: Option<[f64; 3]>,
    pub vpr: Option<[f64; 3]>,
    pub vpd: Option<f64>,
    pub vpf: Option<f64>,
}

impl CameraState {
    pub fn of(c: &Camera) -> CameraState {
        CameraState {
            vpt: Some(c.vpt()),
            vpr: Some(c.vpr()),
            vpd: Some(c.viewer_distance),
            vpf: Some(c.fov),
        }
    }
}

/// A polyline over the model (`ViewLine`): points in model coordinates,
/// each `[x, y, z]`, and an RGBA colour from 0 to 1 (RGB alone is opaque).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Line {
    pub points: Vec<[f64; 3]>,
    pub closed: bool,
    pub color: Vec<f32>,
}

/// A marked point with a label (`ViewMarker`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Marker {
    pub point: [f64; 3],
    pub label: String,
    pub color: Vec<f32>,
}

/// The check and measure panels' marks.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct AnnotationSet {
    pub lines: Vec<Line>,
    pub markers: Vec<Marker>,
}

/// `[r, g, b, a]`, `[r, g, b]` opaque, anything else magenta (as the app
/// does), so a malformed colour shows rather than vanishing.
fn rgba(c: &[f32]) -> [f32; 4] {
    match c {
        [r, g, b, a] => [*r, *g, *b, *a],
        [r, g, b] => [*r, *g, *b, 1.0],
        _ => [1.0, 0.0, 1.0, 1.0],
    }
}

impl AnnotationSet {
    pub fn annotations(&self) -> Annotations {
        Annotations {
            lines: self
                .lines
                .iter()
                .map(|l| AnnotationLine {
                    points: l.points.clone(),
                    closed: l.closed,
                    color: rgba(&l.color),
                })
                .collect(),
            markers: self
                .markers
                .iter()
                .map(|m| AnnotationMarker {
                    point: m.point,
                    label: m.label.clone(),
                    color: rgba(&m.color),
                })
                .collect(),
        }
    }
}

/// A ray into the scene (`PickRay`): origin and unit direction in model
/// coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PickRay {
    pub origin: [f64; 3],
    pub direction: [f64; 3],
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_default_to_the_apps_and_accept_a_partial_object() {
        let s = Settings::default();
        assert!(s.axes && s.scales && s.grid && !s.edges && !s.orthographic);
        let t: Settings =
            serde_json::from_str(r#"{"edges": true, "lighting": "headlight"}"#).unwrap();
        assert!(t.edges && t.axes);
        assert_eq!(t.view().lighting, Lighting::Headlight);
        let json = serde_json::to_value(s).unwrap();
        assert_eq!(json["lighting"], "openscad");
    }

    #[test]
    fn annotations_take_the_apps_shapes() {
        let a: AnnotationSet = serde_json::from_str(
            r#"{"lines": [{"points": [[0,0,0],[1,0,0],[1,1,0]], "closed": true,
                 "color": [1, 0, 0]}],
               "markers": [{"point": [1,2,3], "label": "1", "color": [0,0,1,0.5]}]}"#,
        )
        .unwrap();
        let a = a.annotations();
        assert_eq!(a.lines[0].color, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(a.lines[0].points.len(), 3);
        assert_eq!(a.markers[0].color, [0.0, 0.0, 1.0, 0.5]);
        let bad: AnnotationSet =
            serde_json::from_str(r#"{"markers": [{"point": [0,0,0], "color": []}]}"#).unwrap();
        assert_eq!(bad.annotations().markers[0].color, [1.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn a_file_view_sets_only_what_it_names() {
        let c: CameraState = serde_json::from_str(r#"{"vpd": 140}"#).unwrap();
        assert_eq!(c.vpd, Some(140.0));
        assert_eq!(c.vpt, None);
    }
}
