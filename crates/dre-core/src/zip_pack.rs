//! `zip:` on a destination entry: the output's files, plus every `attach:`ed output's, packed
//! flat into one zip during the run, optionally with a password (AES-256 by default, ZipCrypto
//! on request). The zip is built in the run folder, so a held entry records it and its SHA-256,
//! and `dre release` sends exactly those bytes.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// How a zip's files are encrypted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Encryption {
    /// WinZip AES-256: strong; opens in 7-Zip, WinRAR, Keka, The Unarchiver.
    #[default]
    Aes256,
    /// The legacy zip password: weak, but opens in Windows Explorer and macOS Archive Utility.
    Zipcrypto,
}

impl Encryption {
    pub const NAMES: &[&str] = &["aes256", "zipcrypto"];
}

/// A destination entry's `zip:`, as written: the password may hold Jinja.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ZipSpec {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    pub encryption: Encryption,
}

/// The zip's file name: the last part of the destination's `path`, else `<output>.zip`.
/// An error when the path ends in another extension.
pub fn zip_name(path: Option<&str>, output: &str) -> Result<String, String> {
    let name = match path {
        Some(p) => p.rsplit(['/', '\\']).next().unwrap_or(p).to_string(),
        None => format!("{output}.zip"),
    };
    if name.to_ascii_lowercase().ends_with(".zip") {
        Ok(name)
    } else {
        Err(format!(
            "a zipping destination's path must end in `.zip`, but it names `{name}`"
        ))
    }
}

/// The flat names the files take in the zip; two files with one name are an error.
pub fn entry_names(files: &[PathBuf]) -> Result<Vec<String>, String> {
    let mut seen = BTreeSet::new();
    let mut names = Vec::with_capacity(files.len());
    for f in files {
        let name = f
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .ok_or_else(|| format!("{} has no file name", f.display()))?;
        if !seen.insert(name.clone()) {
            return Err(format!(
                "two files named `{name}` would go into one zip (it's flat); give the outputs different `name:`s"
            ));
        }
        names.push(name);
    }
    Ok(names)
}

/// Pack `files` flat into the zip `to`. Entries carry a fixed date, so the same files make
/// the same zip (without a password; encryption salts are random).
pub fn build(
    to: &Path,
    files: &[PathBuf],
    password: Option<&str>,
    encryption: Encryption,
) -> Result<(), String> {
    use zip::unstable::write::FileOptionsExt;
    use zip::write::SimpleFileOptions;

    if password == Some("") {
        return Err(
            "the zip's `password` is empty; an empty password would leave the zip unencrypted".into(),
        );
    }
    let names = entry_names(files)?;
    if let Some(dir) = to.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    }
    let out = File::create(to).map_err(|e| format!("can't write {}: {e}", to.display()))?;
    let mut w = zip::ZipWriter::new(BufWriter::new(out));
    let base = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .last_modified_time(zip::DateTime::default())
        .unix_permissions(0o644);
    for (f, name) in files.iter().zip(&names) {
        let opts = match (password, encryption) {
            (None, _) => base,
            (Some(pw), Encryption::Aes256) => base.with_aes_encryption(zip::AesMode::Aes256, pw),
            (Some(pw), Encryption::Zipcrypto) => base
                .with_deprecated_encryption(pw.as_bytes())
                .map_err(|e| format!("can't encrypt the zip: {e}"))?,
        };
        w.start_file(name.as_str(), opts)
            .map_err(|e| format!("can't add {name} to the zip: {e}"))?;
        let bytes = std::fs::read(f).map_err(|e| format!("can't read {}: {e}", f.display()))?;
        w.write_all(&bytes)
            .map_err(|e| format!("can't add {name} to the zip: {e}"))?;
    }
    w.finish()
        .map_err(|e| format!("can't write {}: {e}", to.display()))?
        .flush()
        .map_err(|e| format!("can't write {}: {e}", to.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn read(zip: &Path, name: &str, pw: Option<&str>) -> Result<String, String> {
        let mut a = zip::ZipArchive::new(File::open(zip).unwrap()).map_err(|e| e.to_string())?;
        let mut s = String::new();
        match pw {
            Some(pw) => a.by_name_decrypt(name, pw.as_bytes()),
            None => a.by_name(name),
        }
        .map_err(|e| e.to_string())?
        .read_to_string(&mut s)
        .map_err(|e| e.to_string())?;
        Ok(s)
    }

    #[test]
    fn plain_aes_and_zipcrypto_zips_open_with_their_password_only() {
        let d = tempfile::tempdir().unwrap();
        let a = d.path().join("a/summary.csv");
        let b = d.path().join("b/detail.csv");
        std::fs::create_dir_all(a.parent().unwrap()).unwrap();
        std::fs::create_dir_all(b.parent().unwrap()).unwrap();
        std::fs::write(&a, "x\n1\n").unwrap();
        std::fs::write(&b, "y\n2\n").unwrap();
        let files = vec![a.clone(), b];
        let plain = d.path().join("plain.zip");
        build(&plain, &files, None, Encryption::Aes256).unwrap();
        assert_eq!(read(&plain, "summary.csv", None).unwrap(), "x\n1\n");
        // The same files make the same zip.
        let again = d.path().join("again.zip");
        build(&again, &files, None, Encryption::Aes256).unwrap();
        assert_eq!(std::fs::read(&plain).unwrap(), std::fs::read(&again).unwrap());
        for enc in [Encryption::Aes256, Encryption::Zipcrypto] {
            let z = d.path().join(format!("{enc:?}.zip"));
            build(&z, &files, Some("Pack-pw-1"), enc).unwrap();
            assert_eq!(read(&z, "detail.csv", Some("Pack-pw-1")).unwrap(), "y\n2\n");
            assert!(read(&z, "detail.csv", Some("wrong")).is_err(), "{enc:?}");
            assert!(read(&z, "detail.csv", None).is_err(), "{enc:?}");
        }
        assert!(build(&d.path().join("e.zip"), &files, Some(""), Encryption::Aes256).is_err());
        let clash = vec![a, d.path().join("summary.csv")];
        let e = entry_names(&clash).unwrap_err();
        assert!(e.contains("two files named `summary.csv`"), "{e}");
    }

    #[test]
    fn names() {
        assert_eq!(
            zip_name(Some("/out/pack-2026.zip"), "detail").unwrap(),
            "pack-2026.zip"
        );
        assert_eq!(zip_name(None, "detail").unwrap(), "detail.zip");
        assert!(zip_name(Some("out/pack.csv"), "detail").is_err());
    }
}
