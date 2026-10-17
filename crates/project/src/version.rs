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
                (Some(a), Some(b)) => a.cmp(b),
            })
    }
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
        for bad in ["main", "v", "1..2", "1.2.3.4", "release-1", "1.0-"] {
            assert_eq!(Version::parse(bad), None, "{bad}");
        }
    }
}
