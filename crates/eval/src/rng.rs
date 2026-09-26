//! `rands()`'s random numbers, bit for bit as OpenSCAD draws them.
//!
//! OpenSCAD uses `std::mt19937` and `std::uniform_real_distribution<double>`
//! (builtin_functions.cc, `builtin_rands`). Both the Mersenne Twister and
//! `generate_canonical` are specified exactly by the C++ standard (two
//! 32-bit draws per double, the low word first), so seeded sequences are
//! reproducible and the regression goldens depend on them. The seed is the
//! 32-bit truncation of Python's float hash of the seed argument.

/// `std::mt19937`.
#[derive(Debug, Clone)]
pub struct Mt19937 {
    mt: [u32; 624],
    index: usize,
}

impl Mt19937 {
    pub fn new(seed: u32) -> Self {
        let mut mt = [0u32; 624];
        mt[0] = seed;
        for i in 1..624 {
            mt[i] = 1_812_433_253u32
                .wrapping_mul(mt[i - 1] ^ (mt[i - 1] >> 30))
                .wrapping_add(i as u32);
        }
        Mt19937 { mt, index: 624 }
    }

    pub fn seed(&mut self, seed: u32) {
        *self = Self::new(seed);
    }

    fn twist(&mut self) {
        const UPPER: u32 = 0x8000_0000;
        const LOWER: u32 = 0x7fff_ffff;
        for i in 0..624 {
            let y = (self.mt[i] & UPPER) | (self.mt[(i + 1) % 624] & LOWER);
            let mut v = self.mt[(i + 397) % 624] ^ (y >> 1);
            if y & 1 != 0 {
                v ^= 0x9908_b0df;
            }
            self.mt[i] = v;
        }
        self.index = 0;
    }

    pub fn next_u32(&mut self) -> u32 {
        if self.index >= 624 {
            self.twist();
        }
        let mut y = self.mt[self.index];
        self.index += 1;
        y ^= y >> 11;
        y ^= (y << 7) & 0x9d2c_5680;
        y ^= (y << 15) & 0xefc6_0000;
        y ^= y >> 18;
        y
    }

    /// `std::generate_canonical<double, 53>`: two draws, low word first.
    fn canonical(&mut self) -> f64 {
        const R: f64 = 4_294_967_296.0;
        let lo = f64::from(self.next_u32());
        let hi = f64::from(self.next_u32());
        let r = (lo + hi * R) / (R * R);
        if r >= 1.0 {
            1.0 - f64::EPSILON / 2.0
        } else {
            r
        }
    }

    /// `std::uniform_real_distribution<double>(min, max)(*this)`: libc++
    /// computes `(b - a) * generate_canonical(g) + a`, which the arm64
    /// nightly fuses (see `fma`). (`canonical`'s own multiply-add,
    /// `hi * 2^32 + lo`, is exact either way.)
    pub fn uniform(&mut self, min: f64, max: f64) -> f64 {
        crate::fma::mul_add(self.canonical(), max - min, min)
    }
}

/// `hash_floating_point` (geometry/linalg.cc): CPython's float hash with
/// 31-bit modulus, used to turn a `rands()` seed into a 32-bit seed.
pub fn hash_float(v: f64) -> i32 {
    const BITS: u32 = 31;
    const MODULUS: u32 = (1 << BITS) - 1;
    const INF: u32 = 314_159;
    if !v.is_finite() {
        if v.is_infinite() {
            return if v > 0.0 {
                INF as i32
            } else {
                (INF as i32).wrapping_neg()
            };
        }
        return 0;
    }
    let (mut m, mut e) = frexp(v);
    let mut sign: i32 = 1;
    if m < 0.0 {
        sign = -1;
        m = -m;
    }
    let mut x: u32 = 0;
    while m != 0.0 {
        x = ((x << 28) & MODULUS) | (x >> (BITS - 28));
        m *= 268_435_456.0;
        e -= 28;
        let y = m as u32;
        m -= f64::from(y);
        x = x.wrapping_add(y);
        if x >= MODULUS {
            x -= MODULUS;
        }
    }
    let e = if e >= 0 {
        e % BITS as i32
    } else {
        BITS as i32 - 1 - ((-1 - e) % BITS as i32)
    } as u32;
    x = ((x << e) & MODULUS) | (x >> (BITS - e));
    x.wrapping_mul(sign as u32) as i32
}

/// C's `frexp`: `v = m * 2^e` with `0.5 <= |m| < 1`.
fn frexp(v: f64) -> (f64, i32) {
    if v == 0.0 || !v.is_finite() {
        return (v, 0);
    }
    let bits = v.to_bits();
    let exp = ((bits >> 52) & 0x7ff) as i32;
    if exp == 0 {
        // Subnormal: scale up first.
        let (m, e) = frexp(v * 2f64.powi(64));
        return (m, e - 64);
    }
    let e = exp - 1022;
    let m = f64::from_bits((bits & !(0x7ff << 52)) | (1022u64 << 52));
    (m, e)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mt19937_matches_the_standard() {
        // The C++ standard requires the 10000th value of a default-seeded
        // mt19937 to be 4123659995.
        let mut r = Mt19937::new(5489);
        let mut v = 0;
        for _ in 0..10000 {
            v = r.next_u32();
        }
        assert_eq!(v, 4_123_659_995);
    }

    #[test]
    fn python_float_hash() {
        assert_eq!(hash_float(0.0), 0);
        assert_eq!(hash_float(1.0), 1);
        assert_eq!(hash_float(-32.0), -32);
        assert_eq!(hash_float(0.5), 1 << 30);
        assert_eq!(frexp(8.0), (0.5, 4));
    }
}
