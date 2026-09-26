//! VRML export (`src/io/export_wrl.cc`).

use crate::mesh::MeshRef;
use crate::text::fmt_g;

/// `export_wrl`: one `IndexedFaceSet` with OpenSCAD's fixed material and,
/// when the mesh has face colours, a colour per face (the last entry is the
/// default colour, used by faces without one). Vertices and colours print
/// as C++ streams print them (`%g`, six significant digits). Warnings
/// ("Invalid color in WRL export") go to `warnings`.
pub fn write(mesh: MeshRef<'_>, warnings: &mut Vec<String>) -> Vec<u8> {
    let mut out = String::with_capacity(512 + mesh.vertices.len() * 32 + mesh.faces.len() * 16);
    out.push_str("#VRML V2.0 utf8\n\n");
    out.push_str("Shape {\n\n");
    out.push_str("appearance Appearance { material Material {\n");
    out.push_str("ambientIntensity 0.3\n");
    out.push_str("diffuseColor 0.97647 0.843137 0.172549\n");
    out.push_str("specularColor 0.2 0.2 0.2\n");
    out.push_str("shininess 0.3\n");
    out.push_str("} }\n\n");
    out.push_str("geometry IndexedFaceSet {\n\n");
    out.push_str("creaseAngle 0.5\n\n");
    out.push_str("coord Coordinate { point [\n");
    let n = mesh.vertices.len();
    for (i, v) in mesh.vertices.iter().enumerate() {
        out.push_str(&format!("{} {} {}", fmt_g(v[0]), fmt_g(v[1]), fmt_g(v[2])));
        if i + 1 < n {
            out.push(',');
        }
        out.push('\n');
    }
    out.push_str("] }\n\n");
    out.push_str("coordIndex [\n");
    for f in mesh.faces {
        for i in f {
            out.push_str(&format!("{i},"));
        }
        out.push_str("-1\n");
    }
    out.push_str("]\n\n");
    if !mesh.color_indices.is_empty() {
        out.push_str("colorPerVertex FALSE\n\n");
        out.push_str("color Color { color [\n");
        for c in mesh.colors {
            if !c.is_valid() {
                warnings.push("Invalid color in WRL export".into());
            }
            // Alpha is dropped: VRML colours are RGB.
            let [r, g, b, _] = c.0.map(|x| fmt_g(f64::from(x)));
            out.push_str(&format!(" {r} {g} {b},\n"));
        }
        out.push_str(" 0.976471 0.843137 0.172549, # default colour\n");
        out.push_str("] }\n\n");
        out.push_str("colorIndex [\n");
        for &ci in mesh.color_indices {
            let ci = if ci >= 0 {
                ci as usize
            } else {
                mesh.colors.len()
            };
            out.push_str(&format!("{ci} "));
        }
        out.push_str("]\n\n");
    }
    out.push_str("}\n\n");
    out.push_str("}\n");
    out.into_bytes()
}
