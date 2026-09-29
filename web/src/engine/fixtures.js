// Canned geometry for the mock engine: boxes, packed as the worker packs
// its scenes (render::packed; docs/web-protocol.md, "PackedScene"), and
// the exports made from them.
//
// The mock keeps its models as meshes of boxes ({positions, normals,
// indices, color}); `packScene` writes them in the wire's form, so the
// viewer and the tests see exactly what the real worker sends:
//
//   {faces: ArrayBuffer (44 bytes a vertex), edges: ArrayBuffer (24 bytes a
//    segment), meta: JSON text {draws, image_csg, bbox, edge_color}}

/// A box from `min` to `max` with flat normals (24 vertices, 12 triangles).
export function boxMesh(min, max, color = null) {
  const [x0, y0, z0] = min;
  const [x1, y1, z1] = max;
  const faces = [
    [[0, 0, -1], [[x0, y0, z0], [x0, y1, z0], [x1, y1, z0], [x1, y0, z0]]],
    [[0, 0, 1], [[x0, y0, z1], [x1, y0, z1], [x1, y1, z1], [x0, y1, z1]]],
    [[0, -1, 0], [[x0, y0, z0], [x1, y0, z0], [x1, y0, z1], [x0, y0, z1]]],
    [[0, 1, 0], [[x0, y1, z0], [x0, y1, z1], [x1, y1, z1], [x1, y1, z0]]],
    [[-1, 0, 0], [[x0, y0, z0], [x0, y0, z1], [x0, y1, z1], [x0, y1, z0]]],
    [[1, 0, 0], [[x1, y0, z0], [x1, y1, z0], [x1, y1, z1], [x1, y0, z1]]],
  ];
  const positions = new Float32Array(24 * 3);
  const normals = new Float32Array(24 * 3);
  const indices = new Uint32Array(36);
  faces.forEach(([n, quad], f) => {
    quad.forEach((p, i) => {
      positions.set(p, (f * 4 + i) * 3);
      normals.set(n, (f * 4 + i) * 3);
    });
    const b = f * 4;
    indices.set([b, b + 1, b + 2, b, b + 2, b + 3], f * 6);
  });
  return { positions, normals, indices, color };
}

/// The mock's model: a box whose size follows the text's length, so an
/// edit visibly changes the view, and a second one per `part(` found.
export function mockModel(text, mode) {
  const s = 10 + Math.min(20, text.length / 200);
  const meshes = [boxMesh([-s, -s, 0], [s, s, s], mode === "preview" ? [0.98, 0.84, 0.17, 1] : null)];
  const parts = (text.match(/\bpart\s*\(/g) ?? []).length;
  for (let i = 0; i < parts; i++) {
    const x = s + 4 + i * 12;
    meshes.push(boxMesh([x, -5, 0], [x + 10, 5, 6], [0.42, 0.36, 0.95, 1]));
  }
  const maxX = parts ? s + 4 + parts * 12 - 2 : s;
  return { bbox: { min: [-s, -s, 0], max: [maxX, s, s] }, meshes };
}

/// Bytes of one face vertex and one edge segment (render::packed's
/// FACE_VERTEX_SIZE and EDGE_SEGMENT_SIZE).
export const FACE_VERTEX_SIZE = 44;
export const EDGE_SEGMENT_SIZE = 24;

/// A model as the wire's packed scene: every triangle's three vertices
/// (position, normal, colour, barycentric bytes), one opaque draw over all
/// of them, no 2D edges. Uncoloured meshes take Cornfield's face colour,
/// as the worker bakes the scheme in.
export function packScene(model) {
  const tris = model.meshes.reduce((n, m) => n + m.indices.length / 3, 0);
  const faces = new ArrayBuffer(tris * 3 * FACE_VERTEX_SIZE);
  const view = new DataView(faces);
  let at = 0;
  for (const m of model.meshes) {
    const color = m.color ?? [0.976, 0.843, 0.173, 1];
    for (let t = 0; t < m.indices.length; t += 3) {
      for (let k = 0; k < 3; k++) {
        const i = m.indices[t + k] * 3;
        const floats = [
          m.positions[i], m.positions[i + 1], m.positions[i + 2],
          m.normals[i], m.normals[i + 1], m.normals[i + 2],
          ...color,
        ];
        floats.forEach((f, j) => view.setFloat32(at + j * 4, f, true));
        view.setUint8(at + 40 + k, 1);
        at += FACE_VERTEX_SIZE;
      }
    }
  }
  const meta = {
    draws: [{ first: 0, count: tris * 3, state: { cull: "None", depth: "Less", color_write: true, bias: false } }],
    image_csg: [],
    bbox: [model.bbox.min, model.bbox.max],
    edge_color: [1, 0, 0, 1],
  };
  return { faces, edges: new ArrayBuffer(0), meta: JSON.stringify(meta) };
}

function triangles(model) {
  const out = [];
  for (const m of model.meshes) {
    for (let t = 0; t < m.indices.length; t += 3) {
      const tri = [];
      for (let k = 0; k < 3; k++) {
        const i = m.indices[t + k] * 3;
        tri.push([m.positions[i], m.positions[i + 1], m.positions[i + 2]]);
      }
      out.push(tri);
    }
  }
  return out;
}

const enc = new TextEncoder();

export function stl(scene, name = "mock") {
  const lines = [`solid ${name}`];
  for (const [a, b, c] of triangles(scene)) {
    lines.push("  facet normal 0 0 0", "    outer loop");
    for (const p of [a, b, c]) lines.push(`      vertex ${p.join(" ")}`);
    lines.push("    endloop", "  endfacet");
  }
  lines.push(`endsolid ${name}`, "");
  return enc.encode(lines.join("\n"));
}

export function off(scene) {
  const tris = triangles(scene);
  const lines = ["OFF", `${tris.length * 3} ${tris.length} 0`];
  for (const tri of tris) for (const p of tri) lines.push(p.join(" "));
  tris.forEach((_, i) => lines.push(`3 ${i * 3} ${i * 3 + 1} ${i * 3 + 2}`));
  return enc.encode(lines.join("\n") + "\n");
}

export function svg() {
  return enc.encode(
    '<?xml version="1.0" standalone="no"?>\n' +
      '<svg xmlns="http://www.w3.org/2000/svg" width="20mm" height="20mm" viewBox="-10 -10 20 20">\n' +
      '<path d="M -10,-10 L 10,-10 L 10,10 L -10,10 z" stroke="black" fill="lightgray" stroke-width="0.5"/>\n' +
      "</svg>\n",
  );
}

// --- A store-only zip, for a 3MF the viewer apps can open ------------------

const CRC = (() => {
  const t = new Uint32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c >>> 0;
  }
  return t;
})();

function crc32(bytes) {
  let c = 0xffffffff;
  for (const b of bytes) c = CRC[(c ^ b) & 0xff] ^ (c >>> 8);
  return (c ^ 0xffffffff) >>> 0;
}

export function zip(files) {
  const parts = [];
  const central = [];
  let offset = 0;
  for (const [name, data] of files) {
    const n = enc.encode(name);
    const crc = crc32(data);
    const local = new DataView(new ArrayBuffer(30));
    local.setUint32(0, 0x04034b50, true);
    local.setUint16(4, 20, true);
    local.setUint32(14, crc, true);
    local.setUint32(18, data.length, true);
    local.setUint32(22, data.length, true);
    local.setUint16(26, n.length, true);
    parts.push(new Uint8Array(local.buffer), n, data);
    const cd = new DataView(new ArrayBuffer(46));
    cd.setUint32(0, 0x02014b50, true);
    cd.setUint16(4, 20, true);
    cd.setUint16(6, 20, true);
    cd.setUint32(16, crc, true);
    cd.setUint32(20, data.length, true);
    cd.setUint32(24, data.length, true);
    cd.setUint16(28, n.length, true);
    cd.setUint32(42, offset, true);
    central.push(new Uint8Array(cd.buffer), n);
    offset += 30 + n.length + data.length;
  }
  const cdSize = central.reduce((s, p) => s + p.length, 0);
  const end = new DataView(new ArrayBuffer(22));
  end.setUint32(0, 0x06054b50, true);
  end.setUint16(8, files.length, true);
  end.setUint16(10, files.length, true);
  end.setUint32(12, cdSize, true);
  end.setUint32(16, offset, true);
  const all = [...parts, ...central, new Uint8Array(end.buffer)];
  const out = new Uint8Array(all.reduce((s, p) => s + p.length, 0));
  let at = 0;
  for (const p of all) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

export function threeMF(scene) {
  const verts = [];
  const tris = [];
  for (const [a, b, c] of triangles(scene)) {
    const i = verts.length;
    verts.push(a, b, c);
    tris.push([i, i + 1, i + 2]);
  }
  const model =
    '<?xml version="1.0" encoding="UTF-8"?>\n' +
    '<model unit="millimeter" xmlns="http://schemas.microsoft.com/3dmanufacturing/core/2015/02">\n' +
    '<resources><object id="1" type="model"><mesh><vertices>\n' +
    verts.map(([x, y, z]) => `<vertex x="${x}" y="${y}" z="${z}"/>`).join("\n") +
    "\n</vertices><triangles>\n" +
    tris.map(([a, b, c]) => `<triangle v1="${a}" v2="${b}" v3="${c}"/>`).join("\n") +
    '\n</triangles></mesh></object></resources><build><item objectid="1"/></build></model>\n';
  const types =
    '<?xml version="1.0" encoding="UTF-8"?>\n<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types">' +
    '<Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/>' +
    '<Default Extension="model" ContentType="application/vnd.ms-package.3dmanufacturing-3dmodel+xml"/></Types>\n';
  const rels =
    '<?xml version="1.0" encoding="UTF-8"?>\n<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">' +
    '<Relationship Target="/3D/3dmodel.model" Id="rel0" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel"/></Relationships>\n';
  return zip([
    ["[Content_Types].xml", enc.encode(types)],
    ["_rels/.rels", enc.encode(rels)],
    ["3D/3dmodel.model", enc.encode(model)],
  ]);
}
