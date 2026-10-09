//! `profiles.yml`: connection definitions, kept outside the project.
//!
//! Two sections keep database connections apart from delivery targets:
//!
//! ```yaml
//! connections:
//!   warehouse:
//!     targets:
//!       dev: {type: duckdb, path: dev.duckdb}
//! destinations:
//!   client_sftp:
//!     targets:
//!       dev: {type: sftp, host: sftp.example.com}
//! ```
//!
//! A connection profile is referenced by `default_profile`/`profile:` (and a source's
//! `profile:`), a destination profile by `output.destination.profile`; each is looked up in its
//! own section only. Each profile lists one entry per target (environment). Each profile a run
//! uses picks its entry by `--target`, else `DRE_TARGET` (either sets every profile), else the
//! profile's own `target:`, else `dev`. A used profile without that entry is an error; a
//! destination entry `{deliver: false}` deliberately delivers nowhere.
//!
//! DRE 0.1 called the connections section `sources:`; it still loads in 0.2.x, with a warning.
//!
//! Location: `--profiles-dir` > `DRE_PROFILES_DIR` > `~/.dre`, one file, never merged. These are
//! read directly at startup, never through the `env_var()` Jinja function. `env_var()` calls
//! *inside* the file are rendered at run time, just before a connection config is handed to a
//! plugin.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::codes::Code;
use crate::config::de::{self, Found, Located, Loose, Map, UnknownKeys};
use crate::config::node;
use crate::diag::Diagnostics;

pub const PROFILES_FILE: &str = "profiles.yml";

/// The built-in destination type: copying bytes to a local path needs no plugin (ADR 0003).
/// `profile: local` works without a profiles.yml entry; defining a `local` profile overrides it.
pub const LOCAL_TYPE: &str = "local";

/// What `profile: local` resolves to when profiles.yml doesn't define it, for every target.
pub static BUILTIN_LOCAL: std::sync::LazyLock<ProfileTarget> = std::sync::LazyLock::new(|| ProfileTarget {
    kind: LOCAL_TYPE.into(),
    fields: serde_json::Map::new(),
});

/// The section DRE 0.1 kept connections under; read with a warning in 0.2.x.
pub const OLD_CONNECTIONS_SECTION: &str = "sources";

/// The run's target (environment), and a profile's entry, when nothing chooses one.
pub const DEFAULT_TARGET: &str = "dev";
/// The environment variable choosing the run's target, below `--target`.
pub const TARGET_ENV: &str = crate::settings::TARGET;

/// Which section of profiles.yml a profile lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Connection,
    Destination,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Connection => "connection",
            Role::Destination => "destination",
        }
    }
    /// The profiles.yml section key.
    pub fn section(self) -> &'static str {
        match self {
            Role::Connection => "connections",
            Role::Destination => "destinations",
        }
    }
}

/// Where the run's target came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TargetSource {
    Flag,
    Env,
    #[default]
    Default,
}

impl std::fmt::Display for TargetSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            TargetSource::Flag => "--target",
            TargetSource::Env => TARGET_ENV,
            TargetSource::Default => "default",
        })
    }
}

/// The run's target: `--target`, else `DRE_TARGET`, else `dev`. It's `target.name` in templates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunTarget {
    pub name: String,
    pub from: TargetSource,
}

impl Default for RunTarget {
    fn default() -> Self {
        RunTarget {
            name: DEFAULT_TARGET.into(),
            from: TargetSource::Default,
        }
    }
}

impl RunTarget {
    /// `--target` (`flag`), else `DRE_TARGET`, else `dev`.
    pub fn resolve(flag: Option<&str>) -> RunTarget {
        if let Some(t) = flag.filter(|t| !t.is_empty()) {
            return RunTarget {
                name: t.to_string(),
                from: TargetSource::Flag,
            };
        }
        if let Some(t) = crate::settings::env(TARGET_ENV) {
            return RunTarget {
                name: t,
                from: TargetSource::Env,
            };
        }
        RunTarget::default()
    }

    /// Whether `--target` or `DRE_TARGET` chose it, which sets every profile's entry.
    pub fn chosen(&self) -> bool {
        self.from != TargetSource::Default
    }
}

/// What a profile uses for the run: its entry, or why it has none.
#[derive(Debug, Clone, Copy)]
pub enum Entry<'a> {
    Use(&'a ProfileTarget),
    /// `{deliver: false}`: a destination that deliberately delivers nowhere on this target.
    Nowhere,
    /// The profile has no entry for its target.
    Missing,
    /// No such profile.
    Unknown,
}

/// One environment of a profile: a plugin type and its connection fields.
#[derive(Debug, Clone, Serialize)]
pub struct ProfileTarget {
    #[serde(rename = "type")]
    pub kind: String,
    /// Every other field, passed through to the plugin unvalidated by core.
    #[serde(skip)]
    pub fields: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Profile {
    /// Its own default entry (`target:`), below `--target` and `DRE_TARGET`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// The entries that connect or deliver.
    pub targets: BTreeMap<String, ProfileTarget>,
    /// The destination entries written `{deliver: false}`.
    #[serde(skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub nowhere: std::collections::BTreeSet<String>,
}

impl Profile {
    /// Every entry's name, delivering or not.
    pub fn entry_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.targets.keys().chain(&self.nowhere).cloned().collect();
        names.sort();
        names
    }
}

#[derive(Debug, Clone, Default)]
pub struct Profiles {
    /// Where DRE looked for the file.
    pub path: PathBuf,
    /// Why that directory: `--profiles-dir`, `DRE_PROFILES_DIR`, `the project directory` or `~/.dre`.
    pub found_by: &'static str,
    /// The file as shown in diagnostics; `None` when it doesn't exist. Loading problems are
    /// reported when it's needed.
    pub file: Option<PathBuf>,
    /// Every profile each section declares, broken ones too, with its name's line.
    declared: BTreeMap<Role, BTreeMap<String, usize>>,
    pub connections: BTreeMap<String, Profile>,
    pub destinations: BTreeMap<String, Profile>,
    /// The section connections were read from: `connections`, or 0.1's `sources`.
    connections_section: &'static str,
    /// The run's target, which picks each profile's entry (see [`Profiles::target_of`]).
    pub run: RunTarget,
}

/// Resolve the profiles directory: `--profiles-dir` > `DRE_PROFILES_DIR` > `~/.dre`.
/// `dre init` writes here; projects look in their own directory first (see [`locate`]).
pub fn profiles_dir(cli: Option<&Path>) -> PathBuf {
    locate(cli, None).0
}

/// Find the profiles directory for a project, with the reason it was chosen:
/// `--profiles-dir` > `DRE_PROFILES_DIR` > the project directory (when it holds a
/// profiles.yml) > `~/.dre`. With the default `--project-dir .` this is dbt's order.
pub fn locate(cli: Option<&Path>, project: Option<&Path>) -> (PathBuf, &'static str) {
    if let Some(p) = cli {
        return (p.to_path_buf(), "--profiles-dir");
    }
    if let Some(p) = crate::settings::env(crate::settings::PROFILES_DIR) {
        return (PathBuf::from(p), "DRE_PROFILES_DIR");
    }
    if let Some(p) = project.filter(|p| p.join(PROFILES_FILE).is_file()) {
        return (p.to_path_buf(), "the project directory");
    }
    (crate::dre_home(), "~/.dre")
}

impl Profiles {
    pub fn load(dir: &Path, found_by: &'static str, diags: &mut Diagnostics) -> Profiles {
        let path = crate::slash(&dir.join(PROFILES_FILE));
        let mut out = Profiles {
            path: path.clone(),
            found_by,
            ..Default::default()
        };
        if !path.is_file() {
            return out;
        }
        // Parsed badly or not, the file exists: references to it don't pile up extra errors.
        out.file = Some(path.clone());
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                diags.error(Code::IoError, Some(path), None, format!("cannot read file: {e}"));
                return out;
            }
        };
        let tree = match node::parse(&text) {
            Ok(n) => n,
            Err(e) => {
                diags.error(
                    Code::YamlSyntax,
                    Some(path),
                    e.line,
                    format!("invalid YAML: {}", e.message),
                );
                return out;
            }
        };
        let file = Some(path.clone());
        let raw: Loose<ProfilesFile> = match de::from_node(&tree) {
            Ok(r) => r,
            Err(e) => {
                diags.error(Code::InvalidProfiles, file, e.line, e.message);
                return out;
            }
        };
        let raw = match raw {
            Loose::Ok(r) => r,
            Loose::Bad(Found { kind: "nothing", .. }) => return out,
            Loose::Bad(_) => {
                diags.error(
                    Code::InvalidProfiles,
                    file,
                    None,
                    "profiles.yml must be a map with `connections:` and/or `destinations:`",
                );
                return out;
            }
        };
        for k in &raw.unknown.0 {
            diags.error(
                Code::InvalidProfiles,
                file.clone(),
                Some(k.line),
                format!(
                    "unknown profiles.yml section `{}`; profiles go under `connections:` or `destinations:`",
                    k.name
                ),
            );
        }
        let connections = match (raw.connections, raw.sources) {
            (Some(c), Some(s)) => {
                diags.error(
                    Code::InvalidProfiles,
                    file.clone(),
                    s.line(),
                    "profiles.yml has both `connections:` and `sources:`; `sources:` is the old name of `connections:`, so move its profiles under `connections:`",
                );
                Some(("connections", c))
            }
            (Some(c), None) => Some(("connections", c)),
            (None, Some(s)) => {
                diags.warning(
                    Code::ProfilesSourcesRenamed,
                    file.clone(),
                    s.line(),
                    "`sources:` in profiles.yml is now `connections:` (DRE 0.2); rename it. `sources:` still works in 0.2.x",
                );
                out.connections_section = OLD_CONNECTIONS_SECTION;
                Some((OLD_CONNECTIONS_SECTION, s))
            }
            (None, None) => None,
        };
        if let Some((key, section)) = connections {
            let (parsed, declared) = parse_section(Role::Connection, key, section, &path, diags);
            out.connections = parsed;
            out.declared.insert(Role::Connection, declared);
        }
        if let Some(section) = raw.destinations {
            let (parsed, declared) = parse_section(Role::Destination, "destinations", section, &path, diags);
            out.destinations = parsed;
            out.declared.insert(Role::Destination, declared);
        }
        out
    }

    pub fn exists(&self) -> bool {
        self.file.is_some()
    }

    fn section(&self, role: Role) -> &BTreeMap<String, Profile> {
        match role {
            Role::Connection => &self.connections,
            Role::Destination => &self.destinations,
        }
    }

    /// The section key a role's profiles were read from (`sources` for an 0.1 file).
    pub fn section_key(&self, role: Role) -> &'static str {
        match role {
            Role::Connection if self.connections_section == OLD_CONNECTIONS_SECTION => {
                OLD_CONNECTIONS_SECTION
            }
            r => r.section(),
        }
    }

    pub fn get(&self, role: Role, name: &str) -> Option<&Profile> {
        self.section(role).get(name)
    }

    /// Whether a (possibly partially broken) profile of this name is declared in its section.
    pub fn declares(&self, role: Role, name: &str) -> bool {
        self.declared.get(&role).is_some_and(|s| s.contains_key(name))
    }

    /// Whether `name` means the built-in local destination (no `local` profile is defined).
    pub fn is_builtin_local(&self, name: &str) -> bool {
        name == LOCAL_TYPE && !self.destinations.contains_key(name)
    }

    /// The line of a profile's name in the file.
    pub fn line_of(&self, role: Role, name: &str) -> Option<usize> {
        self.declared.get(&role)?.get(name).copied()
    }

    /// The entry a profile uses in this run: the run's target when `--target` or `DRE_TARGET`
    /// chose it, else the profile's own `target:`, else `dev`.
    pub fn target_of(&self, role: Role, profile: &str) -> String {
        if self.run.chosen() {
            return self.run.name.clone();
        }
        self.get(role, profile)
            .and_then(|p| p.target.clone())
            .unwrap_or_else(|| self.run.name.clone())
    }

    /// What a profile uses in this run. `profile: local` without a defined `local` profile is
    /// the built-in local destination, for every target.
    pub fn entry(&self, role: Role, profile: &str) -> Entry<'_> {
        if role == Role::Destination && self.is_builtin_local(profile) {
            return Entry::Use(&BUILTIN_LOCAL);
        }
        let Some(p) = self.get(role, profile) else {
            return Entry::Unknown;
        };
        let t = self.target_of(role, profile);
        match p.targets.get(&t) {
            Some(o) => Entry::Use(o),
            None if p.nowhere.contains(&t) => Entry::Nowhere,
            None => Entry::Missing,
        }
    }

    /// A profile's settings for this run, when it has an entry that connects or delivers.
    pub fn target(&self, role: Role, profile: &str) -> Option<&ProfileTarget> {
        match self.entry(role, profile) {
            Entry::Use(o) => Some(o),
            _ => None,
        }
    }

    /// Why a used profile has no entry for this run, with the way to fix it.
    pub fn missing_entry(&self, role: Role, profile: &str) -> String {
        let t = self.target_of(role, profile);
        let has = self
            .get(role, profile)
            .map(|p| p.entry_names().join(", "))
            .unwrap_or_default();
        let why = match self.run.from {
            TargetSource::Default if self.get(role, profile).is_some_and(|p| p.target.is_some()) => {
                "its `target:`".to_string()
            }
            TargetSource::Default => "the default".to_string(),
            from => from.to_string(),
        };
        let mut msg = format!(
            "{} `{profile}` has no `{t}` entry (it has: {has}); `{t}` comes from {why}",
            role.as_str()
        );
        if role == Role::Destination {
            msg.push_str(&format!(
                ". To deliver nowhere on `{t}`, add `{t}: {{deliver: false}}` to its `targets:`"
            ));
        }
        msg
    }
}

/// A section's profiles, and every profile name it declares with its line.
fn parse_section<P: Into<RawProfile>>(
    role: Role,
    key: &str,
    section: Section<P>,
    path: &Path,
    diags: &mut Diagnostics,
) -> (BTreeMap<String, Profile>, BTreeMap<String, usize>) {
    let Loose::Ok(profiles) = section.value else {
        diags.error(
            Code::InvalidProfiles,
            Some(path.to_path_buf()),
            section.line(),
            format!("`{key}` must be a map of profile names"),
        );
        return Default::default();
    };
    let declared = profiles.iter().map(|(k, _)| (k.value.clone(), k.line)).collect();
    let parsed = profiles
        .0
        .into_iter()
        .filter_map(|(name, p)| {
            parse_profile(role, &name, p.map(|p| p.map(Into::into)), path, diags).map(|p| (name.value, p))
        })
        .collect();
    (parsed, declared)
}

fn parse_profile(
    role: Role,
    name: &Located<String>,
    v: Located<Loose<RawProfile>>,
    path: &Path,
    diags: &mut Diagnostics,
) -> Option<Profile> {
    let file = Some(path.to_path_buf());
    let line = name.line();
    let what = format!("{} profile `{}`", role.as_str(), name.value);
    let Loose::Ok(m) = v.value else {
        diags.error(
            Code::InvalidProfile,
            file,
            line,
            format!("{what} must be a map with `targets`"),
        );
        return None;
    };
    let target_line = m.target.as_ref().and_then(Located::line);
    let own_target = match m.target {
        None => None,
        Some(Located {
            value: Loose::Ok(t), ..
        }) if !t.trim().is_empty() => Some(t),
        Some(t) => {
            diags.error(
                Code::InvalidProfile,
                file.clone(),
                t.line(),
                format!("{what}: `target` must be the name of one of its `targets`"),
            );
            return None;
        }
    };
    let Some(Loose::Ok(targets)) = m.targets.map(|t| t.value) else {
        let hint = if m.outputs.is_some() {
            " (`outputs:` is now `targets:`)"
        } else {
            ""
        };
        diags.error(
            Code::InvalidProfile,
            file,
            line,
            format!("{what} needs a map of named `targets`{hint}"),
        );
        return None;
    };
    let mut parsed = BTreeMap::new();
    let mut nowhere = std::collections::BTreeSet::new();
    let mut ok = true;
    for (tname, o) in targets.0 {
        let tline = tname.line();
        let tname = tname.value;
        let o = o.value;
        if let Some(d) = o.get("deliver") {
            let problem = if role == Role::Connection {
                Some("`deliver: false` is only for destinations; a connection entry needs a `type`")
            } else if d != &serde_json::Value::Bool(false) {
                Some("`deliver` can only be `false` (an entry that delivers just has a `type`)")
            } else if o.as_object().is_some_and(|m| m.len() > 1) {
                Some("`deliver: false` takes no other settings: the entry delivers nowhere")
            } else {
                None
            };
            match problem {
                Some(p) => {
                    diags.error(
                        Code::InvalidProfile,
                        file.clone(),
                        tline,
                        format!("target `{tname}` of {what}: {p}"),
                    );
                    ok = false;
                }
                None => {
                    nowhere.insert(tname);
                }
            }
            continue;
        }
        let Some(kind) = o.get("type").and_then(serde_json::Value::as_str) else {
            diags.error(
                Code::InvalidProfile,
                file.clone(),
                tline,
                format!("target `{tname}` of {what} has no `type`"),
            );
            ok = false;
            continue;
        };
        let kind = kind.to_string();
        let fields = match o {
            serde_json::Value::Object(mut f) => {
                f.remove("type");
                f
            }
            _ => serde_json::Map::new(),
        };
        parsed.insert(tname, ProfileTarget { kind, fields });
    }
    if let Some(t) = &own_target
        && ok
        && !parsed.contains_key(t)
        && !nowhere.contains(t)
    {
        let mut has: Vec<&String> = parsed.keys().chain(&nowhere).collect();
        has.sort();
        diags.error(
            Code::InvalidProfile,
            file.clone(),
            target_line,
            format!(
                "{what}: `target: {t}` isn't one of its targets ({})",
                has.into_iter().cloned().collect::<Vec<_>>().join(", ")
            ),
        );
        return None;
    }
    ok.then_some(Profile {
        target: own_target,
        targets: parsed,
        nowhere,
    })
}

// The file's shape. These types are what's read, and they generate profiles.schema.json; their
// doc comments are its descriptions. Values the loader checks itself, to say what's wrong in
// DRE's words, are `Loose`.

/// A section of profiles by name, each checked by the loader.
type Section<P> = Located<Loose<Map<Located<Loose<P>>>>>;

/// Connections and destinations, in `profiles.yml`: what reports read from and where outputs go. Kept outside the project (`~/.dre`, `--profiles-dir` or `DRE_PROFILES_DIR`). Every profile lists its targets (environments). Each profile the run uses picks one: `--target`, else `DRE_TARGET` (either sets every profile), else the profile's own `target`, else `dev`; a used profile without that entry is an error.
#[derive(Deserialize, JsonSchema)]
#[schemars(title = "DRE profiles", deny_unknown_fields)]
#[schemars(extend("not" = {"required": ["connections", "sources"]}))]
pub struct ProfilesFile {
    /// Database connections, referenced by `default_profile`, `profile:` (report, query, Set, folder `+profile`) and a source's `profile`.
    connections: Option<Section<ConnectionProfile>>,
    /// DRE 0.1's name for `connections:`. Still read in 0.2.x, with a warning: rename it to `connections:`.
    #[schemars(extend("deprecated" = true))]
    sources: Option<Section<ConnectionProfile>>,
    /// Delivery targets, referenced by `output.destination.profile`.
    destinations: Option<Section<DestinationProfile>>,
    #[serde(rename = "$unknown", default)]
    #[schemars(skip)]
    unknown: UnknownKeys,
}

/// A named connection with one entry per target (environment).
#[derive(Deserialize, JsonSchema)]
#[schemars(rename = "connection", deny_unknown_fields)]
pub struct ConnectionProfile {
    /// This profile's entry when neither `--target` nor `DRE_TARGET` is set (dbt's key). Default: `dev`. It doesn't change `target.name`, the run's target.
    target: Option<Located<Loose<String>>>,
    /// The environments of this connection, by name (e.g. `dev`, `prod`).
    #[schemars(required, extend("minProperties" = 1))]
    targets: Option<Located<Loose<Map<Located<ConnectionEntry>>>>>,
    /// DRE 0.1's name for `targets`, read only to say so.
    #[schemars(skip)]
    outputs: Option<serde::de::IgnoredAny>,
}

/// A named destination with one entry per target (environment).
#[derive(Deserialize, JsonSchema)]
#[schemars(rename = "destination", deny_unknown_fields)]
pub struct DestinationProfile {
    /// The environments of this destination, by name (e.g. `dev`, `prod`). An entry is a delivery configuration, or `{deliver: false}` to deliver nowhere on that target.
    #[schemars(required, extend("minProperties" = 1))]
    targets: Option<Located<Loose<Map<Located<DestinationEntry>>>>>,
    /// This profile's entry when neither `--target` nor `DRE_TARGET` is set (dbt's key). Default: `dev`. It doesn't change `target.name`, the run's target.
    target: Option<Located<Loose<String>>>,
    #[schemars(skip)]
    outputs: Option<serde::de::IgnoredAny>,
}

/// Either kind of profile, as the loader checks it.
pub struct RawProfile {
    target: Option<Located<Loose<String>>>,
    targets: Option<Located<Loose<Map<Located<serde_json::Value>>>>>,
    outputs: Option<serde::de::IgnoredAny>,
}

fn entries<E: Into<serde_json::Value>>(
    t: Option<Located<Loose<Map<Located<E>>>>>,
) -> Option<Located<Loose<Map<Located<serde_json::Value>>>>> {
    t.map(|t| {
        t.map(|t| match t {
            Loose::Ok(m) => Loose::Ok(Map(m
                .0
                .into_iter()
                .map(|(k, v)| (k, v.map(Into::into)))
                .collect())),
            Loose::Bad(f) => Loose::Bad(f),
        })
    })
}

impl From<ConnectionProfile> for RawProfile {
    fn from(p: ConnectionProfile) -> RawProfile {
        RawProfile {
            target: p.target,
            targets: entries(p.targets),
            outputs: p.outputs,
        }
    }
}

impl From<DestinationProfile> for RawProfile {
    fn from(p: DestinationProfile) -> RawProfile {
        RawProfile {
            target: p.target,
            targets: entries(p.targets),
            outputs: p.outputs,
        }
    }
}

/// A connection's target: `type` and the plugin's fields, passed on unchecked.
#[derive(Deserialize)]
#[serde(transparent)]
pub struct ConnectionEntry(serde_json::Value);

/// A destination's target: a delivery configuration, or `{deliver: false}`.
#[derive(Deserialize)]
#[serde(transparent)]
pub struct DestinationEntry(serde_json::Value);

impl From<ConnectionEntry> for serde_json::Value {
    fn from(e: ConnectionEntry) -> Self {
        e.0
    }
}

impl From<DestinationEntry> for serde_json::Value {
    fn from(e: DestinationEntry) -> Self {
        e.0
    }
}

impl JsonSchema for ConnectionEntry {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "ConnectionEntry".into()
    }
    fn inline_schema() -> bool {
        true
    }
    fn json_schema(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
        g.subschema_for::<TargetEntry>()
    }
}

impl JsonSchema for DestinationEntry {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "DestinationEntry".into()
    }
    fn inline_schema() -> bool {
        true
    }
    fn json_schema(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({
            "oneOf": [g.subschema_for::<TargetEntry>(), g.subschema_for::<NoDelivery>()]
        })
    }
}

/// One target of a profile: a connection or delivery configuration. `type` names the plugin; every other key is a field of that plugin (see the plugins reference), and secrets belong in `{{ env_var('NAME') }}`.
#[derive(JsonSchema)]
#[schemars(rename = "target", extend("additionalProperties" = true, "not" = {"required": ["deliver"]}))]
#[allow(dead_code)]
struct TargetEntry {
    /// The plugin type of the connection, e.g. `duckdb`, `postgres`, `databricks`, `sftp`, `s3`. `local` needs no plugin.
    #[serde(rename = "type")]
    kind: String,
}

/// A destination entry is a delivery configuration (`type` and the plugin's fields, as for a connection), or `{deliver: false}`: it deliberately delivers nowhere. The output stays in the target path, the run logs it, and `run_results.json` records the delivery as `not_delivered`. Destinations only.
#[derive(JsonSchema)]
#[schemars(rename = "no_delivery", deny_unknown_fields)]
#[allow(dead_code)]
struct NoDelivery {
    /// `false`: this target delivers nowhere. Takes no other keys.
    #[schemars(schema_with = "deliver_false")]
    deliver: bool,
}

fn deliver_false(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
    schemars::json_schema!({"const": false, "x-doc-type": "`false`"})
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(yaml: &str) -> (Profiles, Diagnostics) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(PROFILES_FILE), yaml).unwrap();
        let mut diags = Diagnostics::default();
        let p = Profiles::load(dir.path(), "--profiles-dir", &mut diags);
        (p, diags)
    }

    fn errors(d: &Diagnostics) -> Vec<(Option<usize>, String)> {
        d.sorted()
            .into_iter()
            .map(|d| (d.line, d.message.clone()))
            .collect()
    }

    #[test]
    fn a_name_used_twice_gets_the_line_of_the_one_meant() {
        // Searching the text for `wat:` finds the profile on line 2 first.
        let (_, d) = load("connections:\n  wat:\n    targets:\n      dev: {type: duckdb}\nwat: {}\n");
        assert_eq!(
            errors(&d),
            [(
                Some(5),
                "unknown profiles.yml section `wat`; profiles go under `connections:` or `destinations:`"
                    .into()
            )]
        );
    }

    #[test]
    fn a_target_named_like_a_key_gets_its_own_line() {
        let yaml = "connections:\n  w:\n    target: target\n    targets:\n      dev: {type: duckdb}\n      target: {path: x}\n";
        let (_, d) = load(yaml);
        assert_eq!(
            errors(&d),
            [(
                Some(6),
                "target `target` of connection profile `w` has no `type`".into()
            )]
        );
    }

    #[test]
    fn jinja_values_and_yaml_1_1_words_are_strings_passed_on() {
        let yaml = "connections:\n  w:\n    targets:\n      dev:\n        type: postgres\n        password: \"{{ env_var('PG_PASSWORD') }}\"\n        sslmode: no\n";
        let (p, d) = load(yaml);
        assert!(errors(&d).is_empty(), "{:?}", errors(&d));
        let t = p.get(Role::Connection, "w").unwrap().targets.get("dev").unwrap();
        assert_eq!(t.kind, "postgres");
        assert_eq!(t.fields["password"], "{{ env_var('PG_PASSWORD') }}");
        assert_eq!(t.fields["sslmode"], "no");
        assert_eq!(p.line_of(Role::Connection, "w"), Some(2));
    }

    #[test]
    fn syntax_errors_have_their_line() {
        let (p, d) = load("connections:\n  w: [\n");
        assert!(p.exists());
        let e = errors(&d);
        assert_eq!(e.len(), 1);
        assert!(e[0].1.starts_with("invalid YAML: "), "{e:?}");
        assert!(e[0].0.is_some(), "{e:?}");
    }

    #[test]
    fn merge_keys_share_settings_between_targets() {
        let yaml = "connections:\n  w:\n    targets:\n      dev: &dev {type: duckdb, path: dev.duckdb}\n      ci: {<<: *dev, path: ci.duckdb}\n";
        let (p, d) = load(yaml);
        assert!(errors(&d).is_empty());
        let ci = p.get(Role::Connection, "w").unwrap().targets.get("ci").unwrap();
        assert_eq!(
            (ci.kind.as_str(), &ci.fields["path"]),
            ("duckdb", &serde_json::json!("ci.duckdb"))
        );
    }
}
