//! OpenSCAD's camera (`src/glview/Camera.{h,cc}`) and the matrices
//! `GLView::setupCamera` builds from it (`src/glview/GLView.cc`).
//!
//! OpenSCAD keeps a "gimbal" camera: a translation (`object_trans`, the
//! negated `$vpt`), Euler angles (`object_rot`, stored as
//! `90 - $vpr.x, -$vpr.y, -$vpr.z` wrapped into 0..360) and a viewer
//! distance. The 6-number `--camera` form (eye and centre) is converted to
//! those on the spot, so there is one camera model. The fields are kept in
//! OpenSCAD's internal form rather than as `$vp*` so that a camera set up
//! from the command line reaches the matrices without a round trip through
//! the user-space values, which would move the angles by an ulp.

use io::trig::{atan2_degrees, sin_degrees, tan_degrees};

/// `DEFAULT_DISTANCE`, `DEFAULT_FOV` and the default image size
/// (`Camera.cc:14-17`).
pub const DEFAULT_DISTANCE: f64 = 140.0;
pub const DEFAULT_FOV: f64 = 22.5;
pub const DEFAULT_WIDTH: u32 = 512;
pub const DEFAULT_HEIGHT: u32 = 512;

/// `Camera::ProjectionType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Projection {
    #[default]
    Perspective,
    Orthogonal,
}

/// An axis-aligned box, as `BoundingBox` (Eigen's `AlignedBox3d`) holds
/// one. `None` where OpenSCAD's box would be empty.
pub type BoundingBox = Option<([f64; 3], [f64; 3])>;

/// OpenSCAD's `Camera`, in its internal (gimbal) form.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Camera {
    /// `object_trans`: the negated view centre (`-$vpt`).
    pub object_trans: [f64; 3],
    /// `object_rot`: rotations about x, y and z in degrees, applied in that
    /// order after the look-at transform.
    pub object_rot: [f64; 3],
    /// `viewer_distance` (`$vpd`).
    pub viewer_distance: f64,
    /// `fov` (`$vpf`), the vertical field of view in degrees.
    pub fov: f64,
    pub projection: Projection,
    /// `--viewall`: fit the model's bounding box before drawing.
    pub viewall: bool,
    /// `--autocenter`: look at the bounding box's centre.
    pub autocenter: bool,
    /// Set by `--camera`: the file's `$vp*` assignments are ignored.
    pub locked: bool,
    /// `--imgsize`.
    pub pixel_width: u32,
    pub pixel_height: u32,
}

impl Default for Camera {
    /// `Camera::Camera()`, which calls `resetView()`.
    fn default() -> Self {
        let mut c = Camera {
            object_trans: [0.0; 3],
            object_rot: [0.0; 3],
            viewer_distance: DEFAULT_DISTANCE,
            fov: DEFAULT_FOV,
            projection: Projection::Perspective,
            viewall: false,
            autocenter: false,
            locked: false,
            pixel_width: DEFAULT_WIDTH,
            pixel_height: DEFAULT_HEIGHT,
        };
        c.reset_view();
        c
    }
}

/// `wrap` in `Camera.cc`: an angle forced into 0..360 by `fmod`.
fn wrap(angle: f64) -> f64 {
    (360.0 + angle) % 360.0
}

impl Camera {
    /// `Camera::resetView`: `$vpr = [55, 0, 25]`, `$vpt = 0`, distance 140,
    /// fov 22.5.
    pub fn reset_view(&mut self) {
        self.set_vpr(55.0, 0.0, 25.0);
        self.set_vpt(0.0, 0.0, 0.0);
        self.set_vpd(DEFAULT_DISTANCE);
        self.set_vpf(DEFAULT_FOV);
    }

    /// `Camera::setup` from `--camera` numbers: 7 for translate, rotate and
    /// distance; 6 for an eye point and the point it looks at. Returns
    /// `false` for any other count (OpenSCAD asserts; its command line
    /// rejects the count earlier).
    pub fn setup(&mut self, p: &[f64]) -> bool {
        match p.len() {
            7 => {
                self.set_vpt(p[0], p[1], p[2]);
                self.set_vpr(p[3], p[4], p[5]);
                self.viewer_distance = p[6];
            }
            6 => {
                let eye = [p[0], p[1], p[2]];
                let center = [p[3], p[4], p[5]];
                self.object_trans = center.map(|c| -c);
                let dir = [center[0] - eye[0], center[1] - eye[1], center[2] - eye[2]];
                self.viewer_distance = norm(dir);
                // Looking straight down or up has no heading; OpenSCAD
                // picks 0 or 180 degrees (`Camera.cc:45`).
                self.object_rot[2] = if dir[1] == 0.0 && dir[0] == 0.0 {
                    if dir[2] < 0.0 { 0.0 } else { 180.0 }
                } else {
                    -atan2_degrees(dir[1], dir[0]) + 90.0
                };
                self.object_rot[1] = 0.0;
                let horizontal = norm([dir[0], dir[1], 0.0]);
                self.object_rot[0] = -atan2_degrees(dir[2], horizontal);
            }
            _ => return false,
        }
        self.locked = true;
        true
    }

    /// `$vpt`: the point the camera looks at.
    pub fn vpt(&self) -> [f64; 3] {
        self.object_trans.map(|t| -t)
    }

    pub fn set_vpt(&mut self, x: f64, y: f64, z: f64) {
        self.object_trans = [-x, -y, -z];
    }

    /// `$vpr`, in user space (`Camera::getVpr`).
    pub fn vpr(&self) -> [f64; 3] {
        let r = self.object_rot;
        [wrap(90.0 - r[0]), wrap(-r[1]), wrap(-r[2])]
    }

    pub fn set_vpr(&mut self, x: f64, y: f64, z: f64) {
        self.object_rot = [wrap(90.0 - x), wrap(-y), wrap(-z)];
    }

    pub fn set_vpd(&mut self, d: f64) {
        self.viewer_distance = d;
    }

    pub fn set_vpf(&mut self, f: f64) {
        self.fov = f;
    }

    /// `Camera::viewAll`: move the camera back until the bounding box's
    /// bounding sphere fits the field of view, first centring on the box
    /// when `autocenter` is set. An empty box resets the centre and
    /// distance instead.
    pub fn view_all(&mut self, bbox: BoundingBox) {
        let Some((lo, hi)) = bbox else {
            self.set_vpt(0.0, 0.0, 0.0);
            self.set_vpd(DEFAULT_DISTANCE);
            return;
        };
        let center: [f64; 3] = std::array::from_fn(|i| (lo[i] + hi[i]) / 2.0);
        if self.autocenter {
            self.object_trans = center.map(|c| -c);
        }
        let diagonal: [f64; 3] = std::array::from_fn(|i| hi[i] - lo[i]);
        let bbox_radius = norm(diagonal) / 2.0;
        let offset: [f64; 3] = std::array::from_fn(|i| center[i] + self.object_trans[i]);
        let radius = norm(offset) + bbox_radius;
        self.viewer_distance = radius / sin_degrees(self.fov / 2.0);
    }

    /// The width over the height of the image (`GLView::resizeGL`).
    pub fn aspect_ratio(&self) -> f64 {
        f64::from(self.pixel_width) / f64::from(self.pixel_height)
    }

    /// The projection and modelview matrices `GLView::setupCamera` and
    /// `paintGL` leave for drawing the model, in OpenGL's conventions
    /// (right-handed eye space, clip-space z in -1..1). Row-major.
    ///
    /// - Perspective: `gluPerspective(fov, aspect, 0.1 * dist, 100 * dist)`.
    /// - Orthogonal: `glOrtho` over the height the field of view covers at
    ///   the viewer distance, with depth from `-100 * dist` to `100 * dist`.
    /// - Modelview: `gluLookAt` from `(0, -dist, 0)` towards the origin with
    ///   z up, then `glRotated` about x, y and z by `object_rot`, then
    ///   `glTranslated(object_trans)` (applied once in `setupCamera`,
    ///   undone, and applied again by `paintGL`: the net is one).
    pub fn gl_matrices(&self) -> GlMatrices {
        let dist = self.viewer_distance;
        let aspect = self.aspect_ratio();
        let projection = match self.projection {
            Projection::Perspective => perspective(self.fov, aspect, 0.1 * dist, 100.0 * dist),
            Projection::Orthogonal => {
                let height = dist * tan_degrees(self.fov / 2.0);
                ortho(
                    -height * aspect,
                    height * aspect,
                    -height,
                    height,
                    -100.0 * dist,
                    100.0 * dist,
                )
            }
        };
        let mut modelview = look_at_from_minus_y(dist);
        modelview = mul(&modelview, &rotation(self.object_rot[0], [1.0, 0.0, 0.0]));
        modelview = mul(&modelview, &rotation(self.object_rot[1], [0.0, 1.0, 0.0]));
        modelview = mul(&modelview, &rotation(self.object_rot[2], [0.0, 0.0, 1.0]));
        modelview = mul(&modelview, &translation(self.object_trans));
        GlMatrices {
            projection,
            modelview,
        }
    }
}

/// A 4x4 matrix, row-major (`m[row][column]`).
pub type Mat4 = [[f64; 4]; 4];

/// The fixed-function matrices OpenSCAD draws the model with.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GlMatrices {
    pub projection: Mat4,
    pub modelview: Mat4,
}

impl GlMatrices {
    /// Model to clip space for a target whose depth range is 0..1 (WebGPU,
    /// Metal, Direct3D, Vulkan), where OpenGL's is -1..1: `z' = (z + w) / 2`.
    /// x, y and w, and so every pixel position, are OpenGL's.
    pub fn clip_from_model_zero_to_one(&self) -> Mat4 {
        zero_to_one(&mul(&self.projection, &self.modelview))
    }

    /// The matrix fixed-function lighting transforms normals with: the
    /// inverse transpose of the modelview's upper 3x3. OpenSCAD's modelview
    /// is a rotation followed by translations, so that is the rotation
    /// itself; `GL_NORMALIZE` then renormalises, which the shader does too.
    pub fn normal_matrix(&self) -> [[f64; 3]; 3] {
        std::array::from_fn(|r| std::array::from_fn(|c| self.modelview[r][c]))
    }
}

/// An OpenGL clip matrix (depth -1..1) for a target whose depth range is
/// 0..1: `z' = (z + w) / 2`.
pub fn zero_to_one(gl: &Mat4) -> Mat4 {
    let mut m = *gl;
    for c in 0..4 {
        m[2][c] = 0.5 * gl[2][c] + 0.5 * gl[3][c];
    }
    m
}

fn norm(v: [f64; 3]) -> f64 {
    (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt()
}

pub fn mul(a: &Mat4, b: &Mat4) -> Mat4 {
    std::array::from_fn(|r| std::array::from_fn(|c| (0..4).map(|k| a[r][k] * b[k][c]).sum()))
}

pub(crate) fn identity() -> Mat4 {
    std::array::from_fn(|r| std::array::from_fn(|c| if r == c { 1.0 } else { 0.0 }))
}

/// `gluPerspective` as GLU computes it (`project.c`): the cotangent of half
/// the field of view from `cos / sin` of the half angle in radians.
fn perspective(fovy: f64, aspect: f64, near: f64, far: f64) -> Mat4 {
    let radians = fovy / 2.0 * std::f64::consts::PI / 180.0;
    let delta_z = far - near;
    let cotangent = radians.cos() / radians.sin();
    let mut m = identity();
    m[0][0] = cotangent / aspect;
    m[1][1] = cotangent;
    m[2][2] = -(far + near) / delta_z;
    m[3][2] = -1.0;
    m[2][3] = -2.0 * near * far / delta_z;
    m[3][3] = 0.0;
    m
}

/// `glOrtho`.
pub(crate) fn ortho(l: f64, r: f64, b: f64, t: f64, n: f64, f: f64) -> Mat4 {
    let mut m = identity();
    m[0][0] = 2.0 / (r - l);
    m[1][1] = 2.0 / (t - b);
    m[2][2] = -2.0 / (f - n);
    m[0][3] = -(r + l) / (r - l);
    m[1][3] = -(t + b) / (t - b);
    m[2][3] = -(f + n) / (f - n);
    m
}

/// `gluLookAt(0, -dist, 0, 0, 0, 0, 0, 0, 1)`: forward is +y and up is +z,
/// so eye space is `(x, z, -(y + dist))`.
pub(crate) fn look_at_from_minus_y(dist: f64) -> Mat4 {
    [
        [1.0, 0.0, 0.0, 0.0],
        [0.0, 0.0, 1.0, 0.0],
        [0.0, -1.0, 0.0, -dist],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

/// `glRotated(angle, x, y, z)` for a unit axis.
pub(crate) fn rotation(angle: f64, axis: [f64; 3]) -> Mat4 {
    let a = angle.to_radians();
    let (s, c) = a.sin_cos();
    let [x, y, z] = axis;
    let t = 1.0 - c;
    [
        [x * x * t + c, x * y * t - z * s, x * z * t + y * s, 0.0],
        [y * x * t + z * s, y * y * t + c, y * z * t - x * s, 0.0],
        [x * z * t - y * s, y * z * t + x * s, z * z * t + c, 0.0],
        [0.0, 0.0, 0.0, 1.0],
    ]
}

pub(crate) fn translation(t: [f64; 3]) -> Mat4 {
    let mut m = identity();
    m[0][3] = t[0];
    m[1][3] = t[1];
    m[2][3] = t[2];
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn apply(m: &Mat4, p: [f64; 3]) -> [f64; 4] {
        std::array::from_fn(|r| m[r][0] * p[0] + m[r][1] * p[1] + m[r][2] * p[2] + m[r][3])
    }

    #[test]
    fn default_camera_is_openscads_reset_view() {
        let c = Camera::default();
        assert_eq!(c.object_rot, [35.0, 0.0, 335.0]);
        assert_eq!(c.vpr(), [55.0, 0.0, 25.0]);
        assert_eq!(c.viewer_distance, 140.0);
        assert_eq!((c.pixel_width, c.pixel_height), (512, 512));
    }

    #[test]
    fn eye_camera_looks_from_the_eye_at_the_centre() {
        let mut c = Camera::default();
        assert!(c.setup(&[120.0, 80.0, 60.0, 0.0, 0.0, 0.0]));
        assert!(c.locked);
        let m = c.gl_matrices().modelview;
        // The eye maps to the eye-space origin and the centre straight
        // ahead (-z) at the viewer distance.
        let eye = apply(&m, [120.0, 80.0, 60.0]);
        assert!(eye[..3].iter().all(|v| v.abs() < 1e-9), "{eye:?}");
        let center = apply(&m, [0.0, 0.0, 0.0]);
        let d = (120.0f64 * 120.0 + 80.0 * 80.0 + 60.0 * 60.0).sqrt();
        assert!(center[0].abs() < 1e-9 && center[1].abs() < 1e-9);
        assert!((center[2] + d).abs() < 1e-9);
    }

    #[test]
    fn view_all_fits_the_bounding_sphere() {
        let mut c = Camera {
            autocenter: true,
            ..Camera::default()
        };
        c.view_all(Some(([0.0; 3], [10.0; 3])));
        assert_eq!(c.vpt(), [5.0; 3]);
        let r = (300.0f64).sqrt() / 2.0;
        assert!((c.viewer_distance - r / sin_degrees(11.25)).abs() < 1e-12);
        c.view_all(None);
        assert_eq!((c.vpt(), c.viewer_distance), ([0.0; 3], DEFAULT_DISTANCE));
    }

    #[test]
    fn clip_depth_is_remapped_to_zero_one() {
        let c = Camera::default();
        let gl = c.gl_matrices();
        let m = gl.clip_from_model_zero_to_one();
        // The near plane of the perspective projection lands at depth 0.
        let near = 0.1 * c.viewer_distance;
        let eye_near = [0.0, 0.0, -near, 1.0];
        let proj = &gl.projection;
        let clip: [f64; 4] =
            std::array::from_fn(|r| (0..4).map(|k| proj[r][k] * eye_near[k]).sum());
        assert!((clip[2] / clip[3] + 1.0).abs() < 1e-12);
        let z01 = 0.5 * clip[2] + 0.5 * clip[3];
        assert!((z01 / clip[3]).abs() < 1e-12);
        assert_eq!(m[0], mul(&gl.projection, &gl.modelview)[0]);
    }
}
