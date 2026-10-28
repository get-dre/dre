//! SHA-256 checksums as lowercase hex, the one format DRE writes them in (manifests, releases,
//! held files, plugin downloads). [`hex`] also writes SQL hex literals (`dialect.rs`).

use std::path::Path;

use sha2::{Digest, Sha256};

/// `bytes` as lowercase hex, two digits each.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

pub fn sha256_file(p: &Path) -> std::io::Result<String> {
    Ok(sha256_hex(&std::fs::read(p)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_is_lowercase_and_zero_padded() {
        assert_eq!(hex(&[0x00, 0x0a, 0xff]), "000aff");
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }
}
