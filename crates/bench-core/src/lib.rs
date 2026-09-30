//! What `neoscad bench` (the community benchmark, docs/community-bench.md)
//! and `conformance bench` (the repository's own series) share:
//!
//! - [`timing`]: how a command is spawned, polled and timed best-of-N, and
//!   [`timing::METHOD_VERSION`], the version of that method;
//! - [`kit`]: the bench kit, the release asset holding the models;
//! - [`result`]: the versioned result schema a community run writes
//!   (`bench/result.schema.json` describes the same thing for the
//!   benchmarks repository's validation);
//! - [`official`]: whether the running binary is an official release;
//! - [`machine`]: the hardware and OS facts a result records;
//! - [`submit`]: the GitHub issue a result is submitted as.
//!
//! This is a tool crate like crates/conformance, not a library crate: it
//! reads files, spawns processes and reads the clock, and nothing built
//! for WASM depends on it.

pub mod kit;
pub mod machine;
pub mod official;
pub mod result;
pub mod submit;
pub mod timing;

use sha2::{Digest, Sha256};

/// Lower-case hex SHA-256 of `data`.
pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Lower-case hex SHA-256 of a file, read in blocks (the executable is
/// about 19 MB).
pub fn sha256_file(path: &std::path::Path) -> Result<String, String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = f
            .read(&mut buf)
            .map_err(|e| format!("{}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// Now as an ISO 8601 UTC timestamp to the second ("2026-09-30T12:34:56Z").
pub fn utc_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    utc_from_unix(secs)
}

/// `secs` since the Unix epoch as ISO 8601 UTC (Howard Hinnant's
/// civil-from-days, valid for any date after 1970).
pub fn utc_from_unix(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn utc_dates() {
        assert_eq!(super::utc_from_unix(0), "1970-01-01T00:00:00Z");
        assert_eq!(super::utc_from_unix(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(super::utc_from_unix(1_790_000_000), "2026-09-21T14:13:20Z");
    }

    #[test]
    fn sha256_known_vector() {
        assert_eq!(
            super::sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
