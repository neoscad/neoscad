// OpenSCAD's render-mode drawing (src/glview/GLView.cc, PolySetRenderer.cc)
// with its fixed-function OpenGL state written out: two directional lights,
// colour material, no specular term, no textures.

struct Frame {
    // Model to clip space, depth mapped to 0..1.
    clip_from_model: mat4x4<f32>,
    // The modelview's rotation (upper 3x3), for normals.
    normal_matrix: mat4x4<f32>,
    // Eye-space unit direction towards GL_LIGHT0; GL_LIGHT1 is its opposite.
    light: vec4<f32>,
    // Width and height in pixels, then the outline width in pixels.
    viewport: vec4<f32>,
    edge_color: vec4<f32>,
    background_top: vec4<f32>,
    background_bottom: vec4<f32>,
}

@group(0) @binding(0) var<uniform> frame: Frame;

// --- Background -----------------------------------------------------------
//
// GLView::paintGL clears to the background colour and, when the scheme has a
// different `background-stop`, draws a screen-sized quad from the top colour
// (y = +1) to the stop colour (y = -1), interpolated across the screen.

struct Shaded {
    @builtin(position) position: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex
fn background_vs(@builtin(vertex_index) i: u32) -> Shaded {
    // Two triangles covering clip space.
    var corners = array<vec2<f32>, 6>(
        vec2(-1.0, 1.0), vec2(1.0, 1.0), vec2(1.0, -1.0),
        vec2(-1.0, 1.0), vec2(1.0, -1.0), vec2(-1.0, -1.0),
    );
    let p = corners[i];
    var out: Shaded;
    out.position = vec4(p, 0.0, 1.0);
    out.color = select(frame.background_bottom, frame.background_top, p.y > 0.0);
    return out;
}

@fragment
fn background_fs(in: Shaded) -> @location(0) vec4<f32> {
    return in.color;
}

// --- Faces -------------------------------------------------------------------
//
// Fixed-function lighting as GLView::initializeGL sets it up: lights at
// (-1, 1, 1, 0) and (1, -1, -1, 0) in eye space with white diffuse and no
// ambient of their own, the default light-model ambient of 0.2, and
// glColorMaterial(GL_FRONT_AND_BACK, GL_AMBIENT_AND_DIFFUSE), so a vertex of
// colour c is lit as
//
//     c * 0.2 + c * max(n.L0, 0) + c * max(n.L1, 0),  clamped to 0..1,
//
// with the alpha of c. The two lights are opposite each other, so this is
// c * (0.2 + |n.L0|): which way a face's normal points does not matter, and
// OpenSCAD's normals (VBOBuilder::create_triangle) point inwards. The
// material's specular colour is GL's default black, so there is no highlight.
// Lighting is per vertex, and each triangle's vertices share its normal, so
// the colour is taken flat from the first vertex. A zero normal marks an
// unlit vertex: 2D shapes are drawn with GL_LIGHTING disabled.

struct FaceVertex {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec4<f32>,
}

struct FlatShaded {
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) color: vec4<f32>,
}

@vertex
fn face_vs(v: FaceVertex) -> FlatShaded {
    var out: FlatShaded;
    out.position = frame.clip_from_model * vec4(v.position, 1.0);
    if all(v.normal == vec3(0.0)) {
        out.color = v.color;
    } else {
        // GL_NORMALIZE: normals are renormalised after the modelview.
        let n = normalize((frame.normal_matrix * vec4(v.normal, 0.0)).xyz);
        let d = dot(n, frame.light.xyz);
        let lit = v.color.rgb * 0.2 + v.color.rgb * max(d, 0.0) + v.color.rgb * max(-d, 0.0);
        out.color = vec4(clamp(lit, vec3(0.0), vec3(1.0)), v.color.a);
    }
    return out;
}

@fragment
fn face_fs(in: FlatShaded) -> @location(0) vec4<f32> {
    return in.color;
}

// --- 2D outlines ----------------------------------------------------------------
//
// PolySetRenderer draws each outline as a GL_LINE_LOOP with glLineWidth(2)
// and the depth test off. WebGPU has only one-pixel lines, so each segment is
// an instance expanded here into the parallelogram the OpenGL specification
// rasterises for a wide non-antialiased line: the segment moved by half the
// width up and down if it is x-major, left and right if it is y-major.

struct Segment {
    @location(0) a: vec3<f32>,
    @location(1) b: vec3<f32>,
}

@vertex
fn edge_vs(@builtin(vertex_index) i: u32, s: Segment) -> @builtin(position) vec4<f32> {
    let ca = frame.clip_from_model * vec4(s.a, 1.0);
    let cb = frame.clip_from_model * vec4(s.b, 1.0);
    let half_viewport = frame.viewport.xy * 0.5;
    let d = (cb.xy / cb.w - ca.xy / ca.w) * half_viewport;
    let half_width = frame.viewport.z * 0.5;
    var offset = vec2(0.0, half_width);
    if abs(d.y) > abs(d.x) {
        offset = vec2(half_width, 0.0);
    }
    // Corners: a-, a+, b- and b-, a+, b+.
    let at_b = i == 2u || i == 3u || i == 5u;
    let side = select(-1.0, 1.0, i == 1u || i == 4u || i == 5u);
    let c = select(ca, cb, at_b);
    let shift = offset * side / half_viewport * c.w;
    return vec4(c.xy + shift, c.z, c.w);
}

@fragment
fn edge_fs() -> @location(0) vec4<f32> {
    return frame.edge_color;
}
