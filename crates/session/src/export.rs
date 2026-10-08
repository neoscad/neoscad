//! Geometry export formats and their encoding: one function that the
//! command line's own exports (`crates/cli/src/run.rs`) and the session's
//! (`Session::export`, used by `neoscad serve`) both call, so a file
//! written through the server has the same bytes as one written directly.

use geom::Geometry;
use geom::polyset::PolySet;
use lang::diag::Severity;

/// A geometry export: 3D meshes and 2D outlines.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Format {
    AsciiStl,
    BinaryStl,
    Off,
    Obj,
    ThreeMf,
    Wrl,
    Pov,
    Svg,
    Dxf,
    Pdf,
    /// STEP AP214 with exact surfaces (`--enable exact`; `geom::exact`).
    /// It is not encoded from the mesh: [`crate::Session::export`] builds
    /// it from a second render of the tree ([`encode`] refuses it), and
    /// only for a request with the `exact` extension on.
    Step,
}

impl Format {
    /// By OpenSCAD's format identifier (an `-o` extension or an
    /// `--export-format` value).
    pub fn from_id(id: &str) -> Option<Format> {
        Some(match id {
            "stl" | "asciistl" => Format::AsciiStl,
            "binstl" => Format::BinaryStl,
            "off" => Format::Off,
            "obj" => Format::Obj,
            "3mf" => Format::ThreeMf,
            "wrl" => Format::Wrl,
            "pov" => Format::Pov,
            "svg" => Format::Svg,
            "dxf" => Format::Dxf,
            "pdf" => Format::Pdf,
            "step" | "stp" => Format::Step,
            _ => return None,
        })
    }

    pub fn id(self) -> &'static str {
        match self {
            Format::AsciiStl => "stl",
            Format::BinaryStl => "binstl",
            Format::Off => "off",
            Format::Obj => "obj",
            Format::ThreeMf => "3mf",
            Format::Wrl => "wrl",
            Format::Pov => "pov",
            Format::Svg => "svg",
            Format::Dxf => "dxf",
            Format::Pdf => "pdf",
            Format::Step => "step",
        }
    }

    /// The dimension `checkAndExport` requires (`fileformat::is3D/is2D`).
    pub fn dimension(self) -> u32 {
        match self {
            Format::Svg | Format::Dxf | Format::Pdf => 2,
            _ => 3,
        }
    }
}

/// Everything an encoding needs besides the geometry: the `-O` settings
/// already resolved, and the facts a file records about its origin.
#[derive(Debug, Clone)]
pub struct Settings {
    /// The render colour scheme's face colours (the default colour of
    /// meshes without `color()`).
    pub scheme: geom::color::Scheme,
    pub svg: io::svg::SvgStyle,
    pub pdf: io::pdf::PdfOptions,
    /// Warnings resolving the PDF colours printed, before the export's.
    pub pdf_warnings: Vec<String>,
    pub threemf: io::threemf::Options,
    /// A warning resolving the 3MF colour, printed before the export's.
    pub threemf_warning: Option<String>,
    /// `ExportInfo::title`: the input's file name.
    pub title: String,
    /// The input as named on the command line (PDF metadata).
    pub source_path: String,
    /// `YYYY-MM-DDTHH:MM:SSZ`, from the host's clock (PDF and 3MF).
    pub creation_date: String,
    /// The command line's camera, which POV files record.
    pub pov_camera: Option<io::pov::PovCamera>,
    /// OpenSCAD's experimental `predictible-output` (`--enable`): the
    /// STL, OBJ, 3MF, OFF, WRL and POV writers sort the mesh's vertices
    /// and faces first ([`geom::export::sorted`]), so files are stable
    /// across kernels and versions. Off, as in OpenSCAD, by default.
    /// [`crate::Session::export`] sets it from the request's features
    /// ([`crate::Run::features`] and [`crate::Config::features`]); only a
    /// host calling [`encode`] directly sets it itself.
    pub predictible_output: bool,
}

/// One encoded file and what its encoding printed.
#[derive(Debug, Default)]
pub struct Encoded {
    pub data: Vec<u8>,
    /// Lines printed while encoding (`EXPORT-WARNING:` lines, 3MF errors),
    /// before `warnings`.
    pub immediate: Vec<(Option<Severity>, String)>,
    /// Warnings printed after encoding, as `WARNING: ...` (the
    /// `--hardwarnings` ones).
    pub warnings: Vec<String>,
}

/// Encode `root` (non-empty, of `format`'s dimension) as `format`. `mesh`
/// caches the 3D mesh between several outputs of one result.
pub fn encode(
    format: Format,
    root: &Geometry,
    s: &Settings,
    mesh: &mut Option<PolySet>,
) -> Encoded {
    let mut out = Encoded::default();
    if format == Format::Step {
        // A STEP file needs the tree, not the mesh: only
        // `Session::export` can write it.
        out.immediate.push((
            Some(Severity::Error),
            "ERROR: STEP is written from the model's tree, not encoded from its mesh.".into(),
        ));
        return out;
    }
    let warnings = &mut out.warnings;
    out.data = match (format, root) {
        (Format::Svg, Geometry::Polygon2d(p)) => geom::export::svg(p, &s.svg),
        (Format::Dxf, Geometry::Polygon2d(p)) => geom::export::dxf(p),
        (Format::Pdf, Geometry::Polygon2d(p)) => {
            warnings.extend(s.pdf_warnings.iter().cloned());
            let info = io::pdf::PdfInfo {
                title: &s.title,
                source_path: &s.source_path,
                creation_date: &s.creation_date,
            };
            let (data, export_warnings) = geom::export::pdf(p, &s.pdf, &info);
            // `message_group::Export_Warning`: not a warning for
            // `--hardwarnings`.
            out.immediate.extend(
                export_warnings
                    .into_iter()
                    .map(|w| (None, format!("EXPORT-WARNING: {w}"))),
            );
            data
        }
        _ => {
            let ps = mesh.get_or_insert_with(|| {
                geom::export::as_polyset(root, &s.scheme).expect("3D geometry has a mesh")
            });
            let sort = s.predictible_output;
            match format {
                Format::AsciiStl => geom::export::stl(ps, false, sort, warnings),
                Format::BinaryStl => geom::export::stl(ps, true, sort, warnings),
                Format::Off => geom::export::off(ps, sort, warnings),
                Format::Wrl => geom::export::wrl(ps, sort, warnings),
                Format::Pov => io::pov::write(
                    geom::export::ordered(ps, sort).mesh(),
                    &io::pov::PovOptions {
                        title: &s.title,
                        default_color: s.scheme.face_front,
                        camera: s.pov_camera,
                    },
                ),
                Format::ThreeMf => {
                    // `export_3mf` with the `-O export-3mf/...` settings:
                    // the mesh is triangulated first.
                    warnings.extend(s.threemf_warning.iter().cloned());
                    // `append_polyset` sorts the triangulated mesh, so the
                    // colour entries follow the sorted face order too.
                    let tri = ps.tessellate(warnings);
                    let (data, msgs) = io::threemf::write_with(
                        geom::export::ordered(&tri, sort).mesh(),
                        &io::threemf::WriteOptions {
                            title: &s.title,
                            creation_date: &s.creation_date,
                            default_color: s.scheme.face_front,
                        },
                        &s.threemf,
                    );
                    for m in msgs {
                        match m.severity {
                            Some(Severity::Warning) => warnings.push(m.text),
                            _ => out.immediate.push((Some(Severity::Error), m.text)),
                        }
                    }
                    data
                }
                _ => geom::export::obj(ps, sort, warnings),
            }
        }
    };
    out
}
