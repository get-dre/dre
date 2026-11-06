//! Source-owned SQL parsing and read-only policy, transported by `describe`.
use serde::{Deserialize, Serialize};
use sqlparser::{
    ast::Statement,
    dialect::{DatabricksDialect, DuckDbDialect, GenericDialect, PostgreSqlDialect},
    parser::{Parser, ParserError},
};

/// The parser grammar used to validate a query before execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SqlParser {
    Generic,
    Duckdb,
    Postgres,
    Databricks,
}

impl SqlParser {
    /// Parse SQL using the grammar selected by a source, independently of its installed name.
    pub fn parse(self, sql: &str) -> Result<Vec<Statement>, ParserError> {
        let dialect: Box<dyn sqlparser::dialect::Dialect> = match self {
            Self::Generic => Box::new(GenericDialect {}),
            Self::Duckdb => Box::new(DuckDbDialect {}),
            Self::Postgres => Box::new(PostgreSqlDialect {}),
            Self::Databricks => Box::new(DatabricksDialect {}),
        };
        Parser::parse_sql(dialect.as_ref(), sql)
    }
}

/// How unquoted identifiers match stored names.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum IdentifierCase {
    Insensitive,
    LowerUnlessQuoted,
    UpperUnlessQuoted,
}

/// Source-owned rules for `dre query` and catalogue consumers.
/// All safety fields are required: a missing or unknown policy fails closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SqlDialect {
    pub parser: SqlParser,
    pub identifier_quote: String,
    pub identifier_case: IdentifierCase,
    /// `doubling` or `hex`; hex requires exactly one `{hex}` in `hex_literal`.
    pub literal_style: String,
    pub hex_literal: Option<String>,
    pub denied_functions: Vec<String>,
    /// Formats which accept backtick-quoted file paths in place of a relation.
    pub file_formats: Vec<String>,
    /// When no read-only session exists, the database permission boundary to show the user.
    pub read_only_notice: Option<String>,
}
impl SqlDialect {
    /// Reject incomplete policies rather than assuming SQL safety.
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.identifier_quote.as_str(), "\"" | "`")
            || self
                .denied_functions
                .iter()
                .chain(&self.file_formats)
                .any(|s| s.is_empty() || !s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
            || self
                .read_only_notice
                .as_ref()
                .is_some_and(|s| s.trim().is_empty())
            || match self.literal_style.as_str() {
                "doubling" => self.hex_literal.is_some(),
                "hex" => self
                    .hex_literal
                    .as_ref()
                    .is_none_or(|s| s.matches("{hex}").count() != 1),
                _ => true,
            }
        {
            return Err("source supplied invalid sql_dialect metadata".into());
        }
        Ok(())
    }
    /// Encode a value without exposing its quotes or backslashes to a hex dialect's lexer.
    pub fn literal(&self, value: &str) -> String {
        if let Some(template) = &self.hex_literal {
            let hex: String = value.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
            template.replace("{hex}", &hex)
        } else {
            format!("'{}'", value.replace('\'', "''"))
        }
    }
    pub fn same_name(&self, written: &str, actual: &str) -> bool {
        match self.identifier_case {
            IdentifierCase::Insensitive => written.eq_ignore_ascii_case(actual),
            IdentifierCase::LowerUnlessQuoted => written == actual,
            IdentifierCase::UpperUnlessQuoted => written.to_ascii_uppercase() == actual,
        }
    }
    pub fn denies(&self, function: &str) -> bool {
        let name = function
            .rsplit('.')
            .next()
            .unwrap_or(function)
            .trim_matches(['"', '`']);
        self.denied_functions.iter().any(|f| f.eq_ignore_ascii_case(name))
    }
    /// The stable `dre/catalog/v1` dialect contract.
    pub fn describe(&self) -> serde_json::Value {
        serde_json::json!({"identifier_quote": self.identifier_quote, "identifier_case": self.identifier_case,
            "literal_style": self.literal_style, "hex_literal": self.hex_literal})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn policy() -> serde_json::Value {
        json!({"parser":"postgres", "identifier_quote":"\"", "identifier_case":"lower_unless_quoted", "literal_style":"hex", "hex_literal":"convert_from(decode('{hex}', 'hex'), 'UTF8')", "denied_functions":[], "file_formats":[], "read_only_notice":null})
    }

    #[test]
    fn a_source_can_explicitly_choose_generic_sql_grammar() {
        let mut value = policy();
        value["parser"] = json!("generic");
        let dialect: SqlDialect = serde_json::from_value(value).unwrap();
        dialect.validate().unwrap();
        assert_eq!(dialect.parser.parse("select lower('ok')").unwrap().len(), 1);
    }

    #[test]
    fn explicit_empty_policy_differs_from_missing_policy() {
        let value = policy();
        let dialect: SqlDialect = serde_json::from_value(value.clone()).unwrap();
        dialect.validate().unwrap();
        assert!(!dialect.denies("lower"));
        for field in [
            "parser",
            "identifier_quote",
            "identifier_case",
            "literal_style",
            "denied_functions",
            "file_formats",
        ] {
            let mut incomplete = value.clone();
            incomplete.as_object_mut().unwrap().remove(field);
            assert!(
                serde_json::from_value::<SqlDialect>(incomplete).is_err(),
                "{field}"
            );
        }
        let mut unknown = value;
        unknown["parser"] = json!("unknown");
        assert!(serde_json::from_value::<SqlDialect>(unknown).is_err());
    }

    #[test]
    fn invalid_policies_fail_closed() {
        for (field, value) in [
            ("identifier_quote", json!("")),
            ("literal_style", json!("backslash")),
            ("hex_literal", json!("decode('plain')")),
            ("denied_functions", json!([""])),
            ("file_formats", json!(["a.b"])),
        ] {
            let mut invalid = policy();
            invalid[field] = value;
            assert!(
                serde_json::from_value::<SqlDialect>(invalid)
                    .unwrap()
                    .validate()
                    .is_err(),
                "{field}"
            );
        }
    }

    #[test]
    fn hex_literals_keep_quotes_backslashes_and_unicode_out_of_sql() {
        let mut dialect: SqlDialect = serde_json::from_value(policy()).unwrap();
        assert_eq!(
            dialect.literal("é'\\"),
            "convert_from(decode('c3a9275c', 'hex'), 'UTF8')"
        );
        dialect.denied_functions = vec!["pg_read_file".into()];
        assert!(dialect.denies("pg_catalog.\"PG_READ_FILE\""));
        assert!(!dialect.denies("lower"));
    }
}
