//! Trigonometry in degrees, exact at the angles people type.
//!
//! A port of `utils/degree_trig.cc`: `sin(30)` must be exactly `0.5` and
//! `cos(90)` exactly `0`, otherwise echoed values and geometry drift from
//! OpenSCAD's (`0.5` vs `0.49999999999999994`). The functions reduce the
//! angle to the first octant and special-case 30, 45, 60 and 90 degrees.

// The literals are copied from degree_trig.h so the doubles are identical.
const SQRT1_2: f64 = std::f64::consts::FRAC_1_SQRT_2;
#[allow(clippy::excessive_precision)]
const SQRT3_4: f64 = 0.86602540378443859659;
#[allow(clippy::excessive_precision)]
const SQRT1_3: f64 = 0.57735026918962573106;
#[allow(clippy::excessive_precision)]
const SQRT3: f64 = 1.73205080756887719318;
#[allow(clippy::excessive_precision)]
const RAD2DEG: f64 = 57.2957795130823208767;
#[allow(clippy::excessive_precision)]
const DEG2RAD: f64 = 0.017453292519943295769;
/// Beyond this magnitude a double cannot hold a meaningful angle.
const HUGE: f64 = (1i64 << 26) as f64 * 360.0 * (1i64 << 26) as f64;

fn reduce(x: f64, period: f64) -> Option<f64> {
    if (0.0..period).contains(&x) {
        Some(x)
    } else if x < HUGE && x > -HUGE {
        Some(x - period * (x / period).floor())
    } else {
        None
    }
}

pub fn sin_degrees(x: f64) -> f64 {
    let Some(mut x) = reduce(x, 360.0) else {
        return f64::NAN;
    };
    let oppose = x >= 180.0;
    if oppose {
        x -= 180.0;
    }
    if x > 90.0 {
        x = 180.0 - x;
    }
    let r = if x < 45.0 {
        if x == 30.0 { 0.5 } else { (x * DEG2RAD).sin() }
    } else if x == 45.0 {
        SQRT1_2
    } else if x == 60.0 {
        SQRT3_4
    } else {
        ((90.0 - x) * DEG2RAD).cos()
    };
    if oppose { -r } else { r }
}

pub fn cos_degrees(x: f64) -> f64 {
    let Some(mut x) = reduce(x, 360.0) else {
        return f64::NAN;
    };
    let mut oppose = x >= 180.0;
    if oppose {
        x -= 180.0;
    }
    if x > 90.0 {
        x = 180.0 - x;
        oppose = !oppose;
    }
    let r = if x > 45.0 {
        if x == 60.0 {
            0.5
        } else {
            ((90.0 - x) * DEG2RAD).sin()
        }
    } else if x == 45.0 {
        SQRT1_2
    } else if x == 30.0 {
        SQRT3_4
    } else {
        (x * DEG2RAD).cos()
    };
    if oppose { -r } else { r }
}

pub fn tan_degrees(x: f64) -> f64 {
    // `const int cycles = floor(x / 180.0)`: a saturating conversion.
    let cycles = (x / 180.0).floor() as i32;
    let mut x = if (0.0..180.0).contains(&x) {
        x
    } else if x < HUGE && x > -HUGE {
        x - 180.0 * f64::from(cycles)
    } else {
        return f64::NAN;
    };
    let oppose = x > 90.0;
    if oppose {
        x = 180.0 - x;
    }
    let r = if x == 0.0 {
        if cycles % 2 == 0 { 0.0 } else { -0.0 }
    } else if x == 30.0 {
        SQRT1_3
    } else if x == 45.0 {
        1.0
    } else if x == 60.0 {
        SQRT3
    } else if x == 90.0 {
        if cycles % 2 == 0 {
            f64::INFINITY
        } else {
            f64::NEG_INFINITY
        }
    } else {
        (x * DEG2RAD).tan()
    };
    if oppose { -r } else { r }
}

/// `round()` as C does it: half away from zero.
fn round(x: f64) -> f64 {
    x.round()
}

pub fn asin_degrees(x: f64) -> f64 {
    let degs = x.asin() * RAD2DEG;
    let whole = round(degs);
    if sin_degrees(whole) == x { whole } else { degs }
}

pub fn acos_degrees(x: f64) -> f64 {
    let degs = x.acos() * RAD2DEG;
    let whole = round(degs);
    if cos_degrees(whole) == x { whole } else { degs }
}

pub fn atan_degrees(x: f64) -> f64 {
    let degs = x.atan() * RAD2DEG;
    let whole = round(degs);
    if tan_degrees(whole) == x { whole } else { degs }
}

pub fn atan2_degrees(y: f64, x: f64) -> f64 {
    let degs = y.atan2(x) * RAD2DEG;
    let whole = round(degs);
    if (degs - whole).abs() < 3.0e-14 {
        whole
    } else {
        degs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_special_angles() {
        assert_eq!(sin_degrees(30.0), 0.5);
        assert_eq!(sin_degrees(180.0), 0.0);
        assert_eq!(sin_degrees(-90.0), -1.0);
        assert_eq!(cos_degrees(90.0), 0.0);
        assert_eq!(cos_degrees(60.0), 0.5);
        assert_eq!(cos_degrees(120.0), -0.5);
        assert_eq!(tan_degrees(45.0), 1.0);
        assert_eq!(tan_degrees(90.0), f64::INFINITY);
        assert_eq!(tan_degrees(-90.0), f64::NEG_INFINITY);
        assert_eq!(asin_degrees(0.5), 30.0);
        assert_eq!(acos_degrees(0.5), 60.0);
        assert_eq!(atan_degrees(1.0), 45.0);
        assert_eq!(atan2_degrees(1.0, 1.0), 45.0);
        assert!(sin_degrees(f64::INFINITY).is_nan());
    }
}
