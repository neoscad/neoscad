//! Where a model sits in a view that shows several: a rigid transform
//! (a rotation and a translation, no scale, no mirror) from the model's
//! own coordinates to the view's.
//!
//! A packed scene has no object identity (its draws are grouped by render
//! state, not by object), so a page that wants parts to move without
//! re-evaluating the program renders each moving part as a scene of its
//! own and places each one ([`crate::viewport::Viewport::add_model`]).
//! The transform is folded into each model's per-frame uniforms
//! (`clip_from_model` times the transform, the normal matrix times its
//! rotation), so the shaders, the geometry on the GPU and the single-model
//! path the desktop app draws with are all unchanged.
//!
//! Only rigid motions are accepted. Normals are carried by the rotation
//! part alone, which is the inverse transpose of a rotation; a scale or
//! shear would need the true inverse transpose, and a mirror would also
//! flip the triangles' winding, which the face pipelines' culling depends
//! on (a mirrored model would draw its back faces). Refusing them is
//! cheaper than supporting them, and nothing that moves a rigid body
//! needs them.

use crate::camera::{BoundingBox, Mat4, mul};

/// How far a matrix may be from rigid and still count as rigid: its
/// columns' lengths and dot products against 1 and 0. A matrix composed
/// in JavaScript and sent as 32-bit floats is rigid to about 1e-6; a
/// matrix with a scale of 1.01 is not one a caller meant to be rigid.
const RIGID_TOLERANCE: f64 = 1e-3;

/// A rigid transform, `m[row][col]` like [`crate::camera`]'s matrices
/// (so it multiplies column vectors: `p' = m * p`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    m: Mat4,
}

/// Why a matrix is not a [`Placement`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlacementError {
    /// Not 16 numbers.
    Length(usize),
    /// A NaN or an infinity.
    NotFinite,
    /// The bottom row is not `0 0 0 1` (a projective matrix).
    NotAffine,
    /// The upper 3x3 is not a rotation: it scales, shears or mirrors.
    NotRigid,
}

impl std::fmt::Display for PlacementError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlacementError::Length(n) => {
                write!(f, "a model matrix has 16 numbers (column-major), not {n}")
            }
            PlacementError::NotFinite => f.write_str("a model matrix must be finite"),
            PlacementError::NotAffine => {
                f.write_str("a model matrix's bottom row must be 0, 0, 0, 1")
            }
            PlacementError::NotRigid => f.write_str(
                "a model matrix must be a rotation and a translation \
                 (no scale, shear or mirror)",
            ),
        }
    }
}

impl std::error::Error for PlacementError {}

impl Default for Placement {
    fn default() -> Self {
        Placement::IDENTITY
    }
}

impl Placement {
    pub const IDENTITY: Placement = Placement {
        m: [
            [1.0, 0.0, 0.0, 0.0],
            [0.0, 1.0, 0.0, 0.0],
            [0.0, 0.0, 1.0, 0.0],
            [0.0, 0.0, 0.0, 1.0],
        ],
    };

    /// A placement from 16 numbers in column-major order (WebGL's,
    /// WebGPU's and DOMMatrix's `toFloat32Array()` order: the
    /// translation is elements 12, 13 and 14).
    pub fn from_column_major(a: &[f32]) -> Result<Placement, PlacementError> {
        if a.len() != 16 {
            return Err(PlacementError::Length(a.len()));
        }
        if a.iter().any(|x| !x.is_finite()) {
            return Err(PlacementError::NotFinite);
        }
        let m: Mat4 = std::array::from_fn(|r| std::array::from_fn(|c| f64::from(a[c * 4 + r])));
        Placement::from_matrix(m)
    }

    /// A placement from a row-indexed matrix (`m[row][col]`).
    pub fn from_matrix(m: Mat4) -> Result<Placement, PlacementError> {
        if m.iter().flatten().any(|x| !x.is_finite()) {
            return Err(PlacementError::NotFinite);
        }
        if m[3] != [0.0, 0.0, 0.0, 1.0] {
            return Err(PlacementError::NotAffine);
        }
        let col = |c: usize| [m[0][c], m[1][c], m[2][c]];
        let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
        for i in 0..3 {
            for j in i..3 {
                let want = if i == j { 1.0 } else { 0.0 };
                if (dot(col(i), col(j)) - want).abs() > RIGID_TOLERANCE {
                    return Err(PlacementError::NotRigid);
                }
            }
        }
        // Orthonormal with a negative determinant is a mirror.
        let (x, y, z) = (col(0), col(1), col(2));
        let det = x[0] * (y[1] * z[2] - y[2] * z[1]) - y[0] * (x[1] * z[2] - x[2] * z[1])
            + z[0] * (x[1] * y[2] - x[2] * y[1]);
        if det <= 0.0 {
            return Err(PlacementError::NotRigid);
        }
        Ok(Placement { m })
    }

    /// The matrix, `m[row][col]`.
    pub fn matrix(&self) -> &Mat4 {
        &self.m
    }

    /// Whether this is exactly the identity (models at identity draw
    /// exactly as a model shown alone).
    pub fn is_identity(&self) -> bool {
        *self == Placement::IDENTITY
    }

    /// `clip_from_model` for a model placed so: the view's matrix times
    /// this one.
    pub fn after(&self, clip_from_model: &Mat4) -> Mat4 {
        mul(clip_from_model, &self.m)
    }

    /// The normal matrix for a model placed so: the view's times this
    /// rotation (a rotation's inverse transpose is itself).
    pub fn normals_after(&self, normal_matrix: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
        std::array::from_fn(|r| {
            std::array::from_fn(|c| (0..3).map(|k| normal_matrix[r][k] * self.m[k][c]).sum())
        })
    }

    /// A point of the model in the view's coordinates.
    pub fn apply(&self, p: [f64; 3]) -> [f64; 3] {
        std::array::from_fn(|r| {
            self.m[r][0] * p[0] + self.m[r][1] * p[1] + self.m[r][2] * p[2] + self.m[r][3]
        })
    }

    /// The axis-aligned box around `bbox` placed so: the box of its eight
    /// corners moved, which holds the moved model (it can be larger than
    /// the model's own box would be, at an angle).
    pub fn bounds(&self, bbox: BoundingBox) -> BoundingBox {
        let (lo, hi) = bbox?;
        if self.is_identity() {
            return Some((lo, hi));
        }
        let mut out_lo = [f64::INFINITY; 3];
        let mut out_hi = [f64::NEG_INFINITY; 3];
        for i in 0..8 {
            let corner = [
                if i & 1 == 0 { lo[0] } else { hi[0] },
                if i & 2 == 0 { lo[1] } else { hi[1] },
                if i & 4 == 0 { lo[2] } else { hi[2] },
            ];
            let p = self.apply(corner);
            for k in 0..3 {
                out_lo[k] = out_lo[k].min(p[k]);
                out_hi[k] = out_hi[k].max(p[k]);
            }
        }
        Some((out_lo, out_hi))
    }
}

/// The box around two boxes (`None` for none).
pub fn union(a: BoundingBox, b: BoundingBox) -> BoundingBox {
    match (a, b) {
        (None, x) | (x, None) => x,
        (Some((alo, ahi)), Some((blo, bhi))) => Some((
            std::array::from_fn(|k| alo[k].min(blo[k])),
            std::array::from_fn(|k| ahi[k].max(bhi[k])),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A rotation by `deg` about z, then a move by `t`, column-major.
    fn rz_then_move(deg: f64, t: [f64; 3]) -> [f32; 16] {
        let (s, c) = deg.to_radians().sin_cos();
        [
            c as f32,
            s as f32,
            0.0,
            0.0, //
            -s as f32,
            c as f32,
            0.0,
            0.0, //
            0.0,
            0.0,
            1.0,
            0.0, //
            t[0] as f32,
            t[1] as f32,
            t[2] as f32,
            1.0,
        ]
    }

    fn close(a: [f64; 3], b: [f64; 3]) -> bool {
        (0..3).all(|k| (a[k] - b[k]).abs() < 1e-5)
    }

    #[test]
    fn column_major_puts_the_translation_in_the_last_column() {
        let p = Placement::from_column_major(&rz_then_move(90.0, [1.0, 2.0, 3.0])).unwrap();
        // (1, 0, 0) turns to (0, 1, 0), then moves by (1, 2, 3).
        assert!(close(p.apply([1.0, 0.0, 0.0]), [1.0, 3.0, 3.0]));
        assert!(close(p.apply([0.0, 0.0, 0.0]), [1.0, 2.0, 3.0]));
        assert!(!p.is_identity());
    }

    #[test]
    fn identity_is_exact_and_folds_to_the_same_matrices() {
        let a: Vec<f32> = (0..16)
            .map(|i| if i % 5 == 0 { 1.0 } else { 0.0 })
            .collect();
        let p = Placement::from_column_major(&a).unwrap();
        assert!(p.is_identity());
        // Folding the identity into a view's matrices changes no bit, so
        // a model placed at identity draws exactly as one shown alone.
        let view: Mat4 =
            std::array::from_fn(|r| std::array::from_fn(|c| 0.1 + (r * 4 + c) as f64 * 0.37 - 1.3));
        assert_eq!(p.after(&view), view);
        let n: [[f64; 3]; 3] =
            std::array::from_fn(|r| std::array::from_fn(|c| 0.3 * r as f64 - 0.7 * c as f64));
        assert_eq!(p.normals_after(&n), n);
    }

    #[test]
    fn refuses_what_is_not_rigid() {
        assert_eq!(
            Placement::from_column_major(&[1.0; 15]),
            Err(PlacementError::Length(15))
        );
        let mut m = rz_then_move(30.0, [0.0; 3]);
        m[0] = f32::NAN;
        assert_eq!(
            Placement::from_column_major(&m),
            Err(PlacementError::NotFinite)
        );
        let mut scaled = rz_then_move(30.0, [0.0; 3]);
        for x in &mut scaled[0..3] {
            *x *= 1.1;
        }
        assert_eq!(
            Placement::from_column_major(&scaled),
            Err(PlacementError::NotRigid)
        );
        // mirror([1, 0, 0]): orthonormal, but it would flip the winding.
        let mut mirrored = rz_then_move(0.0, [0.0; 3]);
        mirrored[0] = -1.0;
        assert_eq!(
            Placement::from_column_major(&mirrored),
            Err(PlacementError::NotRigid)
        );
        let mut projective = rz_then_move(0.0, [0.0; 3]);
        projective[3] = 0.5;
        assert_eq!(
            Placement::from_column_major(&projective),
            Err(PlacementError::NotAffine)
        );
        // f32 rounding of a composed rotation is within the tolerance.
        assert!(Placement::from_column_major(&rz_then_move(33.3, [5.0, 0.0, 0.0])).is_ok());
    }

    #[test]
    fn normals_turn_with_the_model() {
        let p = Placement::from_column_major(&rz_then_move(90.0, [7.0, 0.0, 0.0])).unwrap();
        let identity: [[f64; 3]; 3] =
            std::array::from_fn(|r| std::array::from_fn(|c| if r == c { 1.0 } else { 0.0 }));
        let n = p.normals_after(&identity);
        // A +x normal of the model faces +y once placed; the move does not
        // affect normals.
        let x = [n[0][0], n[1][0], n[2][0]];
        assert!(close(x, [0.0, 1.0, 0.0]));
    }

    #[test]
    fn bounds_hold_the_moved_box() {
        let p = Placement::from_column_major(&rz_then_move(45.0, [10.0, 0.0, 0.0])).unwrap();
        let (lo, hi) = p
            .bounds(Some(([-1.0, -1.0, 0.0], [1.0, 1.0, 2.0])))
            .unwrap();
        let r = 2f64.sqrt();
        assert!(close(lo, [10.0 - r, -r, 0.0]));
        assert!(close(hi, [10.0 + r, r, 2.0]));
        assert_eq!(p.bounds(None), None);
        assert_eq!(
            union(
                Some(([0.0; 3], [1.0; 3])),
                Some(([-1.0, 2.0, 0.5], [0.5, 3.0, 0.6]))
            ),
            Some(([-1.0, 0.0, 0.0], [1.0, 3.0, 1.0]))
        );
        assert_eq!(union(None, None), None);
    }
}
