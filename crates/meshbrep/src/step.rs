//! A deterministic STEP AP214 writer.
//!
//! The file depends only on the B-rep and the options: entities are
//! numbered in a fixed traversal order, numbers are written in Rust's
//! shortest round-trip form, and the header's names and date are the
//! caller's (fixed defaults otherwise, never the clock).

use std::fmt::Write as _;

use crate::math::*;
use crate::model::{BSpline, Brep, Curve, Face, Surface};

/// Names and dates for the file header and the product.
#[derive(Clone, Debug, PartialEq)]
pub struct StepOptions {
    /// The product's name and id.
    pub product_name: String,
    /// `FILE_NAME`'s name.
    pub file_name: String,
    /// `FILE_NAME`'s time stamp (ISO 8601). Fixed by default, so that the
    /// same model always gives the same bytes.
    pub timestamp: String,
    /// `FILE_NAME`'s author.
    pub author: String,
    /// `FILE_NAME`'s organisation.
    pub organization: String,
    /// `FILE_NAME`'s originating system. Defaults to this crate's name and
    /// version; set it to keep files identical across crate versions.
    pub originating_system: String,
    /// `FILE_DESCRIPTION`'s text.
    pub description: String,
}

impl Default for StepOptions {
    fn default() -> Self {
        StepOptions {
            product_name: "part".into(),
            file_name: "part.step".into(),
            timestamp: "1970-01-01T00:00:00".into(),
            author: String::new(),
            organization: String::new(),
            originating_system: concat!("meshbrep ", env!("CARGO_PKG_VERSION")).into(),
            description: String::new(),
        }
    }
}

/// A STEP string literal: quotes doubled, backslashes doubled, non-ASCII
/// as `\X2\…\X0\` (UTF-16 hex), per ISO 10303-21.
fn string(s: &str) -> String {
    let mut out = String::from("'");
    let mut wide: Vec<u16> = Vec::new();
    let flush = |wide: &mut Vec<u16>, out: &mut String| {
        if !wide.is_empty() {
            out.push_str("\\X2\\");
            for u in wide.iter() {
                let _ = write!(out, "{u:04X}");
            }
            out.push_str("\\X0\\");
            wide.clear();
        }
    };
    for c in s.chars() {
        if c.is_ascii() && !c.is_ascii_control() {
            flush(&mut wide, &mut out);
            match c {
                '\'' => out.push_str("''"),
                '\\' => out.push_str("\\\\"),
                _ => out.push(c),
            }
        } else {
            let mut buf = [0u16; 2];
            wide.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
    flush(&mut wide, &mut out);
    out.push('\'');
    out
}

/// A STEP real: shortest round-trip digits, always with a decimal point.
fn real(x: f64) -> String {
    let mut out = String::new();
    push_real(&mut out, x, &mut String::new());
    out
}

/// [`real`], appended to `out` (`scratch` is reused between calls). The
/// writer formats millions of these on a large faceted model, and the
/// temporary strings cost as much as the digits.
fn push_real(out: &mut String, x: f64, scratch: &mut String) {
    if x == 0.0 {
        out.push_str("0.");
        return;
    }
    scratch.clear();
    let _ = write!(scratch, "{x:E}");
    let (mant, exp) = scratch.split_once('E').expect("exponent");
    out.push_str(mant);
    if !mant.contains('.') {
        out.push('.');
    }
    if exp != "0" {
        out.push('E');
        out.push_str(exp);
    }
}

struct W {
    out: Vec<String>,
    scratch: String,
}

impl W {
    fn add(&mut self, s: String) -> usize {
        self.out.push(s);
        self.out.len()
    }
    fn reserve(&mut self) -> usize {
        self.add(String::new())
    }
    fn set(&mut self, id: usize, s: String) {
        self.out[id - 1] = s;
    }
    /// `KIND('',(x,y,...))`.
    fn tuple(&mut self, kind: &str, xs: &[f64]) -> usize {
        let mut s = String::with_capacity(kind.len() + 8 + 24 * xs.len());
        s.push_str(kind);
        s.push_str("('',(");
        for (i, &x) in xs.iter().enumerate() {
            if i > 0 {
                s.push(',');
            }
            push_real(&mut s, x, &mut self.scratch);
        }
        s.push_str("))");
        self.add(s)
    }
    fn pt(&mut self, p: [f64; 3]) -> usize {
        self.tuple("CARTESIAN_POINT", &p)
    }
    fn pt2(&mut self, p: [f64; 2]) -> usize {
        self.tuple("CARTESIAN_POINT", &p)
    }
    fn dir(&mut self, d: [f64; 3]) -> usize {
        self.tuple("DIRECTION", &d)
    }
    fn ax2(&mut self, o: [f64; 3], z: [f64; 3], x: [f64; 3]) -> usize {
        let p = self.pt(o);
        let a = self.dir(z);
        let b = self.dir(x);
        self.add(format!("AXIS2_PLACEMENT_3D('',#{p},#{a},#{b})"))
    }
}

fn refs(ids: &[usize]) -> String {
    let mut s = String::with_capacity(8 * ids.len());
    for (k, i) in ids.iter().enumerate() {
        if k > 0 {
            s.push(',');
        }
        let _ = write!(s, "#{i}");
    }
    s
}

fn bspline_entity<const D: usize>(w: &mut W, b: &BSpline<D>) -> usize {
    let cps: Vec<usize> = b
        .control
        .iter()
        .map(|p| {
            if D == 3 {
                w.pt([p[0], p[1], p[2 % D]])
            } else {
                w.pt2([p[0], p[1]])
            }
        })
        .collect();
    let mut ks: Vec<f64> = Vec::new();
    let mut ms: Vec<usize> = Vec::new();
    for &k in &b.knots {
        if ks.last() == Some(&k) {
            *ms.last_mut().expect("multiplicity") += 1;
        } else {
            ks.push(k);
            ms.push(1);
        }
    }
    w.add(format!(
        "B_SPLINE_CURVE_WITH_KNOTS('',{},({}),.UNSPECIFIED.,.F.,.F.,({}),({}),.UNSPECIFIED.)",
        b.degree,
        refs(&cps),
        ms.iter()
            .map(|m| m.to_string())
            .collect::<Vec<_>>()
            .join(","),
        ks.iter().map(|&k| real(k)).collect::<Vec<_>>().join(",")
    ))
}

fn curve_entity(w: &mut W, c: &Curve) -> usize {
    match c {
        Curve::Line { origin, direction } => {
            let p = w.pt(*origin);
            let d = w.dir(*direction);
            let vct = w.add(format!("VECTOR('',#{d},1.)"));
            w.add(format!("LINE('',#{p},#{vct})"))
        }
        Curve::Circle {
            center,
            normal,
            x_axis,
            radius,
        } => {
            let a = w.ax2(*center, *normal, *x_axis);
            w.add(format!("CIRCLE('',#{a},{})", real(*radius)))
        }
        Curve::Ellipse {
            center,
            normal,
            x_axis,
            major,
            minor,
        } => {
            let a = w.ax2(*center, *normal, *x_axis);
            w.add(format!(
                "ELLIPSE('',#{a},{},{})",
                real(*major),
                real(*minor)
            ))
        }
        Curve::BSpline(b) => bspline_entity(w, b),
    }
}

fn surface_entity(w: &mut W, f: &Face) -> usize {
    let fr = f.frame;
    let a = w.ax2(fr.origin, fr.z, fr.x);
    match &f.surface {
        Surface::Cylinder { radius, .. } => {
            w.add(format!("CYLINDRICAL_SURFACE('',#{a},{})", real(*radius)))
        }
        Surface::Cone { slope, .. } => w.add(format!(
            "CONICAL_SURFACE('',#{a},{},{})",
            real(f.ref_radius),
            real(atan(*slope))
        )),
        Surface::Sphere { radius, .. } => {
            w.add(format!("SPHERICAL_SURFACE('',#{a},{})", real(*radius)))
        }
        _ => w.add(format!("PLANE('',#{a})")),
    }
}

/// Which non-void shell each void shell lies in: the smallest by volume
/// whose bounding box holds the void's.
fn void_parents(b: &Brep) -> Vec<Option<usize>> {
    let bbox = |s: &crate::model::Shell| {
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for &f in &s.faces {
            for lp in &b.faces[f as usize].loops {
                for c in &lp.coedges {
                    let e = &b.edges[c.edge as usize];
                    for vtx in [e.start, e.end] {
                        let p = b.vertices[vtx as usize];
                        for k in 0..3 {
                            lo[k] = lo[k].min(p[k]);
                            hi[k] = hi[k].max(p[k]);
                        }
                    }
                }
            }
        }
        (lo, hi)
    };
    let boxes: Vec<_> = b.shells.iter().map(bbox).collect();
    let size = |(lo, hi): ([f64; 3], [f64; 3])| (0..3).map(|k| hi[k] - lo[k]).product::<f64>();
    b.shells
        .iter()
        .enumerate()
        .map(|(i, s)| {
            if !s.void {
                return None;
            }
            let (vl, vh) = boxes[i];
            b.shells
                .iter()
                .enumerate()
                .filter(|(j, o)| {
                    !o.void && (0..3).all(|k| boxes[*j].0[k] <= vl[k] && vh[k] <= boxes[*j].1[k])
                })
                .min_by(|a, c| size(boxes[a.0]).total_cmp(&size(boxes[c.0])))
                .map(|(j, _)| j)
        })
        .collect()
}

/// Writes `brep` as a STEP AP214 file (`AUTOMOTIVE_DESIGN` schema), in
/// millimetres, with parameter-space curves on curved faces and seam
/// curves on periodic ones.
pub fn write_step(brep: &Brep, opts: &StepOptions) -> String {
    let mut w = W {
        out: Vec::new(),
        scratch: String::new(),
    };
    let name = string(&opts.product_name);
    let app = w.add("APPLICATION_CONTEXT('automotive design')".into());
    w.add(format!(
        "APPLICATION_PROTOCOL_DEFINITION('international standard','automotive_design',2000,#{app})"
    ));
    let pc = w.add(format!("PRODUCT_CONTEXT('',#{app},'mechanical')"));
    let prod = w.add(format!("PRODUCT({name},{name},'',(#{pc}))"));
    let pdf = w.add(format!("PRODUCT_DEFINITION_FORMATION('','',#{prod})"));
    let pdc = w.add(format!(
        "PRODUCT_DEFINITION_CONTEXT('part definition',#{app},'design')"
    ));
    let pd = w.add(format!("PRODUCT_DEFINITION('design','',#{pdf},#{pdc})"));
    let pds = w.add(format!("PRODUCT_DEFINITION_SHAPE('','',#{pd})"));
    let sdr = w.reserve();
    let lu = w.add("(LENGTH_UNIT()NAMED_UNIT(*)SI_UNIT(.MILLI.,.METRE.))".into());
    let au = w.add("(NAMED_UNIT(*)PLANE_ANGLE_UNIT()SI_UNIT($,.RADIAN.))".into());
    let su = w.add("(NAMED_UNIT(*)SI_UNIT($,.STERADIAN.)SOLID_ANGLE_UNIT())".into());
    let unc = w.add(format!(
        "UNCERTAINTY_MEASURE_WITH_UNIT(LENGTH_MEASURE(1.E-07),#{lu},'distance_accuracy_value','confusion accuracy')"
    ));
    let ctx = w.add(format!(
        "(GEOMETRIC_REPRESENTATION_CONTEXT(3)GLOBAL_UNCERTAINTY_ASSIGNED_CONTEXT((#{unc}))GLOBAL_UNIT_ASSIGNED_CONTEXT((#{lu},#{au},#{su}))REPRESENTATION_CONTEXT('Context3D','3D'))"
    ));
    let ctx2 = w.add(
        "(GEOMETRIC_REPRESENTATION_CONTEXT(2)PARAMETRIC_REPRESENTATION_CONTEXT()REPRESENTATION_CONTEXT('2D SPACE',''))"
            .into(),
    );
    let origin = w.ax2([0.0; 3], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]);
    // Surfaces first: parameter-space curves refer to them.
    let sids: Vec<usize> = brep
        .faces
        .iter()
        .map(|f| surface_entity(&mut w, f))
        .collect();
    let vids: Vec<usize> = brep
        .vertices
        .iter()
        .map(|&p| {
            let q = w.pt(p);
            w.add(format!("VERTEX_POINT('',#{q})"))
        })
        .collect();
    // Each edge's parameter-space curves, in face order.
    let mut pcs: Vec<Vec<(usize, &BSpline<2>)>> = vec![Vec::new(); brep.edges.len()];
    for (fi, f) in brep.faces.iter().enumerate() {
        for lp in &f.loops {
            for c in &lp.coedges {
                if let Some(pc) = &c.pcurve {
                    pcs[c.edge as usize].push((fi, pc));
                }
            }
        }
    }
    let eids: Vec<usize> = brep
        .edges
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let c3 = curve_entity(&mut w, &e.curve);
            let geom = if pcs[i].is_empty() {
                c3
            } else {
                let ps: Vec<usize> = pcs[i]
                    .iter()
                    .map(|&(fi, pc)| {
                        let b = bspline_entity(&mut w, pc);
                        let dr = w.add(format!("DEFINITIONAL_REPRESENTATION('',(#{b}),#{ctx2})"));
                        w.add(format!("PCURVE('',#{},#{dr})", sids[fi]))
                    })
                    .collect();
                let kind = if e.seam {
                    "SEAM_CURVE"
                } else {
                    "SURFACE_CURVE"
                };
                w.add(format!("{kind}('',#{c3},({}),.PCURVE_S1.)", refs(&ps)))
            };
            w.add(format!(
                "EDGE_CURVE('',#{},#{},#{geom},.T.)",
                vids[e.start as usize], vids[e.end as usize]
            ))
        })
        .collect();
    let parents = void_parents(brep);
    let face_entity = |w: &mut W, fi: usize, flip: bool| -> usize {
        let f = &brep.faces[fi];
        let bounds: Vec<usize> = f
            .loops
            .iter()
            .map(|lp| {
                let oes: Vec<usize> = lp
                    .coedges
                    .iter()
                    .map(|c| {
                        w.add(format!(
                            "ORIENTED_EDGE('',*,*,#{},{})",
                            eids[c.edge as usize],
                            if c.forward { ".T." } else { ".F." }
                        ))
                    })
                    .collect();
                let l = w.add(format!("EDGE_LOOP('',({}))", refs(&oes)));
                let kind = if lp.outer {
                    "FACE_OUTER_BOUND"
                } else {
                    "FACE_BOUND"
                };
                w.add(format!(
                    "{kind}('',#{l},{})",
                    if flip { ".F." } else { ".T." }
                ))
            })
            .collect();
        w.add(format!(
            "ADVANCED_FACE('',({}),#{},{})",
            refs(&bounds),
            sids[fi],
            if f.same_sense != flip { ".T." } else { ".F." }
        ))
    };
    let mut shell_ids = Vec::with_capacity(brep.shells.len());
    for sh in &brep.shells {
        let fs: Vec<usize> = sh
            .faces
            .iter()
            .map(|&f| face_entity(&mut w, f as usize, sh.void))
            .collect();
        shell_ids.push(w.add(format!("CLOSED_SHELL('',({}))", refs(&fs))));
    }
    let mut solids = Vec::new();
    for (i, sh) in brep.shells.iter().enumerate() {
        if sh.void && parents[i].is_some() {
            continue;
        }
        let voids: Vec<usize> = (0..brep.shells.len())
            .filter(|&j| parents[j] == Some(i))
            .map(|j| w.add(format!("ORIENTED_CLOSED_SHELL('',*,#{},.F.)", shell_ids[j])))
            .collect();
        solids.push(if voids.is_empty() {
            w.add(format!("MANIFOLD_SOLID_BREP('',#{})", shell_ids[i]))
        } else {
            w.add(format!(
                "BREP_WITH_VOIDS('',#{},({}))",
                shell_ids[i],
                refs(&voids)
            ))
        });
    }
    let mut items = solids;
    items.push(origin);
    let rep = w.add(format!(
        "ADVANCED_BREP_SHAPE_REPRESENTATION('',({}),#{ctx})",
        refs(&items)
    ));
    w.set(
        sdr,
        format!("SHAPE_DEFINITION_REPRESENTATION(#{pds},#{rep})"),
    );

    let mut s = String::with_capacity(w.out.iter().map(|l| l.len() + 10).sum::<usize>() + 1024);
    s.push_str("ISO-10303-21;\nHEADER;\n");
    let _ = writeln!(
        s,
        "FILE_DESCRIPTION(({}),'2;1');",
        string(&opts.description)
    );
    let _ = writeln!(
        s,
        "FILE_NAME({},{},({}),({}),{},{},'');",
        string(&opts.file_name),
        string(&opts.timestamp),
        string(&opts.author),
        string(&opts.organization),
        string("meshbrep"),
        string(&opts.originating_system)
    );
    s.push_str("FILE_SCHEMA(('AUTOMOTIVE_DESIGN { 1 0 10303 214 1 1 1 1 }'));\nENDSEC;\nDATA;\n");
    let mut num = String::new();
    for (i, l) in w.out.iter().enumerate() {
        num.clear();
        let _ = write!(num, "{}", i + 1);
        s.push('#');
        s.push_str(&num);
        s.push('=');
        s.push_str(l);
        s.push_str(";\n");
    }
    s.push_str("ENDSEC;\nEND-ISO-10303-21;\n");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reals_always_have_a_point() {
        assert_eq!(real(0.0), "0.");
        assert_eq!(real(-0.0), "0.");
        assert_eq!(real(1.0), "1.");
        assert_eq!(real(1.5), "1.5");
        assert_eq!(real(15.0), "1.5E1");
        assert_eq!(real(1e-7), "1.E-7");
        assert_eq!(real(-2.5e-12), "-2.5E-12");
        for x in [0.1, 1.0 / 3.0, 123456.789, -1e300, 5e-324] {
            assert_eq!(real(x).parse::<f64>().unwrap(), x);
        }
    }

    #[test]
    fn strings_are_escaped() {
        assert_eq!(string("it's"), "'it''s'");
        assert_eq!(string("a\\b"), "'a\\\\b'");
        assert_eq!(string("é"), "'\\X2\\00E9\\X0\\'");
    }
}
