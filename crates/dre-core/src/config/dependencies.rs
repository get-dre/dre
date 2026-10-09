//! The shapes of `dependencies.yml` (or `packages.yml`): `plugins:`, the plugin packages, which
//! any project YAML file may also declare, and `packages:`, the macro packages.

use schemars::JsonSchema;
use serde::Deserialize;
use serde::de::Deserializer;

use super::de::{Located, Loose, Map, OneOf, UnknownKeys};

/// Plugin packages and macro packages, in `dependencies.yml` or `packages.yml` at the project root.
#[derive(Deserialize, JsonSchema)]
#[schemars(title = "DRE dependencies", deny_unknown_fields)]
pub struct DependenciesFile {
    #[serde(default)]
    #[schemars(schema_with = "plugins")]
    pub plugins: Option<PluginsSection>,
    /// Macro packages (git or local). Their macros are called through the package name, e.g. `{{ dre_utils.star(...) }}`. Exact commits are pinned in `dre.lock`.
    pub packages: Option<Located<Loose<Vec<Loose<Package>>>>>,
}

/// The `plugins:` key of any project YAML file.
#[derive(Deserialize)]
pub struct PluginsKey {
    pub plugins: Option<PluginsSection>,
}

/// `plugins:`: a list of packages, or package names mapped to versions.
pub type PluginsSection = Located<Loose<OneOf<Vec<Located<Loose<PluginItem>>>, Map<serde_json::Value>>>>;

/// One `plugins:` item (see [`Item`] for its description).
pub type PluginItem = OneOf<PackageName, OneOf<PinnedVersion, PluginEntry>>;

/// A package name, e.g. `duckdb`. Any version.
#[derive(Deserialize, JsonSchema)]
#[serde(transparent)]
#[schemars(inline)]
pub struct PackageName(pub String);

/// A package name mapped to a version constraint, e.g. `duckdb: ">=1.0"`.
#[derive(JsonSchema)]
#[schemars(inline, extend("additionalProperties" = true, "minProperties" = 1, "maxProperties" = 1))]
pub struct PinnedVersion {
    #[schemars(skip)]
    pub name: Located<String>,
    #[schemars(skip)]
    pub version: serde_json::Value,
}

impl<'de> Deserialize<'de> for PinnedVersion {
    /// One key, which isn't `name` (that's a [`PluginEntry`]).
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let m = Map::<serde_json::Value>::deserialize(d)?;
        match <[_; 1]>::try_from(m.0) {
            Ok([(name, version)]) if name.value != "name" => Ok(PinnedVersion { name, version }),
            _ => Err(serde::de::Error::custom("not a single `name: version` pair")),
        }
    }
}

/// A package with its source.
#[derive(Deserialize, JsonSchema)]
#[schemars(deny_unknown_fields, inline)]
pub struct PluginEntry {
    /// The package name: lowercase letters, digits and `_`.
    #[schemars(with = "String")]
    pub name: Loose<String>,
    /// A version constraint such as `1.2.0` or `>=1.0`. Not allowed with `local`.
    #[serde(default)]
    #[schemars(with = "String")]
    pub version: Option<serde_json::Value>,
    /// Install from the releases of this GitHub repository, `owner/repo`.
    #[serde(default)]
    #[schemars(with = "String")]
    pub github: Option<serde_json::Value>,
    /// Use the package folder at this path as it is.
    #[serde(default)]
    #[schemars(with = "String")]
    pub local: Option<serde_json::Value>,
    /// Install from this registry index (a URL or a path) instead of the default one.
    #[serde(default)]
    #[schemars(with = "String")]
    pub registry: Option<serde_json::Value>,
    #[serde(rename = "$unknown", default)]
    #[schemars(skip)]
    pub unknown: UnknownKeys,
}

/// A macro package, from git or from a folder.
#[derive(Deserialize, JsonSchema)]
#[schemars(rename = "package", deny_unknown_fields)]
#[schemars(extend("oneOf" = [{"required": ["git", "revision"]}, {"required": ["local"]}]))]
pub struct Package {
    /// The git URL of the package. Needs `revision`.
    pub git: Option<Loose<String>>,
    /// A tag, branch or commit of the git package.
    pub revision: Option<Loose<String>>,
    /// The path of a package folder in this project's tree.
    pub local: Option<Loose<String>>,
    /// The registry name of a package: not available yet, read only to say so.
    #[schemars(skip)]
    pub package: Option<Loose<String>>,
}

/// Package names mapped to version constraints.
#[derive(JsonSchema)]
#[schemars(inline, extend("additionalProperties" = true))]
#[allow(dead_code)]
struct VersionMap {}

/// The plugin packages this project uses. DRE installs them on demand into `dre_deps/` and pins them in `dre.lock`. May be written in any project YAML file; `dependencies.yml` is the usual place.
#[derive(JsonSchema)]
#[schemars(inline, extend("x-doc-type" = "list of plugin packages: a name, `name: \"<version>\"`, or a map (see below)"))]
#[allow(dead_code)]
struct Plugins(OneOf<Vec<Item>, OneOf<VersionMap, ()>>);

/// One plugin package: just its name, a `name: "<version>"` pair, or a map with `name` and where to get it.
#[derive(JsonSchema)]
#[schemars(inline)]
#[allow(dead_code)]
struct Item(PluginItem);

/// The schema of `plugins:`, wherever it's written.
pub(super) fn plugins(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
    g.subschema_for::<Plugins>()
}
