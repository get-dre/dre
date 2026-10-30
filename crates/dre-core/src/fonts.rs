//! Pinned font packages from the plugin registry, cached separately from executables.

use std::path::Path;

use dre_protocol::msg::FontRequest;
use serde::{Deserialize, Serialize};

use crate::context::Context;
use crate::manager::{self, Artifact};

#[derive(Deserialize)]
struct FontIndex {
    #[serde(default)]
    fonts: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
    name: String,
    versions: Vec<Version>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Version {
    version: String,
    regular: Artifact,
    bold: Artifact,
    license: Artifact,
}

impl Version {
    fn files(&self) -> [(&str, &Artifact); 3] {
        [
            ("regular.otf", &self.regular),
            ("bold.otf", &self.bold),
            ("OFL.txt", &self.license),
        ]
    }

    fn cached(&self, dir: &Path) -> bool {
        self.files().iter().all(|(name, artifact)| {
            valid_checksum(&artifact.sha256)
                && crate::checksum::sha256_file(&dir.join(name))
                    .is_ok_and(|sum| sum.eq_ignore_ascii_case(&artifact.sha256))
        })
    }
}

fn valid_checksum(sum: &str) -> bool {
    sum.len() == 64 && sum.bytes().all(|c| c.is_ascii_hexdigit())
}

/// Resolve an exact font version, using the verified cache before contacting the registry.
/// The registry's optional `fonts` sibling leaves older plugin index readers unchanged.
pub fn resolve(ctx: &Context, request: &FontRequest) -> Result<Vec<String>, String> {
    // A plugin controls this request: never let it choose a path outside the cache.
    if request.name.is_empty()
        || !request
            .name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
        || semver::Version::parse(&request.version).is_err()
    {
        return Err("invalid font package name or version".into());
    }
    let dir = std::path::absolute(
        ctx.env()
            .dre_home()
            .join("fonts")
            .join(&request.name)
            .join(&request.version),
    )
    .map_err(|e| format!("can't locate font cache: {e}"))?;
    std::fs::create_dir_all(&dir).map_err(|e| format!("can't create font cache: {e}"))?;
    // Hold one OS lock through validation and publication, across threads and processes.
    // Concurrent replacements can fail on Windows even when each individual write is atomic.
    // Keep this lock file in place: deleting it would let waiters lock different files.
    let cache_lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("cache.lock"))
        .map_err(|e| format!("can't open font cache lock: {e}"))?;
    cache_lock
        .lock()
        .map_err(|e| format!("can't lock font cache: {e}"))?;
    let manifest = dir.join("font.json");
    let cached: Option<Version> = std::fs::read(&manifest)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    if !cached
        .as_ref()
        .is_some_and(|v| v.version == request.version && v.cached(&dir))
    {
        let bytes = manager::fetch(ctx, &manager::registry_url(ctx.env()))?;
        let index: FontIndex =
            serde_json::from_slice(&bytes).map_err(|e| format!("the font registry isn't valid: {e}"))?;
        let version = index
            .fonts
            .into_iter()
            .find(|p| p.name == request.name)
            .and_then(|p| p.versions.into_iter().find(|v| v.version == request.version))
            .ok_or_else(|| format!("font {} {} isn't in the registry", request.name, request.version))?;
        for (name, artifact) in version.files() {
            if !valid_checksum(&artifact.sha256) {
                return Err(format!("font {} {name} needs a SHA-256 checksum", request.name));
            }
            let bytes = manager::fetch(ctx, &artifact.url)?;
            if !crate::checksum::sha256_hex(&bytes).eq_ignore_ascii_case(&artifact.sha256) {
                return Err(format!(
                    "font {} {name} failed its SHA-256 checksum",
                    request.name
                ));
            }
            atomic_write(&dir.join(name), &bytes)?;
        }
        let bytes = serde_json::to_vec(&version).map_err(|e| format!("can't record cached font: {e}"))?;
        atomic_write(&manifest, &bytes)?;
    }
    Ok(["regular.otf", "bold.otf"]
        .iter()
        .map(|name| dir.join(name).to_string_lossy().into_owned())
        .collect())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let parent = path.parent().ok_or("font cache file has no parent")?;
    let mut temp =
        tempfile::NamedTempFile::new_in(parent).map_err(|e| format!("can't create cached font: {e}"))?;
    temp.write_all(bytes)
        .map_err(|e| format!("can't write cached font: {e}"))?;
    temp.persist(path)
        .map_err(|e| format!("can't install cached font: {e}"))?;
    Ok(())
}
