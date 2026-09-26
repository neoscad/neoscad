// OpenSCAD's drawing (src/glview/GLView.cc, PolySetRenderer.cc,
// preview/OpenCSGRenderer.cc) with its fixed-function OpenGL state written
// out: two directional lights, colour material, no specular term, no
// textures; plus its edge shader (shaders/ViewEdges.*) and the view
// options' lines.

struct Frame {
    // Model to clip space, depth mapped to 0..1.
    clip_from_model: mat4x4<f32>,
    // The modelview's rotation (upper 3x3), for normals.
    normal_matrix: mat4x4<f32>,
    // The camera's rotated frame without its translation, to clip space
    // (crosshairs).
    clip_from_view: mat4x4<f32>,
    // The small axes' frame to clip space.
    clip_from_small_axes: mat4x4<f32>,
    // Eye-space unit direction towards GL_LIGHT0; GL_LIGHT1 is its opposite.
    light: vec4<f32>,
    // Width and height in pixels, the outline width in pixels, and 1 when
    // edges are shown (--view edges).
    viewport: vec4<f32>,
    edge_color: vec4<f32>,
    background_top: vec4<f32>,
    background_bottom: vec4<f32>,
    // x: the ambient term, y: the diffuse scale. OpenSCAD's lighting is
    // 0.2 and 1 (below); a snapshot's headlight uses others. With 0.2 and 1
    // the expressions below are OpenSCAD's to the bit: multiplying by 1 is
    // exact and 0.2 is the same f32 either way.
    shade: vec4<f32>,
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
//
// With edges shown, OpenSCAD draws faces with ViewEdges.frag instead: the
// same shading, unclamped until the framebuffer, mixed towards a lighter
// edge colour within about 1.4 pixels of each triangle edge whose
// barycentric flag is 0 (a quad's diagonal and a fan's spokes have 1 and
// are not drawn).

struct FaceVertex {
    @location(0) position: vec3<f32>,
    @location(1) normal: vec3<f32>,
    @location(2) color: vec4<f32>,
    @location(3) barycentric: vec4<f32>,
}

struct FlatShaded {
    @builtin(position) position: vec4<f32>,
    // Fixed-function colour: lit and clamped.
    @location(0) @interpolate(flat) color: vec4<f32>,
    // The vertex colour and ViewEdges.vert's `shading`, for the edge view.
    @location(1) @interpolate(flat) base: vec4<f32>,
    @location(2) @interpolate(flat) shading: f32,
    @location(3) barycentric: vec3<f32>,
}

@vertex
fn face_vs(v: FaceVertex) -> FlatShaded {
    var out: FlatShaded;
    out.position = frame.clip_from_model * vec4(v.position, 1.0);
    out.base = v.color;
    out.barycentric = v.barycentric.xyz;
    if all(v.normal == vec3(0.0)) {
        out.color = v.color;
        out.shading = -1.0;
    } else {
        // GL_NORMALIZE: normals are renormalised after the modelview.
        let n = normalize((frame.normal_matrix * vec4(v.normal, 0.0)).xyz);
        let d = dot(n, frame.light.xyz);
        let k = frame.shade.y;
        let lit = v.color.rgb * frame.shade.x + v.color.rgb * max(d, 0.0) * k
            + v.color.rgb * max(-d, 0.0) * k;
        out.color = vec4(clamp(lit, vec3(0.0), vec3(1.0)), v.color.a);
        out.shading = frame.shade.x + abs(d) * k;
    }
    return out;
}

// GLSL's smoothstep, defined also when both edges are 0 (a component that
// is 1 across the whole triangle then gives 1, as the GPU's does).
fn smooth01(high: vec3<f32>, x: vec3<f32>) -> vec3<f32> {
    let t = clamp(x / max(high, vec3(1e-30)), vec3(0.0), vec3(1.0));
    return t * t * (3.0 - 2.0 * t);
}

@fragment
fn face_fs(in: FlatShaded) -> @location(0) vec4<f32> {
    // Derivatives must be taken in uniform control flow.
    let d = fwidth(in.barycentric);
    if frame.viewport.w > 0.5 && in.shading >= 0.0 {
        // edgeFactor(): th = fade = 1.414, so smoothstep(0, 1.414 d, vBC).
        let a3 = smooth01(1.414 * d, in.barycentric);
        let f = min(min(a3.x, a3.y), a3.z);
        let edge = vec4((in.base.rgb + vec3(1.0)) / 2.0, 1.0);
        let face = vec4(in.base.rgb * in.shading, in.base.a);
        return clamp(mix(edge, face, f), vec4(0.0), vec4(1.0));
    }
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

// --- View-option lines -----------------------------------------------------------
//
// One-pixel GL_LINES in one of four spaces (see overlay.rs). A stippled line
// follows glLineStipple(3, 0xAAAA): the stipple counter counts pixels along
// the line's major axis from its first point, and bit (counter / 3) of the
// pattern decides whether a pixel is drawn, so three pixels off, three on.

struct LineVertex {
    @location(0) position: vec4<f32>,
    @location(1) start: vec4<f32>,
    @location(2) color: vec4<f32>,
    @location(3) space: u32,
    @location(4) stipple: u32,
}

struct LineOut {
    @builtin(position) position: vec4<f32>,
    @location(0) @interpolate(flat) color: vec4<f32>,
    // The first point in framebuffer pixels; x < -1e30 when not stippled.
    @location(1) @interpolate(flat) start: vec2<f32>,
}

fn line_clip(p: vec4<f32>, space: u32) -> vec4<f32> {
    switch space {
        case 0u: { return frame.clip_from_model * p; }
        case 1u: { return frame.clip_from_view * p; }
        case 2u: { return frame.clip_from_small_axes * p; }
        default: { return vec4(p.xy, 0.5, 1.0); }
    }
}

@vertex
fn line_vs(v: LineVertex) -> LineOut {
    var out: LineOut;
    out.position = line_clip(v.position, v.space);
    out.color = v.color;
    out.start = vec2(-2e30, 0.0);
    if v.stipple != 0u {
        let s = line_clip(v.start, v.space);
        let ndc = s.xy / s.w;
        out.start = vec2((ndc.x + 1.0) * 0.5 * frame.viewport.x, (1.0 - ndc.y) * 0.5 * frame.viewport.y);
    }
    return out;
}

@fragment
fn line_fs(in: LineOut) -> @location(0) vec4<f32> {
    if in.start.x > -1e30 {
        let d = abs(in.position.xy - in.start);
        let counter = u32(floor(max(d.x, d.y)));
        let bit = (counter / 3u) % 16u;
        if ((0xAAAAu >> bit) & 1u) == 0u {
            discard;
        }
    }
    return in.color;
}
