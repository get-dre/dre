//! Query-level checks and their structured outcomes. SQL definitions live in project macros.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One named declaration's arguments and behavior.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Keyword arguments passed to the test macro. DRE supplies relation automatically.
    #[serde(default)]
    pub arguments: Map<String, Value>,
    /// Execution and severity settings; defaults to local execution and error severity.
    #[serde(default)]
    pub config: Config,
    #[serde(rename = "$unknown", default, skip_serializing)]
    #[schemars(skip)]
    pub unknown: crate::config::de::UnknownKeys,
}

/// Initial local-test options. Unsupported execution modes fail rather than being ignored.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// The test engine. Currently only `local` is supported.
    #[schemars(regex(pattern = "^local$"))]
    pub execution: String,
    /// Data-failure severity. Currently only `error` is supported.
    #[schemars(regex(pattern = "^error$"))]
    pub severity: String,
    #[serde(rename = "$unknown", default, skip_serializing)]
    #[schemars(skip)]
    pub unknown: crate::config::de::UnknownKeys,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            execution: "local".into(),
            severity: "error".into(),
            unknown: Default::default(),
        }
    }
}

/// A resolved declaration, including its name rather than the YAML's single-key map.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct DataTest {
    pub name: String,
    pub arguments: Map<String, Value>,
    pub config: Config,
}

/// Resolve a single-key test declaration and reject options this implementation cannot run.
pub(crate) fn resolve(entry: &BTreeMap<String, Settings>) -> Result<DataTest, String> {
    if entry.len() != 1 {
        return Err("each entry must name exactly one test".into());
    }
    let (name, settings) = entry.first_key_value().expect("one entry was checked above");
    if !name.split('.').all(identifier) || name.split('.').count() > 2 {
        return Err(format!(
            "`{name}` must be a macro name, optionally qualified by its package"
        ));
    }
    for argument in settings.arguments.keys() {
        if !identifier(argument) || argument == "relation" {
            return Err(format!(
                "`{argument}` is not a valid test argument; `relation` is supplied by DRE"
            ));
        }
    }
    if let Some(key) = settings.unknown.0.first() {
        return Err(format!(
            "unsupported data test key `{}`; use arguments and config",
            key.name
        ));
    }
    if let Some(key) = settings.config.unknown.0.first() {
        return Err(format!(
            "unsupported data test config key `{}`; this version supports only execution: local and severity: error",
            key.name
        ));
    }
    if settings.config.execution != "local" || settings.config.severity != "error" {
        return Err("this version supports only execution: local and severity: error".into());
    }
    Ok(DataTest {
        name: name.clone(),
        arguments: settings.arguments.clone(),
        config: settings.config.clone(),
    })
}

fn identifier(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

impl DataTest {
    pub(crate) fn macro_name(&self) -> String {
        match self.name.split_once('.') {
            Some((package, name)) => format!("{package}.test_{name}"),
            None => format!("test_{}", self.name),
        }
    }

    /// Call a checked macro with values passed through template context, never interpolated SQL.
    pub(crate) fn invocation(&self) -> String {
        let args: Vec<String> = self
            .arguments
            .keys()
            .map(|key| format!("{key}=__dre_arguments.{key}"))
            .collect();
        format!(
            "{{{{ {}(relation=__dre_relation{}) }}}}",
            self.macro_name(),
            if args.is_empty() {
                String::new()
            } else {
                format!(", {}", args.join(", "))
            }
        )
    }
}

/// One test's outcome, without retained failure-row values.
#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Outcome {
    pub query: String,
    pub test: String,
    pub execution: String,
    pub severity: String,
    pub status: Status,
    /// Full number of failure rows; absent for execution errors.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failures: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub duration_ms: u64,
}

/// A successful check, a data failure, or a check which could not execute.
#[derive(Debug, Clone, Copy, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Pass,
    Fail,
    Error,
}

pub(crate) fn schema(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let settings = g.subschema_for::<Settings>();
    schemars::json_schema!({
        "type": "array",
        "description": "Local checks of the saved result. Each entry names one test block or explicit test_ macro, optionally qualified by package, with arguments and config; run by dre test, never dre run.",
        "items": {"type": "object", "minProperties": 1, "maxProperties": 1,
                  "propertyNames": {"pattern": "^[A-Za-z_][A-Za-z0-9_]*(\\.[A-Za-z_][A-Za-z0-9_]*)?$"},
                  "additionalProperties": settings}
    })
}
