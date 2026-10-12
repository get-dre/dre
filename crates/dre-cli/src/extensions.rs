//! Generic companion binaries: separate from plugins and their protocol handshake.
use crate::output::Printer;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::{Command, ExitCode};

#[derive(clap::Args)]
pub struct McpArgs {
    /// Arguments passed unchanged to the installed companion.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

#[derive(clap::Subcommand)]
pub enum ExtensionCommand {
    /// Install a separately licensed companion executable.
    Install(InstallArgs),
    /// Update an installed companion after verifying its checksum.
    Update(InstallArgs),
}
#[derive(clap::Args)]
pub struct InstallArgs {
    /// Companion package name, optionally with @version.
    pub name: String,
    /// Acknowledge the proprietary licence shown before an extension is installed.
    #[arg(long)]
    pub accept_proprietary: bool,
    #[command(flatten)]
    pub project: crate::ProjectArgs,
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
    versions: Vec<dre_core::manager::IndexVersion>,
}

fn install(name: &str, accept: bool) -> Result<PathBuf, String> {
    let (name, requested) = name.split_once('@').map_or((name, None), |(n, v)| (n, Some(v)));
    if name.is_empty()
        || !name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err("invalid extension name".into());
    }
    let bytes = dre_core::manager::fetch(&dre_core::manager::registry_url())?;
    let index: Index =
        serde_json::from_slice(&bytes).map_err(|e| format!("invalid extension registry: {e}"))?;
    if index.schema != 2 {
        return Err("unsupported extension registry schema".into());
    }
    let extension = index
        .extensions
        .iter()
        .find(|e| e.name == name && e.kind == "extension")
        .ok_or_else(|| format!("extension `{name}` is not in this registry"))?;
    eprintln!(
        "Extension `{name}` licence: {}{}",
        extension.license,
        if extension.proprietary_license {
            " (proprietary)"
        } else {
            ""
        }
    );
    if extension.proprietary_license && !accept {
        return Err("this extension is proprietary-licensed; review its licence and pass --accept-proprietary to install it".into());
    }
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
    let platform = dre_core::manager::platform();
    let artifact = version
        .artifacts
        .get(&platform)
        .ok_or_else(|| format!("no extension artifact for {platform}"))?;
    if artifact.sha256.len() != 64 || !artifact.sha256.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("extension artifacts require a published SHA-256 checksum".into());
    }
    let bytes = dre_core::manager::fetch(&artifact.url)?;
    let digest = Sha256::digest(&bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    if !digest.eq_ignore_ascii_case(&artifact.sha256) {
        return Err("extension artifact checksum mismatch; download discarded".into());
    }
    let executable = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.into()
    };
    let bytes = if artifact.url.ends_with(".tar.gz") || artifact.url.ends_with(".tgz") {
        use std::io::Read;
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes.as_slice()));
        let mut found = None;
        for entry in archive.entries().map_err(|e| e.to_string())? {
            let mut entry = entry.map_err(|e| e.to_string())?;
            if entry.header().entry_type().is_file()
                && entry
                    .path()
                    .map_err(|e| e.to_string())?
                    .file_name()
                    .is_some_and(|n| n == executable.as_str())
            {
                if found.is_some() {
                    return Err("extension archive contains duplicate executables".into());
                }
                let mut data = Vec::new();
                entry.read_to_end(&mut data).map_err(|e| e.to_string())?;
                found = Some(data);
            }
        }
        found.ok_or("extension archive has no expected executable")?
    } else {
        bytes
    };
    let root = dre_core::dre_home().join("extensions").join(name);
    let folder = root.join(version.version.to_string());
    std::fs::create_dir_all(&folder).map_err(|e| e.to_string())?;
    let destination = folder.join(&executable);
    dre_core::fs::write_atomic(&destination, &bytes).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| e.to_string())?;
    }
    let receipt = serde_json::json!({"name":name,"version":version.version,"license":extension.license,"proprietary_license":extension.proprietary_license,"platform":platform,"artifact_sha256":digest});
    dre_core::fs::write_atomic(
        &folder.join("extension.json"),
        serde_json::to_vec(&receipt).unwrap().as_slice(),
    )
    .map_err(|e| e.to_string())?;
    dre_core::fs::write_atomic(&root.join("current"), version.version.to_string().as_bytes())
        .map_err(|e| e.to_string())?;
    Ok(destination)
}

pub fn manage(command: ExtensionCommand, printer: &Printer) -> ExitCode {
    let args = match command {
        ExtensionCommand::Install(a) | ExtensionCommand::Update(a) => a,
    };
    match install(&args.name, args.accept_proprietary) {
        Ok(path) => {
            println!("Installed extension {}", path.display());
            ExitCode::SUCCESS
        }
        Err(error) => {
            printer.error(&error);
            ExitCode::from(2)
        }
    }
}

fn installed() -> Option<PathBuf> {
    let root = dre_core::dre_home().join("extensions/dre-mcp");
    let current = std::fs::read_to_string(root.join("current")).ok()?;
    let version = semver::Version::parse(current.trim()).ok()?;
    let name = if cfg!(windows) { "dre-mcp.exe" } else { "dre-mcp" };
    let path = root.join(version.to_string()).join(name);
    path.is_file().then_some(path)
}

pub fn handoff(args: McpArgs, _printer: &Printer) -> ExitCode {
    let binary = std::env::var_os("DRE_MCP_BIN")
        .map(PathBuf::from)
        .or_else(installed)
        .unwrap_or_else(|| PathBuf::from("dre-mcp"));
    match Command::new(&binary).args(&args.args).status() {
        Ok(status) => ExitCode::from(status.code().unwrap_or(1).clamp(0, 255) as u8),
        Err(error) => {
            if error.kind() == std::io::ErrorKind::NotFound
                && std::io::stdin().is_terminal()
                && std::io::stderr().is_terminal()
            {
                eprint!(
                    "DRE MCP is a proprietary-licensed companion. Install it from the extension registry? [y/N] "
                );
                let _ = std::io::stderr().flush();
                let mut answer = String::new();
                let _ = std::io::stdin().read_line(&mut answer);
                if ["y", "yes"].contains(&answer.trim().to_ascii_lowercase().as_str()) {
                    match install("dre-mcp", true) {
                        Ok(path) => {
                            return match Command::new(path).args(&args.args).status() {
                                Ok(status) => ExitCode::from(status.code().unwrap_or(1).clamp(0, 255) as u8),
                                Err(error) => {
                                    eprintln!("cannot start installed companion: {error}");
                                    ExitCode::from(2)
                                }
                            };
                        }
                        Err(error) => eprintln!("extension installation failed: {error}"),
                    }
                }
            } else {
                eprintln!(
                    "cannot start {}: {error}; install the companion with `dre extension install dre-mcp --accept-proprietary`, or set DRE_MCP_BIN to a local build",
                    binary.display()
                );
            }
            ExitCode::from(2)
        }
    }
}
