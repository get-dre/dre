use dre_protocol::sql::{IdentifierCase, SqlDialect, SqlParser};

pub fn dialect() -> SqlDialect {
    SqlDialect {
        parser: SqlParser::Postgres,
        identifier_quote: "\"".into(),
        identifier_case: IdentifierCase::LowerUnlessQuoted,
        literal_style: "hex".into(),
        hex_literal: Some("convert_from(decode('{hex}', 'hex'), 'UTF8')".into()),
        denied_functions: [
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
    tables: "(select current_database() as table_catalog, n.nspname as table_schema, c.relname as table_name, obj_description(c.oid, 'pg_class') as description from pg_class c join pg_namespace n on n.oid=c.relnamespace where c.relkind in ('r','v','m','f','p')) t",
    columns: "select a.attname, format_type(a.atttypid,a.atttypmod), col_description(c.oid,a.attnum) from pg_attribute a join pg_class c on c.oid=a.attrelid join pg_namespace n on n.oid=c.relnamespace where current_database()={catalog} and n.nspname={schema} and c.relname={table} and a.attnum>0 and not a.attisdropped order by a.attnum",
    resolve: Some(
        "select current_database(), ns.nspname, cl.relname from pg_class cl join pg_namespace ns on ns.oid=cl.relnamespace where cl.oid=to_regclass({name})",
    ),
};
