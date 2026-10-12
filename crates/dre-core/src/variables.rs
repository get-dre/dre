//! Optional variable declarations, separate from `vars` so existing map values keep their meaning.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

pub type Definitions = BTreeMap<String, Definition>;

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    /// The value's type. Dates are real calendar dates in YYYY-MM-DD form.
    #[serde(rename = "type")]
    pub kind: VariableType,
    /// Used when no project, folder, report, Set, schedule or CLI value supplies this variable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// Optional exact values accepted for this variable; required for the enum type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed: Option<Vec<Value>>,
    /// What this input means, including its units or reporting period.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether execution requires a value. A default satisfies this requirement.
    #[serde(default)]
    pub required: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum VariableType {
    String,
    #[serde(alias = "int")]
    Integer,
    Number,
    Boolean,
    Date,
    #[serde(alias = "list")]
    Array,
    #[serde(alias = "map")]
    Object,
    Enum,
}

impl Definition {
    pub fn check(&self, name: &str, value: &Value) -> Result<(), String> {
        let valid = match self.kind {
            VariableType::String => value.is_string(),
            VariableType::Integer => value.as_i64().is_some() || value.as_u64().is_some(),
            VariableType::Number => value.is_number(),
            VariableType::Boolean => value.is_boolean(),
            VariableType::Date => value
                .as_str()
                .is_some_and(|s| s.len() == 10 && chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()),
            VariableType::Array => value.is_array(),
            VariableType::Object => value.is_object(),
            VariableType::Enum => !value.is_null() && self.allowed.as_ref().is_some_and(|v| !v.is_empty()),
        };
        if !valid {
            return Err(format!(
                "variable `{name}` must have type {}",
                serde_json::to_value(&self.kind).unwrap().as_str().unwrap()
            ));
        }
        if self
            .allowed
            .as_ref()
            .is_some_and(|allowed| !allowed.contains(value))
        {
            return Err(format!("variable `{name}` is not one of its allowed values"));
        }
        Ok(())
    }
}

pub fn check_values(
    definitions: &Definitions,
    vars: &Map<String, Value>,
    cli: &BTreeMap<String, String>,
    required: bool,
) -> Vec<String> {
    let mut errors = Vec::new();
    for (name, definition) in definitions {
        let value = cli
            .get(name)
            .map(|v| crate::render::cli_var_value(v))
            .or_else(|| vars.get(name).cloned());
        match value {
            Some(value) => {
                if let Err(error) = definition.check(name, &value) {
                    errors.push(error);
                }
            }
            None if required && definition.required => errors.push(format!(
                "required variable `{name}` is missing; pass --var {name}=VALUE"
            )),
            None => {}
        }
    }
    errors
}
