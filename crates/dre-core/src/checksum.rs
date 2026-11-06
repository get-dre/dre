//! SHA-256 checksums as lowercase hex, the one format DRE writes them in (manifests, releases,
//! held files, plugin downloads). [`hex`] also writes SQL hex literals (`dialect.rs`).

use std::path::Path;

use sha2::{Digest, Sha256};

/// `bytes` as lowercase hex, two digits each.
#[must_use]
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[must_use]
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Streamed, so a big file never sits in memory whole.
pub fn sha256_file(p: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0; 64 * 1024];
    loop {
        match f.read(&mut buf)? {
            0 => return Ok(hex(&h.finalize())),
            n => h.update(&buf[..n]),
        }
    }
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

    #[test]
    fn a_file_hashes_the_same_streamed_as_in_one_piece() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("big.bin");
        let bytes: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&p, &bytes).unwrap();
        assert_eq!(sha256_file(&p).unwrap(), sha256_hex(&bytes));
    }
}
