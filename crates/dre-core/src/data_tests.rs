//! Query-level checks and their structured outcomes. SQL definitions live in project macros.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One named declaration's arguments and behavior.
#[derive(Debug, Clone, Default, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    /// Keyword arguments passed to the test macro. Local tests receive relation automatically.
    #[serde(default)]
    pub arguments: Map<String, Value>,
    /// Earlier saved query results in this Binding, keyed by macro argument. Local tests only; hidden and empty datasets are supported.
    #[serde(default)]
    pub query_results: BTreeMap<String, String>,
    /// Execution and severity settings; defaults to local execution and error severity.
    #[serde(default)]
    pub config: Config,
    #[serde(rename = "$unknown", default, skip_serializing)]
    #[schemars(skip)]
    pub unknown: crate::config::de::UnknownKeys,
}

/// Data-test execution and severity options.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize, JsonSchema)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// The test engine: `local` checks saved rows; `source` checks an explicit relation.
    #[schemars(regex(pattern = "^(local|source)$"))]
    pub execution: String,
    /// Data-failure severity. `warn` records bad data without failing the Binding.
    #[schemars(regex(pattern = "^(error|warn)$"))]
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
    pub query_results: BTreeMap<String, String>,
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
    for argument in settings.arguments.keys().chain(settings.query_results.keys()) {
        if !identifier(argument) || (argument == "relation" && settings.config.execution != "source") {
            return Err(format!(
                "`{argument}` is not a valid test argument; `relation` is supplied by DRE"
            ));
        }
    }
    if settings.query_results.contains_key("relation") {
        return Err("`relation` is supplied by DRE; query_results must name another macro argument".into());
    }
    if let Some(argument) = settings
        .query_results
        .keys()
        .find(|key| settings.arguments.contains_key(*key))
    {
        return Err(format!(
            "`{argument}` is supplied by both arguments and query_results"
        ));
    }
    if settings.config.execution != "local" && !settings.query_results.is_empty() {
        return Err(
            "query_results is available only for local tests; saved results are not uploaded to sources"
                .into(),
        );
    }
    if let Some(key) = settings.unknown.0.first() {
        return Err(format!(
            "unsupported data test key `{}`; use arguments, query_results and config",
            key.name
        ));
    }
    if let Some(key) = settings.config.unknown.0.first() {
        return Err(format!(
            "unsupported data test config key `{}`; this version supports execution: local or source and severity: error or warn",
            key.name
        ));
    }
    if !matches!(settings.config.execution.as_str(), "local" | "source")
        || !matches!(settings.config.severity.as_str(), "error" | "warn")
    {
        return Err("this version supports execution: local or source and severity: error or warn".into());
    }
    if settings.config.execution == "source"
        && !settings
            .arguments
            .get("relation")
            .and_then(Value::as_str)
            .is_some_and(|r| !r.trim().is_empty())
    {
        return Err("execution: source requires an explicit nonempty string arguments.relation".into());
    }
    Ok(DataTest {
        name: name.clone(),
        arguments: settings.arguments.clone(),
        config: settings.config.clone(),
        query_results: settings.query_results.clone(),
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
            .chain(self.query_results.keys())
            .filter(|key| key.as_str() != "relation")
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
    Warn,
    Error,
}

pub(crate) fn schema(g: &mut schemars::SchemaGenerator) -> schemars::Schema {
    let settings = g.subschema_for::<Settings>();
    schemars::json_schema!({
        "type": "array",
        "description": "Checks of saved local results or explicit source relations. Each entry names one test block or explicit test_ macro, optionally qualified by package, with arguments, config, and optional query_results mapping macro arguments to earlier query names in the same Binding; run by dre test and dre build, never dre run.",
        "items": {"type": "object", "minProperties": 1, "maxProperties": 1,
                  "propertyNames": {"pattern": "^[A-Za-z_][A-Za-z0-9_]*(\\.[A-Za-z_][A-Za-z0-9_]*)?$"},
                  "additionalProperties": settings}
    })
}
