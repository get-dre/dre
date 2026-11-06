use dre_protocol::sql::{IdentifierCase, SqlDialect, SqlParser};

pub fn dialect() -> SqlDialect {
    SqlDialect {
        parser: SqlParser::Duckdb,
        identifier_quote: "\"".into(),
        identifier_case: IdentifierCase::Insensitive,
        literal_style: "doubling".into(),
        hex_literal: None,
        denied_functions: [
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
        ]
        .into_iter()
        .map(String::from)
        .collect(),
        file_formats: Vec::new(),
        read_only_notice: None,
    }
}

pub const CATALOG: dre_protocol::catalog::SqlCatalog = dre_protocol::catalog::SqlCatalog {
    defaults: "select current_database(), current_schema()",
    tables: "(select database_name as table_catalog, schema_name as table_schema, table_name, comment as description from duckdb_tables() union all select database_name, schema_name, view_name, comment from duckdb_views()) t",
    columns: "select column_name, data_type, comment from duckdb_columns() where database_name={catalog} and schema_name={schema} and table_name={table} order by column_index",
    resolve: None,
};
