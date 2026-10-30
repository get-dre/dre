//! Pinned font packages from the plugin registry, cached separately from executables.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use dre_protocol::msg::{FontError, FontRequest};
use serde::{Deserialize, Serialize};

use crate::context::{Context, Env};
use crate::manager::{self, Artifact};

#[derive(Deserialize)]
struct FontIndex {
    #[serde(default)]
    fonts: Vec<Package>,
}

#[derive(Deserialize)]
struct Package {
    name: String,
    #[serde(default)]
    scripts: Vec<String>,
    versions: Vec<Version>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Version {
    version: String,
    regular: Artifact,
    bold: Artifact,
    license: Artifact,
    /// The package's scripts, recorded in the cache for `dre fonts ls`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    scripts: Vec<String>,
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

/// One run's font answers, shared by its plugin processes: each package is resolved once, so an
/// offline run tries the registry once per font rather than once per row, and only the first
/// answer to a failed font is new to the person (later ones are `repeated`).
#[derive(Default)]
pub struct RunFonts {
    answers: Mutex<HashMap<(String, String), Arc<Answer>>>,
}

/// One package's answer for a run, set by its first request.
type Answer = OnceLock<Result<Vec<String>, String>>;

impl RunFonts {
    /// [`resolve`], once per package for the run; requests for the same package wait for the
    /// first.
    ///
    /// # Panics
    ///
    /// Panics if the answers are poisoned (a thread panicked while adding one).
    pub fn resolve(&self, ctx: &Context, request: &FontRequest) -> Result<Vec<String>, FontError> {
        let answer = self
            .answers
            .lock()
            .unwrap()
            .entry((request.name.clone(), request.version.clone()))
            .or_default()
            .clone();
        let mut first = false;
        let result = answer.get_or_init(|| {
            first = true;
            resolve(ctx, request)
        });
        result.clone().map_err(|message| FontError {
            message,
            repeated: !first,
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
    let mut index = None;
    let (dir, _) = ensure(ctx, &request.name, &request.version, || {
        let index = match &mut index {
            Some(index) => index,
            None => index.insert(fetch_index(ctx)?),
        };
        let found = index
            .fonts
            .iter()
            .find(|p| p.name == request.name)
            .and_then(|p| Some((p, p.versions.iter().find(|v| v.version == request.version)?)));
        let (package, version) = found
            .ok_or_else(|| format!("font {} {} isn't in the registry", request.name, request.version))?;
        Ok(cached_version(package, version))
    })?;
    Ok(font_paths(&dir))
}

/// A font package made ready ahead of a run by [`install`].
#[derive(Debug)]
pub struct Installed {
    pub name: String,
    pub version: String,
    pub scripts: Vec<String>,
    pub dir: PathBuf,
    /// False when the verified files were already cached.
    pub downloaded: bool,
}

/// Fill the font cache ahead of time (offline servers, Docker image builds) with the newest
/// version of each package covering `scripts`, or of every package when `scripts` is empty. A
/// script is the registry's code (`ja`, `zh-Hans`, `Arab`), case-insensitively, or a language
/// code for it ([`script_for`]). Nothing is downloaded when one of them is unknown.
pub fn install(ctx: &Context, scripts: &[String]) -> Result<Vec<Installed>, String> {
    let index = fetch_index(ctx)?;
    let mut chosen: Vec<&Package> = Vec::new();
    if scripts.is_empty() {
        chosen.extend(&index.fonts);
    }
    for asked in scripts {
        let script = script_for(asked);
        let package = index
            .fonts
            .iter()
            .find(|p| p.scripts.iter().any(|s| s.eq_ignore_ascii_case(script)))
            .ok_or_else(|| {
                format!(
                    "no font in the registry covers `{asked}`; known scripts: {}",
                    known(&index)
                )
            })?;
        if !chosen.iter().any(|p| p.name == package.name) {
            chosen.push(package);
        }
    }
    let mut installed = Vec::new();
    for package in chosen {
        let version = package
            .versions
            .iter()
            .filter_map(|v| Some((semver::Version::parse(&v.version).ok()?, v)))
            .max_by(|a, b| a.0.cmp(&b.0))
            .map(|(_, v)| v)
            .ok_or_else(|| format!("font {} has no valid version in the registry", package.name))?;
        let (dir, downloaded) = ensure(ctx, &package.name, &version.version, || {
            Ok(cached_version(package, version))
        })?;
        installed.push(Installed {
            name: package.name.clone(),
            version: version.version.clone(),
            scripts: package.scripts.clone(),
            dir,
            downloaded,
        });
    }
    Ok(installed)
}

/// A font package in the cache, as [`cached`] finds it.
#[derive(Debug)]
pub struct Cached {
    pub name: String,
    pub version: String,
    /// The scripts it covers (empty for packages cached by DRE before 0.9.0).
    pub scripts: Vec<String>,
    pub dir: PathBuf,
    /// The size of its files.
    pub bytes: u64,
    /// Every file is present and matches its checksum; a run would download it again otherwise.
    pub verified: bool,
}

/// The font packages in `~/.dre/fonts`, by name and then version.
pub fn cached(env: &Env) -> Vec<Cached> {
    let mut found = Vec::new();
    let root = env.dre_home().join("fonts");
    for package in read_dirs(&root) {
        for dir in read_dirs(&package) {
            let Some(version) = std::fs::read(dir.join("font.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Version>(&bytes).ok())
            else {
                continue;
            };
            let bytes = version
                .files()
                .iter()
                .filter_map(|(name, _)| std::fs::metadata(dir.join(name)).ok())
                .map(|m| m.len())
                .sum();
            found.push(Cached {
                name: package
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                verified: version.cached(&dir),
                version: version.version,
                scripts: version.scripts,
                dir,
                bytes,
            });
        }
    }
    found.sort_by(|a, b| {
        let v = |c: &Cached| semver::Version::parse(&c.version).ok();
        a.name.cmp(&b.name).then_with(|| v(a).cmp(&v(b)))
    });
    found
}

/// Language codes accepted for a registry script, lower case. The registry names Chinese,
/// Japanese and Korean by language and the other scripts by ISO 15924 code.
const LANGUAGES: &[(&str, &str)] = &[
    ("ar", "Arab"),
    ("he", "Hebr"),
    ("hi", "Deva"),
    ("mr", "Deva"),
    ("ne", "Deva"),
    ("th", "Thai"),
    ("zh", "zh-Hans"),
    ("zh-cn", "zh-Hans"),
    ("zh-sg", "zh-Hans"),
    ("zh-tw", "zh-Hant"),
    ("zh-hk", "zh-Hant"),
    ("zh-mo", "zh-Hant"),
];

/// The registry script for `asked`: a language code's script (`ar` is `Arab`), else `asked`.
pub fn script_for(asked: &str) -> &str {
    LANGUAGES
        .iter()
        .find(|(lang, _)| lang.eq_ignore_ascii_case(asked))
        .map_or(asked, |(_, script)| script)
}

/// The registry's scripts, each with its language codes: "`ja`, `Arab` (also `ar`)".
fn known(index: &FontIndex) -> String {
    index
        .fonts
        .iter()
        .flat_map(|p| &p.scripts)
        .map(|script| {
            let langs: Vec<String> = LANGUAGES
                .iter()
                .filter(|(_, s)| s.eq_ignore_ascii_case(script))
                .map(|(lang, _)| format!("`{lang}`"))
                .collect();
            if langs.is_empty() {
                format!("`{script}`")
            } else {
                format!("`{script}` (also {})", langs.join(", "))
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn fetch_index(ctx: &Context) -> Result<FontIndex, String> {
    let bytes = manager::fetch(ctx, &manager::registry_url(ctx.env()))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("the font registry isn't valid: {e}"))
}

/// The version as the cache records it, with its package's scripts.
fn cached_version(package: &Package, version: &Version) -> Version {
    Version {
        scripts: package.scripts.clone(),
        ..version.clone()
    }
}

fn font_paths(dir: &Path) -> Vec<String> {
    ["regular.otf", "bold.otf"]
        .iter()
        .map(|name| dir.join(name).to_string_lossy().into_owned())
        .collect()
}

fn read_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// Make `name` `version` ready in the cache: its directory, and whether it was downloaded.
/// `lookup` finds the version in the registry, and is called only when the cache can't serve it.
fn ensure(
    ctx: &Context,
    name: &str,
    version: &str,
    lookup: impl FnOnce() -> Result<Version, String>,
) -> Result<(PathBuf, bool), String> {
    let dir = std::path::absolute(ctx.env().dre_home().join("fonts").join(name).join(version))
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
    if cached
        .as_ref()
        .is_some_and(|v| v.version == version && v.cached(&dir))
    {
        return Ok((dir, false));
    }
    let entry = lookup()?;
    for (file, artifact) in entry.files() {
        if !valid_checksum(&artifact.sha256) {
            return Err(format!("font {name} {file} needs a SHA-256 checksum"));
        }
        let bytes = manager::fetch(ctx, &artifact.url)?;
        if !crate::checksum::sha256_hex(&bytes).eq_ignore_ascii_case(&artifact.sha256) {
            return Err(format!("font {name} {file} failed its SHA-256 checksum"));
        }
        atomic_write(&dir.join(file), &bytes)?;
    }
    let bytes = serde_json::to_vec(&entry).map_err(|e| format!("can't record cached font: {e}"))?;
    atomic_write(&manifest, &bytes)?;
    Ok((dir, true))
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
