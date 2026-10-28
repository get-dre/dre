//! What DRE knows about each source type's SQL: how to quote identifiers and string literals,
//! how unquoted names compare, and which functions reach outside the database. One table, so
//! `dre catalog`, `dre query` and the integrations reading the catalogue agree.
//!
//! A type missing here fails closed: no catalogue, and no literal an integration could build.

use serde::Serialize;
use serde_json::{Value, json};

/// How a string value becomes a SQL literal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LiteralStyle {
    /// `'…'` with each `'` doubled; the dialect has no backslash escapes in literals.
    Doubling,
    /// The UTF-8 bytes as hex, decoded by this expression (`{hex}` stands for the digits): no
    /// quote or backslash of the value ever reaches the dialect's lexer.
    Hex(&'static str),
}

/// How an identifier written without quotes matches a stored name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentifierCase {
    /// Names compare case-insensitively, quoted or not.
    Insensitive,
    /// Unquoted names fold to lowercase; quoted names are exact.
    LowerUnlessQuoted,
}

#[derive(Debug, Clone, Copy)]
pub struct Dialect {
    pub identifier_quote: char,
    pub literal_style: LiteralStyle,
    pub identifier_case: IdentifierCase,
    /// Functions that read files, run programs or reach the network; `dre query` refuses them.
    pub denied_functions: &'static [&'static str],
}

const DUCKDB: Dialect = Dialect {
    identifier_quote: '"',
    literal_style: LiteralStyle::Doubling,
    identifier_case: IdentifierCase::Insensitive,
    denied_functions: &[
        "read_text",
        "read_blob",
        "read_csv",
        "read_csv_auto",
        "read_json",
        "read_json_auto",
        "read_json_objects",
        "read_json_objects_auto",
        "read_ndjson",
        "read_ndjson_auto",
        "read_ndjson_objects",
        "read_parquet",
        "parquet_scan",
        "parquet_metadata",
        "parquet_schema",
        "parquet_file_metadata",
        "parquet_kv_metadata",
        "read_xlsx",
        "sniff_csv",
        "glob",
        "delta_scan",
        "iceberg_scan",
        "iceberg_metadata",
        "iceberg_snapshots",
        "sqlite_scan",
        "sqlite_attach",
        "postgres_scan",
        "postgres_query",
        "postgres_attach",
        "mysql_query",
        "query",
        "query_table",
        "getenv",
        "duckdb_secrets",
        "duckdb_settings",
        "which_secret",
    ],
};

const POSTGRES: Dialect = Dialect {
    identifier_quote: '"',
    literal_style: LiteralStyle::Hex("convert_from(decode('{hex}', 'hex'), 'UTF8')"),
    identifier_case: IdentifierCase::LowerUnlessQuoted,
    denied_functions: &[
        "pg_read_file",
        "pg_read_binary_file",
        "pg_ls_dir",
        "pg_ls_logdir",
        "pg_ls_waldir",
        "pg_ls_tmpdir",
        "pg_ls_archive_statusdir",
        "pg_stat_file",
        "lo_import",
        "lo_export",
        "lo_get",
        "lo_open",
        "loread",
        "dblink",
        "dblink_exec",
        "dblink_connect",
        "dblink_send_query",
        "pg_execute_server_program",
        // Run SQL passed as a string, past this guard.
        "query_to_xml",
        "query_to_xml_and_xmlschema",
        "query_to_xmlschema",
        "cursor_to_xml",
        "cursor_to_xmlschema",
        "table_to_xml",
        "table_to_xml_and_xmlschema",
        "database_to_xml",
        "schema_to_xml",
        "current_setting",
        "pg_reload_conf",
        "pg_terminate_backend",
        "pg_cancel_backend",
    ],
};

const DATABRICKS: Dialect = Dialect {
    identifier_quote: '`',
    literal_style: LiteralStyle::Hex("decode(unhex('{hex}'), 'UTF-8')"),
    identifier_case: IdentifierCase::Insensitive,
    denied_functions: &[
        "read_files",
        "read_kafka",
        "read_kinesis",
        "read_pubsub",
        "read_pulsar",
        "read_state_metadata",
        "read_statestore",
        "cloud_files_state",
        "http_request",
        "java_method",
        "reflect",
        "secret",
        "list_secrets",
        "ai_query",
    ],
};

/// The dialect of a source type, or `None` when DRE doesn't know its quoting rules.
pub fn of(kind: &str) -> Option<&'static Dialect> {
    match kind {
        "duckdb" => Some(&DUCKDB),
        "postgres" => Some(&POSTGRES),
        "databricks" => Some(&DATABRICKS),
        _ => None,
    }
}

impl Dialect {
    /// `value` as a SQL string literal.
    pub fn literal(&self, value: &str) -> String {
        match self.literal_style {
            LiteralStyle::Hex(template) => template.replace("{hex}", &crate::checksum::hex(value.as_bytes())),
            LiteralStyle::Doubling => format!("'{}'", value.replace('\'', "''")),
        }
    }

    /// Whether an unquoted identifier `written` names the stored `actual`.
    pub fn same_name(&self, written: &str, actual: &str) -> bool {
        match self.identifier_case {
            IdentifierCase::Insensitive => written.eq_ignore_ascii_case(actual),
            IdentifierCase::LowerUnlessQuoted => written == actual,
        }
    }

    /// Whether `function` (any case, optionally schema-qualified) reaches outside the database.
    pub fn denies(&self, function: &str) -> bool {
        let name = function
            .rsplit('.')
            .next()
            .unwrap_or(function)
            .trim_matches(['"', '`']);
        self.denied_functions.iter().any(|f| f.eq_ignore_ascii_case(name))
    }

    /// The rules as `dre catalog` reports them.
    pub fn describe(&self) -> Value {
        json!({
            "identifier_quote": self.identifier_quote.to_string(),
            "identifier_case": self.identifier_case,
            "literal_style": match self.literal_style {
                LiteralStyle::Doubling => "doubling",
                LiteralStyle::Hex(_) => "hex",
            },
            "hex_literal": match self.literal_style {
                LiteralStyle::Hex(template) => Some(template),
                LiteralStyle::Doubling => None,
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_never_pass_a_quote_or_backslash_to_hex_dialects() {
        let value = "x' or '1'='1\\";
        assert_eq!(of("duckdb").unwrap().literal(value), "'x'' or ''1''=''1\\'");
        let pg = of("postgres").unwrap().literal(value);
        assert!(pg.starts_with("convert_from(decode('78") && !pg.contains(" or "));
        assert_eq!(
            of("databricks").unwrap().literal("é"),
            "decode(unhex('c3a9'), 'UTF-8')"
        );
    }

    #[test]
    fn unknown_types_have_no_dialect() {
        assert!(of("snowflake").is_none());
    }

    #[test]
    fn denied_functions_match_any_case_and_schema() {
        let pg = of("postgres").unwrap();
        assert!(pg.denies("PG_READ_FILE") && pg.denies("pg_catalog.pg_read_file"));
        // Functions that run SQL given as a string would carry a denied call past the guard.
        assert!(pg.denies("query_to_xml") && of("duckdb").unwrap().denies("query"));
        assert!(!pg.denies("lower"));
    }
}
