//! Multiply-adds the way OpenSCAD's own build rounds them on this platform.
//!
//! OpenSCAD writes its vector maths as plain C++ (`r_e += a * b` in
//! `Value.cc`'s `multmatvec`, `multvecmat` and `multvecvec`, `sum += x * x`
//! in `norm`, `a * b - c * d` in `cross`). Its arm64 build rounds these as
//! fused multiply-adds and its x86_64 build does not: `[1, 0.1] *
//! [-0.010000000000000002, 0.1]` echoes `-8.32667e-19` from the arm64
//! nightly and `0` from the same binary's x86_64 slice. (The mechanism is
//! presumably clang's default `-ffp-contract=on`, which only fuses where
//! the target has an FMA instruction, as every arm64 CPU does and baseline
//! x86-64 does not; the build flags themselves are not checked.) Those
//! last bits decide tie-breaks further on, such as polyhedron face
//! diagonals and arc point counts in BOSL2, so neoscad follows the
//! platform it runs on: fused on `aarch64`, plain elsewhere. Plain on
//! wasm32 also avoids `mul_add`'s slow software fallback there.
//!
//! Use these helpers wherever OpenSCAD's source has a multiply feeding an
//! add in one expression, so the policy lives in one place.

/// `a * b + c`, fused on `aarch64` (one rounding) and plain elsewhere (two).
#[inline]
pub fn mul_add(a: f64, b: f64, c: f64) -> f64 {
    #[cfg(target_arch = "aarch64")]
    {
        a.mul_add(b, c)
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        a * b + c
    }
}

/// `a * b - c * d` as clang contracts it: LLVM fuses the first product
/// and rounds the second (`fma(a, b, -(c * d))`). Checked against the
/// arm64 nightly through `cross()`.
#[inline]
pub fn mul_sub_mul(a: f64, b: f64, c: f64, d: f64) -> f64 {
    mul_add(a, b, -(c * d))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn follows_the_platform() {
        // 0.1 * 0.1 rounds up; fused, the exact product survives the
        // cancellation against -(1 * 0.010000000000000002).
        let r = mul_add(0.1, 0.1, -0.010000000000000002);
        if cfg!(target_arch = "aarch64") {
            assert_eq!(r, -8.326672684688674e-19);
        } else {
            assert_eq!(r, 0.0);
        }
    }
}
