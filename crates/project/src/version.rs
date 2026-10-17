//! Git tags as versions, for minimal version selection: `v1.2.3`,
//! `1.2`, `v2.0.716-rc1`. A tag that is not of this form is no version,
//! and two requirements of it agree only when they name one commit.

use std::cmp::Ordering;

/// A tag read as a semantic version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    pub nums: [u64; 3],
    /// After `-`; a pre-release sorts before its release.
    pub pre: Option<String>,
}

impl Version {
    /// `v`/`V` prefix optional, one to three numeric components, an
    /// optional `-pre` and `+build` (ignored).
    pub fn parse(tag: &str) -> Option<Version> {
        let t = tag.strip_prefix(['v', 'V']).unwrap_or(tag);
        let t = t.split('+').next()?;
        let (core, pre) = match t.split_once('-') {
            Some((c, p)) if !p.is_empty() => (c, Some(p.to_string())),
            Some(_) => return None,
            None => (t, None),
        };
        let mut nums = [0u64; 3];
        let parts: Vec<&str> = core.split('.').collect();
        if parts.is_empty() || parts.len() > 3 {
            return None;
        }
        for (i, p) in parts.iter().enumerate() {
            if p.is_empty() || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            nums[i] = p.parse().ok()?;
        }
        Some(Version { nums, pre })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.nums
            .cmp(&other.nums)
            .then_with(|| match (&self.pre, &other.pre) {
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(a), Some(b)) => pre_cmp(a, b),
            })
    }
}

/// Pre-releases compared as semver does: identifier by identifier (split
/// at `.`), numeric ones as numbers and below alphanumeric ones, the
/// others by their bytes, and a shorter list first when one is a prefix
/// of the other. A tag's `rc1`, `rc2`, `rc10` are one identifier each, so
/// within an identifier a run of digits is compared as a number too
/// (`rc10` after `rc2`, as a person reading the tags expects; strict
/// semver would put it first).
fn pre_cmp(a: &str, b: &str) -> Ordering {
    let mut x = a.split('.');
    let mut y = b.split('.');
    loop {
        match (x.next(), y.next()) {
            // Equal by value (`01` and `1`) is decided by the text, so that
            // the order agrees with equality.
            (None, None) => return a.cmp(b),
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(p), Some(q)) => {
                let numeric = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
                let o = match (numeric(p), numeric(q)) {
                    (true, true) => digits_cmp(p, q),
                    (true, false) => Ordering::Less,
                    (false, true) => Ordering::Greater,
                    (false, false) => natural_cmp(p, q),
                };
                if o != Ordering::Equal {
                    return o;
                }
            }
        }
    }
}

/// Two runs of ASCII digits by value, whatever their length.
fn digits_cmp(p: &str, q: &str) -> Ordering {
    let p = p.trim_start_matches('0');
    let q = q.trim_start_matches('0');
    p.len().cmp(&q.len()).then_with(|| p.cmp(q))
}

/// Text with its digit runs compared as numbers: `rc2` < `rc10`.
fn natural_cmp(p: &str, q: &str) -> Ordering {
    let (pb, qb) = (p.as_bytes(), q.as_bytes());
    let (mut i, mut j) = (0, 0);
    while i < pb.len() && j < qb.len() {
        if pb[i].is_ascii_digit() && qb[j].is_ascii_digit() {
            let (si, sj) = (i, j);
            while i < pb.len() && pb[i].is_ascii_digit() {
                i += 1;
            }
            while j < qb.len() && qb[j].is_ascii_digit() {
                j += 1;
            }
            let o = digits_cmp(&p[si..i], &q[sj..j]);
            if o != Ordering::Equal {
                return o;
            }
        } else {
            let o = pb[i].cmp(&qb[j]);
            if o != Ordering::Equal {
                return o;
            }
            i += 1;
            j += 1;
        }
    }
    (pb.len() - i).cmp(&(qb.len() - j)).then_with(|| p.cmp(q))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn orders_tags() {
        let v = |s| Version::parse(s).unwrap();
        assert!(v("v2.0.716") > v("v2.0.99"));
        assert!(v("1.2") < v("1.2.1"));
        assert!(v("v1.0.0-rc1") < v("v1.0.0"));
        assert_eq!(v("V3"), v("3.0.0"));
        assert_eq!(v("1.0.0+build5"), v("1.0.0"));
        // Pre-releases by identifier, numbers as numbers.
        assert!(v("v1.0.0-rc10") > v("v1.0.0-rc2"));
        assert!(v("1.0.0-rc.10") > v("1.0.0-rc.2"));
        assert!(v("1.0.0-alpha") < v("1.0.0-alpha.1"));
        assert!(v("1.0.0-alpha.1") < v("1.0.0-alpha.beta"));
        assert!(v("1.0.0-1") < v("1.0.0-alpha"));
        assert!(v("1.0.0-beta.11") > v("1.0.0-beta.2"));
        assert!(v("1.0.0-rc.1") < v("1.0.0"));
        for bad in ["main", "v", "1..2", "1.2.3.4", "release-1", "1.0-"] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
    }
}
