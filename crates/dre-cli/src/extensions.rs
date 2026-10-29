//! Generic companion binaries: separate from plugins and their protocol handshake.
//!
//! Core knows nothing about an extension's licence beyond the registry's label, which it shows
//! before installing. Installing always needs a yes at the prompt or `--accept-proprietary`, and
//! only runs an artifact downloaded over `https` whose minisign signature matches the key built
//! into `dre`. Settings in a project's `extensions:` entries beyond `name` and `version` belong to
//! the extension, which reads and checks them itself.
use crate::output::{Printer, Tone};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

/// The key extension artifacts are signed with (minisign). Rotating it: add the new key here
/// in a core release before the extension release signed with it.
const TRUSTED_KEY: &str = "RWR2AdtS79I6bUXgpG6WHb3NHBN3Y/iZpzZyiTZoQJopVeOZxP5swhXP";

#[derive(clap::Args)]
pub struct McpArgs {
    /// Install the companion without asking, after reviewing the licence the registry names.
    #[arg(long)]
    pub accept_proprietary: bool,
    /// Arguments passed unchanged to the installed companion.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

#[derive(clap::Subcommand)]
pub enum ExtensionCommand {
    /// Install, or update to the newest version, a separately licensed companion executable.
    #[command(visible_alias = "update")]
    Install(InstallArgs),
}
#[derive(clap::Args)]
pub struct InstallArgs {
    /// Companion package name, optionally with @version.
    pub name: String,
    /// Install without asking, after reviewing the licence the registry names.
    #[arg(long)]
    pub accept_proprietary: bool,
}

#[derive(Deserialize)]
struct Index {
    schema: u32,
    #[serde(default)]
    extensions: Vec<Extension>,
}
#[derive(Deserialize)]
struct Extension {
    name: String,
    kind: String,
    license: String,
    proprietary_license: bool,
    /// The `dre` subcommand that hands off to it; default: the name without `dre-`.
    #[serde(default)]
    command: Option<String>,
    versions: Vec<Version>,
}
#[derive(Deserialize)]
struct Version {
    version: semver::Version,
    artifacts: BTreeMap<String, Artifact>,
}
#[derive(Deserialize)]
struct Artifact {
    url: String,
    sha256: String,
    /// The artifact's minisign signature (the whole `.minisig` file).
    #[serde(default)]
    minisig: Option<String>,
}

/// The `dre` subcommand an extension handles: its registry `command`, else its name without
/// `dre-`.
fn command_of<'a>(name: &'a str, command: Option<&'a str>) -> &'a str {
    command.unwrap_or(name.strip_prefix("dre-").unwrap_or(name))
}

fn executable(name: &str) -> String {
    format!("{name}{}", std::env::consts::EXE_SUFFIX)
}

fn extensions_dir(ctx: &dre_core::Context) -> PathBuf {
    ctx.env().dre_home().join("extensions")
}

/// Whether the user agrees to install: `--accept-proprietary`, else a yes at the prompt.
/// `None` when nobody can be asked.
fn consent(question: &str, accepted: bool) -> Option<bool> {
    if accepted {
        return Some(true);
    }
    if !(std::io::stdin().is_terminal() && std::io::stderr().is_terminal()) {
        return None;
    }
    eprint!("{question} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut answer = String::new();
    let _ = std::io::stdin().read_line(&mut answer);
    Some(["y", "yes"].contains(&answer.trim().to_ascii_lowercase().as_str()))
}

enum Installed {
    Done(PathBuf),
    Declined,
    /// Not interactive and no `--accept-proprietary`.
    NotAsked,
}

fn registry(ctx: &dre_core::Context) -> Result<Vec<Extension>, String> {
    let bytes = dre_core::manager::fetch(ctx, &dre_core::manager::registry_url(ctx.env()))?;
    let index: Index =
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid extension registry: {e}"))?;
    if index.schema != 2 {
        return Err("unsupported extension registry schema".into());
    }
    Ok(index
        .extensions
        .into_iter()
        .filter(|e| e.kind == "extension")
        .collect())
}

/// Download, check and install `name` (optionally `name@version`), after asking.
fn install(ctx: &dre_core::Context, spec: &str, accepted: bool) -> Result<Installed, String> {
    let (name, requested) = spec.split_once('@').map_or((spec, None), |(n, v)| (n, Some(v)));
    if !dre_core::packages::valid_extension_name(name) {
        return Err("invalid extension name".into());
    }
    let extension = registry(ctx)?
        .into_iter()
        .find(|e| e.name == name)
        .ok_or_else(|| format!("extension `{name}` is not in this registry"))?;
    install_from(ctx, extension, requested, accepted)
}

fn install_from(
    ctx: &dre_core::Context,
    extension: Extension,
    requested: Option<&str>,
    accepted: bool,
) -> Result<Installed, String> {
    let name = extension.name.as_str();
    let requested = requested
        .map(semver::Version::parse)
        .transpose()
        .map_err(|e| e.to_string())?;
    let version = extension
        .versions
        .iter()
        .filter(|v| requested.as_ref().is_none_or(|r| r == &v.version))
        .max_by_key(|v| &v.version)
        .ok_or("no matching extension version")?;
    let label = if extension.proprietary_license {
        format!("{} (proprietary)", extension.license)
    } else {
        extension.license.clone()
    };
    match consent(
        &format!(
            "Extension `{name}` {} is licensed under {label}, separately from DRE. Install it?",
            version.version
        ),
        accepted,
    ) {
        Some(true) => {}
        Some(false) => return Ok(Installed::Declined),
        None => return Ok(Installed::NotAsked),
    }
    let platform = dre_core::manager::platform();
    let artifact = version
        .artifacts
        .get(&platform)
        .ok_or_else(|| format!("no extension artifact for {platform}"))?;
    let file_name = artifact.url.rsplit(['/', '\\']).next().unwrap_or_default();
    // Released archives are named for what they hold; the signature covers that name too.
    if artifact.url.starts_with("https://")
        && !file_name.starts_with(&format!("{name}-{}-{platform}.", version.version))
    {
        return Err(format!(
            "extension artifact `{file_name}` doesn't match {name} {} for {platform}",
            version.version
        ));
    }
    let bytes = download(ctx, artifact, file_name)?;
    let digest = dre_core::checksum::sha256_hex(&bytes);
    let file = executable(name);
    let binary = if artifact.url.ends_with(".tar.gz") || artifact.url.ends_with(".tgz") {
        unpack(&bytes, &file)?
    } else {
        bytes
    };
    let root = extensions_dir(ctx).join(name);
    let folder = root.join(version.version.to_string());
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let destination = folder.join(&file);
    dre_core::fs::write_atomic(&destination, &binary).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    let command = command_of(name, extension.command.as_deref());
    let receipt = serde_json::json!({"name": name, "version": version.version, "command": command,
        "license": extension.license, "proprietary_license": extension.proprietary_license,
        "platform": platform, "artifact_sha256": digest});
    dre_core::fs::write_atomic(
        &folder.join("extension.json"),
        serde_json::to_vec(&receipt)
            .expect("a JSON value serializes")
            .as_slice(),
    )
    .map_err(|e| e.to_string())?;
    dre_core::fs::write_atomic(&root.join("current"), version.version.to_string().as_bytes())
        .map_err(|e| e.to_string())?;
    Ok(Installed::Done(destination))
}

/// The artifact's bytes, once its URL, checksum and signature check out.
fn download(ctx: &dre_core::Context, artifact: &Artifact, file_name: &str) -> Result<Vec<u8>, String> {
    let local_registry = !dre_core::manager::registry_url(ctx.env()).contains("://");
    // A local registry (a development build) may point at local files; anything else needs TLS.
    if !(artifact.url.starts_with("https://") || local_registry && !artifact.url.contains("://")) {
        return Err("extension artifacts must be downloaded over https".into());
    }
    if artifact.sha256.len() != 64 || !artifact.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("extension artifacts require a published SHA-256 checksum".into());
    }
    let signature = artifact
        .minisig
        .as_deref()
        .ok_or("the extension artifact is not signed; download refused")?;
    let bytes = dre_core::manager::fetch(ctx, &artifact.url)?;
    if !dre_core::checksum::sha256_hex(&bytes).eq_ignore_ascii_case(&artifact.sha256) {
        return Err("extension artifact checksum mismatch; download discarded".into());
    }
    verify(&bytes, signature, file_name)?;
    Ok(bytes)
}

/// Check `bytes` against a minisign signature from the trusted key, whose signed (trusted)
/// comment names `file_name`: a signature can't be replayed for another artifact.
fn verify(bytes: &[u8], signature: &str, file_name: &str) -> Result<(), String> {
    let refused = |why: String| format!("extension signature check failed ({why}); download discarded");
    let key = trusted_key();
    let key = minisign_verify::PublicKey::from_base64(&key).map_err(|e| refused(e.to_string()))?;
    let signature = minisign_verify::Signature::decode(signature).map_err(|e| refused(e.to_string()))?;
    key.verify(bytes, &signature, false)
        .map_err(|e| refused(e.to_string()))?;
    if !signature
        .trusted_comment()
        .split('\t')
        .any(|field| field == format!("file:{file_name}"))
    {
        return Err(refused(format!(
            "the signature is for another file than {file_name}"
        )));
    }
    Ok(())
}

/// Tests sign with their own key; release builds only ever trust the built-in one.
fn trusted_key() -> String {
    #[cfg(debug_assertions)]
    if let Ok(key) = std::env::var("DRE_TEST_EXTENSION_KEY") {
        return key;
    }
    TRUSTED_KEY.into()
}

fn unpack(archive: &[u8], file: &str) -> Result<Vec<u8>, String> {
    use std::io::Read;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(archive));
    let mut found = None;
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        if entry.header().entry_type().is_file()
            && entry
                .path()
                .map_err(|e| e.to_string())?
                .file_name()
                .is_some_and(|n| n == file)
        {
            if found.is_some() {
                return Err("extension archive contains duplicate executables".into());
            }
            let mut data = Vec::new();
            entry.read_to_end(&mut data).map_err(|e| e.to_string())?;
            found = Some(data);
        }
    }
    found.ok_or_else(|| "extension archive has no expected executable".into())
}

/// `dre deps`: install the project's declared extensions at their pinned versions.
pub fn sync_declared(
    ctx: &dre_core::Context,
    root: &Path,
    accepted: bool,
    printer: &Printer,
) -> Result<usize, String> {
    let mut installed = 0;
    for entry in dre_core::packages::declared_extensions(root)? {
        let path = extensions_dir(ctx)
            .join(&entry.name)
            .join(&entry.version)
            .join(executable(&entry.name));
        if path.is_file() {
            installed += 1;
            continue;
        }
        match install(ctx, &format!("{}@{}", entry.name, entry.version), accepted)? {
            Installed::Done(path) => {
                printer.line(Tone::Good, "Installed", &format!("companion {}", path.display()));
                installed += 1;
            }
            Installed::Declined => printer.line(Tone::Warn, "Skipped", &format!("companion {}", entry.name)),
            Installed::NotAsked => printer.line(
                Tone::Warn,
                "Skipped",
                &format!(
                    "companion {}: installing it needs a yes at the prompt or --accept-proprietary",
                    entry.name
                ),
            ),
        }
    }
    Ok(installed)
}

pub fn manage(ctx: &dre_core::Context, command: ExtensionCommand, printer: &Printer) -> ExitCode {
    let ExtensionCommand::Install(args) = command;
    match install(ctx, &args.name, args.accept_proprietary) {
        Ok(Installed::Done(path)) => {
            printer.line(Tone::Good, "Installed", &format!("extension {}", path.display()));
            ExitCode::SUCCESS
        }
        Ok(Installed::Declined) => ExitCode::from(1),
        Ok(Installed::NotAsked) => {
            printer.error("not installed: review the licence, then pass --accept-proprietary");
            ExitCode::from(2)
        }
        Err(error) => {
            printer.error(&error);
            ExitCode::from(2)
        }
    }
}

/// The installed extension handling `dre <command>`: the declared pin's version when the
/// project has one, else the current version.
fn installed(
    ctx: &dre_core::Context,
    command: &str,
    pins: &[dre_core::config::dependencies::ExtensionEntry],
) -> Option<PathBuf> {
    let mut dirs: Vec<_> = std::fs::read_dir(extensions_dir(ctx)).ok()?.flatten().collect();
    dirs.sort_by_key(|d| d.file_name());
    for dir in dirs {
        let name = dir.file_name().to_string_lossy().into_owned();
        let version = match pins.iter().find(|e| e.name == name) {
            Some(pin) => pin.version.clone(),
            None => std::fs::read_to_string(dir.path().join("current")).unwrap_or_default(),
        };
        let Ok(version) = semver::Version::parse(version.trim()) else {
            continue;
        };
        let folder = dir.path().join(version.to_string());
        let receipt: serde_json::Value = std::fs::read(folder.join("extension.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default();
        let handles = command_of(&name, receipt["command"].as_str()) == command;
        let path = folder.join(executable(&name));
        if handles && path.is_file() {
            return Some(path);
        }
    }
    None
}

fn project_dir(args: &[String]) -> PathBuf {
    args.windows(2)
        .find_map(|pair| match pair {
            [flag, dir] if flag == "--project-dir" => Some(PathBuf::from(dir)),
            _ => None,
        })
        .or_else(|| {
            args.iter()
                .find_map(|arg| arg.strip_prefix("--project-dir=").map(PathBuf::from))
        })
        .unwrap_or_else(|| PathBuf::from("."))
}

/// `dre <command> …`: run the extension that handles `command`, with `--dre-bin` set to this
/// `dre` so it never runs a different one from the PATH. Offers to install it when missing.
pub fn handoff(ctx: &dre_core::Context, command: &str, args: McpArgs, printer: &Printer) -> ExitCode {
    let pins = match dre_core::packages::declared_extensions(&project_dir(&args.args)) {
        Ok(pins) => pins,
        Err(error) => {
            printer.error(&error);
            return ExitCode::from(2);
        }
    };
    let variable = format!("DRE_{}_BIN", command.to_ascii_uppercase());
    let binary = match std::env::var_os(&variable) {
        Some(binary) => PathBuf::from(binary),
        None => match installed(ctx, command, &pins) {
            Some(binary) => binary,
            None => match offer(ctx, command, &pins, args.accept_proprietary, printer) {
                Ok(binary) => binary,
                Err(code) => return code,
            },
        },
    };
    let mut forwarded = Vec::new();
    if !args
        .args
        .iter()
        .any(|a| a == "--dre-bin" || a.starts_with("--dre-bin="))
        && let Ok(dre) = std::env::current_exe()
    {
        forwarded.push("--dre-bin".into());
        forwarded.push(dre.into_os_string());
    }
    forwarded.extend(args.args.iter().map(Into::into));
    match Command::new(&binary).args(&forwarded).status() {
        Ok(status) => ExitCode::from(status.code().unwrap_or(1).clamp(0, 255) as u8),
        Err(error) => {
            printer.error(&format!("cannot start {}: {error}", binary.display()));
            ExitCode::from(2)
        }
    }
}

/// Install the extension handling `command` (the project's pinned version, if any).
fn offer(
    ctx: &dre_core::Context,
    command: &str,
    pins: &[dre_core::config::dependencies::ExtensionEntry],
    accepted: bool,
    printer: &Printer,
) -> Result<PathBuf, ExitCode> {
    let failed = |error: String| {
        printer.error(&error);
        ExitCode::from(2)
    };
    let extension = registry(ctx)
        .map_err(failed)?
        .into_iter()
        .find(|e| command_of(&e.name, e.command.as_deref()) == command)
        .ok_or_else(|| failed(format!("no extension provides `dre {command}` in this registry")))?;
    let name = extension.name.clone();
    let pin = pins.iter().find(|e| e.name == name).map(|e| e.version.clone());
    match install_from(ctx, extension, pin.as_deref(), accepted).map_err(failed)? {
        Installed::Done(path) => Ok(path),
        Installed::Declined => Err(ExitCode::from(1)),
        Installed::NotAsked => Err(failed(format!(
            "`dre {command}` needs the {name} extension, which isn't installed; run `dre {command} --accept-proprietary` or `dre extension install {name}` after reviewing its licence"
        ))),
    }
}
