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
    /// For `string` variables: a regular expression the whole value must match, e.g.
    /// `^[A-Z]{3}[0-9]{4}$` for a customer code. Anchors are implied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
    /// For `string` variables: the most characters a value may have.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_length: Option<u32>,
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
    /// Problems with the declaration itself: `pattern`/`max_length` on a non-string, or a
    /// pattern that isn't a valid regular expression.
    pub fn problems(&self, name: &str) -> Vec<String> {
        let mut problems = Vec::new();
        let string = matches!(self.kind, VariableType::String);
        for (key, set) in [
            ("pattern", self.pattern.is_some()),
            ("max_length", self.max_length.is_some()),
        ] {
            if set && !string {
                problems.push(format!("variable `{name}`: `{key}` applies only to type string"));
            }
        }
        if let Some(pattern) = &self.pattern
            && let Err(error) = full_match(pattern)
        {
            problems.push(format!("variable `{name}`: invalid `pattern`: {error}"));
        }
        problems
    }

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
                serde_json::to_value(&self.kind)
                    .unwrap()
                    .as_str()
                    .expect("a JSON value serializes")
            ));
        }
        if self
            .allowed
            .as_ref()
            .is_some_and(|allowed| !allowed.contains(value))
        {
            return Err(format!("variable `{name}` is not one of its allowed values"));
        }
        if let Some(text) = value.as_str() {
            if let Some(max) = self.max_length
                && text.chars().count() > max as usize
            {
                return Err(format!(
                    "variable `{name}` is longer than its max_length of {max}"
                ));
            }
            if let Some(pattern) = &self.pattern
                && !full_match(pattern).is_ok_and(|re| re.is_match(text))
            {
                return Err(format!(
                    "variable `{name}` does not match its pattern `{pattern}`"
                ));
            }
        }
        Ok(())
    }
}

/// `pattern` compiled to match a whole value.
fn full_match(pattern: &str) -> Result<regex::Regex, regex::Error> {
    regex::Regex::new(&format!("^(?:{pattern})$"))
}

/// Check the values a run will use: types, allowed values, patterns, and that every required
/// variable has one.
pub fn check_run_values(
    definitions: &Definitions,
    vars: &Map<String, Value>,
    cli: &BTreeMap<String, String>,
) -> Vec<String> {
    check(definitions, vars, cli, true)
}

/// Check the values that are set (by `validate`): a missing required value isn't an error yet,
/// the run may still be given one.
pub fn check_given_values(
    definitions: &Definitions,
    vars: &Map<String, Value>,
    cli: &BTreeMap<String, String>,
) -> Vec<String> {
    check(definitions, vars, cli, false)
}

fn check(
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn string(pattern: Option<&str>, max_length: Option<u32>) -> Definition {
        Definition {
            kind: VariableType::String,
            default: None,
            allowed: None,
            description: None,
            required: false,
            pattern: pattern.map(str::to_owned),
            max_length,
        }
    }

    #[test]
    fn pattern_matches_the_whole_value() {
        let code = string(Some("[A-Z]{3}[0-9]{2}"), None);
        assert!(code.check("code", &json!("ABC12")).is_ok());
        assert!(code.check("code", &json!("ABC12' or 1=1 --")).is_err());
        assert!(code.check("code", &json!("xABC12")).is_err());
        // An alternation can't escape the implied anchors.
        let either = string(Some("a|b"), None);
        assert!(either.check("v", &json!("ab")).is_err());
    }

    #[test]
    fn max_length_counts_characters() {
        let short = string(None, Some(3));
        assert!(short.check("v", &json!("éée")).is_ok());
        assert!(short.check("v", &json!("abcd")).is_err());
    }

    #[test]
    fn constraints_only_apply_to_strings_and_must_compile() {
        let mut number = string(Some("1"), Some(1));
        number.kind = VariableType::Number;
        assert_eq!(number.problems("n").len(), 2);
        assert_eq!(string(Some("("), None).problems("s").len(), 1);
    }
}
