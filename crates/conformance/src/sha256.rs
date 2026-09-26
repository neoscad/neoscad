//! SHA-256 of the manifest bytes, which pins a progress snapshot to the exact
//! test list it was run against.

use sha2::{Digest, Sha256};

/// Lower-case hex digest of `data`.
pub fn hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn known_vectors() {
        assert_eq!(
            super::hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            super::hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
