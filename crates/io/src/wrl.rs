//! VRML export (`src/io/export_wrl.cc`).

use crate::mesh::MeshRef;
use crate::text::{write_g, write_int};

/// `export_wrl`: one `IndexedFaceSet` with OpenSCAD's fixed material and,
/// when the mesh has face colours, a colour per face (the last entry is the
/// default colour, used by faces without one). Vertices and colours print
/// as C++ streams print them (`%g`, six significant digits). Warnings
/// ("Invalid color in WRL export") go to `warnings`.
pub fn write(mesh: MeshRef<'_>, warnings: &mut Vec<String>) -> Vec<u8> {
    // Numbers go straight into the output (`write_g`, `write_int`), as in
    // the OFF writer: a `String` per number was most of a big export.
    let mut out = Vec::with_capacity(512 + mesh.vertices.len() * 32 + mesh.faces.len() * 16);
    out.extend_from_slice(b"#VRML V2.0 utf8\n\n");
    out.extend_from_slice(b"Shape {\n\n");
    out.extend_from_slice(b"appearance Appearance { material Material {\n");
    out.extend_from_slice(b"ambientIntensity 0.3\n");
    out.extend_from_slice(b"diffuseColor 0.97647 0.843137 0.172549\n");
    out.extend_from_slice(b"specularColor 0.2 0.2 0.2\n");
    out.extend_from_slice(b"shininess 0.3\n");
    out.extend_from_slice(b"} }\n\n");
    out.extend_from_slice(b"geometry IndexedFaceSet {\n\n");
    out.extend_from_slice(b"creaseAngle 0.5\n\n");
    out.extend_from_slice(b"coord Coordinate { point [\n");
    let n = mesh.vertices.len();
    for (i, v) in mesh.vertices.iter().enumerate() {
        write_g(&mut out, v[0]);
        out.push(b' ');
        write_g(&mut out, v[1]);
        out.push(b' ');
        write_g(&mut out, v[2]);
        if i + 1 < n {
            out.push(b',');
        }
        out.push(b'\n');
    }
    out.extend_from_slice(b"] }\n\n");
    out.extend_from_slice(b"coordIndex [\n");
    for f in mesh.faces {
        for &i in f {
            write_int(&mut out, i);
            out.push(b',');
        }
        out.extend_from_slice(b"-1\n");
    }
    out.extend_from_slice(b"]\n\n");
    if !mesh.color_indices.is_empty() {
        out.extend_from_slice(b"colorPerVertex FALSE\n\n");
        out.extend_from_slice(b"color Color { color [\n");
        for c in mesh.colors {
            if !c.is_valid() {
                warnings.push("Invalid color in WRL export".into());
            }
            // Alpha is dropped: VRML colours are RGB.
            for &x in &c.0[..3] {
                out.push(b' ');
                write_g(&mut out, f64::from(x));
            }
            out.extend_from_slice(b",\n");
        }
        out.extend_from_slice(b" 0.976471 0.843137 0.172549, # default colour\n");
        out.extend_from_slice(b"] }\n\n");
        out.extend_from_slice(b"colorIndex [\n");
        for &ci in mesh.color_indices {
            let ci = if ci >= 0 {
                ci as usize
            } else {
                mesh.colors.len()
            };
            write_int(&mut out, ci);
            out.push(b' ');
        }
        out.extend_from_slice(b"]\n\n");
    }
    out.extend_from_slice(b"}\n\n");
    out.extend_from_slice(b"}\n");
    out
}
