//! Live relation context for integrations. Descriptions are read from the database;
//! DRE does not create or maintain upstream comments.

use crate::{
    dialect::{Dialect, IdentifierCase},
    profiles::{Entry, Role},
    project::Project,
};
use serde::Deserialize;
use serde_json::{Value, json};
use sqlparser::{
    ast::{SetExpr, Statement, TableFactor},
    dialect::GenericDialect,
    parser::Parser,
};

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    #[serde(default)]
    pub tables: Vec<String>,
    #[serde(default)]
    pub schemas: Vec<String>,
}

fn parts(name: &str, dialect: &Dialect) -> Result<Vec<String>, String> {
    let statements = Parser::parse_sql(&GenericDialect {}, &format!("select * from {name}"))
        .map_err(|e| format!("invalid relation `{name}`: {e}"))?;
    let [Statement::Query(query)] = statements.as_slice() else {
        return Err("expected a relation name".into());
    };
    let SetExpr::Select(select) = query.body.as_ref() else {
        return Err("expected a relation name".into());
    };
    let [from] = select.from.as_slice() else {
        return Err(format!("`{name}` must be a relation name, not SQL"));
    };
    if !from.joins.is_empty() || select.selection.is_some() || query.limit_clause.is_some() {
        return Err(format!("`{name}` must be a relation name, not SQL"));
    }
    let TableFactor::Table {
        name: object,
        alias: None,
        args: None,
        ..
    } = &from.relation
    else {
        return Err(format!(
            "`{name}` must be a relation name, not a function or alias"
        ));
    };
    if query.to_string() != format!("SELECT * FROM {}", from.relation) {
        return Err(format!("`{name}` must contain only a relation name"));
    }
    object
        .0
        .iter()
        .map(|part| {
            part.as_ident()
                .map(|id| {
                    if dialect.identifier_case == IdentifierCase::LowerUnlessQuoted
                        && id.quote_style.is_none()
                    {
                        id.value.to_ascii_lowercase()
                    } else {
                        id.value.clone()
                    }
                })
                .ok_or_else(|| "relation identifiers must be literal".into())
        })
        .collect()
}

fn rows(project: &Project, profile: &str, sql: &str) -> Result<Vec<Value>, String> {
    let value = crate::query::execute(project, profile, sql, 100_000)?;
    if value["truncated"] == true {
        return Err("catalogue exceeds 100000 rows; narrow the requested schema".into());
    }
    Ok(value["rows"].as_array().cloned().unwrap_or_default())
}

pub fn build(project: &Project, profile: &str, request: &Request) -> Result<Value, String> {
    let Entry::Use(target) = project.profiles.entry(Role::Connection, profile) else {
        return Err(format!("connection `{profile}` is unavailable"));
    };
    let unsupported = || {
        format!(
            "live catalogue comments are not yet supported for `{}`; supported: duckdb, postgres, databricks",
            target.kind
        )
    };
    let dialect = crate::dialect::of(&target.kind).ok_or_else(unsupported)?;
    let lit = |value: &str| dialect.literal(value);
    let defaults = match target.kind.as_str() {
        "duckdb" | "postgres" => "select current_database(), current_schema()",
        "databricks" => "select current_catalog(), current_schema()",
        _ => return Err(unsupported()),
    };
    let defaults = rows(project, profile, defaults)?;
    let first = defaults.first();
    let catalog = first
        .and_then(|r| r[0].as_str())
        .ok_or("the connection returned no current catalog")?;
    let schema = first
        .and_then(|r| r[1].as_str())
        .ok_or("the connection returned no current schema")?;
    let mut wanted = Vec::new();
    for name in &request.tables {
        let p = parts(name, dialect)?;
        let (c, s, n) = match p.as_slice() {
            [n] => (catalog.to_owned(), schema.to_owned(), n.clone()),
            [s, n] => (catalog.to_owned(), s.clone(), n.clone()),
            [c, s, n] => (c.clone(), s.clone(), n.clone()),
            _ => return Err(format!("`{name}` needs one to three name parts")),
        };
        let (c, s, n) = if target.kind == "postgres" {
            if c != catalog {
                return Err("Postgres does not support cross-database relation context".into());
            }
            // Let PostgreSQL resolve search_path and quoted names instead of guessing the schema.
            let resolved = rows(
                project,
                profile,
                &format!(
                    "select current_database(), ns.nspname, cl.relname from pg_class cl join pg_namespace ns on ns.oid=cl.relnamespace where cl.oid=to_regclass({})",
                    lit(name)
                ),
            )?;
            let row = resolved
                .first()
                .ok_or_else(|| format!("table `{name}` is missing or not visible"))?;
            (
                row[0].as_str().ok_or("catalog missing")?.to_owned(),
                row[1].as_str().ok_or("schema missing")?.to_owned(),
                row[2].as_str().ok_or("name missing")?.to_owned(),
            )
        } else {
            (c, s, n)
        };
        wanted.push((name.clone(), c, s, n));
    }
    let mut filters = Vec::new();
    let comparison = |column: &str, value: &str| match dialect.identifier_case {
        IdentifierCase::Insensitive => format!("lower({column})=lower({})", lit(value)),
        IdentifierCase::LowerUnlessQuoted => format!("{column}={}", lit(value)),
    };
    let equal = |left: &str, right: &str| dialect.same_name(left, right);
    for (_, c, s, n) in &wanted {
        filters.push(format!(
            "({} and {} and {})",
            comparison("table_catalog", c),
            comparison("table_schema", s),
            comparison("table_name", n)
        ));
    }
    for name in &request.schemas {
        let p = parts(name, dialect)?;
        let (c, s) = match p.as_slice() {
            [s] => (catalog, s.as_str()),
            [c, s] => (c.as_str(), s.as_str()),
            _ => return Err(format!("schema `{name}` needs one or two name parts")),
        };
        filters.push(format!(
            "({} and {})",
            comparison("table_catalog", c),
            comparison("table_schema", s)
        ));
    }
    let tables_view = match target.kind.as_str() {
        "duckdb" => "(select database_name as table_catalog, schema_name as table_schema, table_name, comment as description from duckdb_tables() union all select database_name, schema_name, view_name, comment from duckdb_views()) t".to_owned(),
        "postgres" => "(select current_database() as table_catalog, n.nspname as table_schema, c.relname as table_name, obj_description(c.oid, 'pg_class') as description from pg_class c join pg_namespace n on n.oid=c.relnamespace where c.relkind in ('r','v','m','f','p')) t".to_owned(),
        "databricks" => "(select table_catalog, table_schema, table_name, comment as description from system.information_schema.tables) t".to_owned(),
        _ => return Err(unsupported()),
    };
    let filter = if filters.is_empty() {
        "false".into()
    } else {
        filters.join(" or ")
    };
    let tables = rows(
        project,
        profile,
        &format!(
            "select table_catalog, table_schema, table_name, description from {tables_view} where {filter} order by table_catalog, table_schema, table_name"
        ),
    )?;
    let mut out = Vec::new();
    for table in tables {
        let c = table[0].as_str().ok_or("catalog is missing")?;
        let s = table[1].as_str().ok_or("schema is missing")?;
        let n = table[2].as_str().ok_or("table name is missing")?;
        let sql = match target.kind.as_str() {
            "duckdb" => format!(
                "select column_name, data_type, comment from duckdb_columns() where database_name={} and schema_name={} and table_name={} order by column_index",
                lit(c),
                lit(s),
                lit(n)
            ),
            "postgres" => format!(
                "select a.attname, format_type(a.atttypid,a.atttypmod), col_description(c.oid,a.attnum) from pg_attribute a join pg_class c on c.oid=a.attrelid join pg_namespace n on n.oid=c.relnamespace where current_database()={} and n.nspname={} and c.relname={} and a.attnum>0 and not a.attisdropped order by a.attnum",
                lit(c),
                lit(s),
                lit(n)
            ),
            "databricks" => format!(
                "select column_name, full_data_type, comment from system.information_schema.columns where table_catalog={} and table_schema={} and table_name={} order by ordinal_position",
                lit(c),
                lit(s),
                lit(n)
            ),
            _ => return Err(unsupported()),
        };
        let columns: Vec<Value> = rows(project, profile, &sql)?
            .iter()
            .map(|r| json!({"name":r[0], "type":r[1], "description":r[2]}))
            .collect();
        let requested: Vec<_> = wanted
            .iter()
            .filter(|(_, wc, ws, wn)| equal(wc, c) && equal(ws, s) && equal(wn, n))
            .map(|(name, _, _, _)| name.clone())
            .collect();
        out.push(json!({"catalog":c,"schema":s,"name":n,"qualified_name":[c,s,n],"description":table[3], "description_source":"database", "columns":columns,"requested":requested}));
    }
    for (name, c, s, n) in &wanted {
        if !out.iter().any(|t| {
            equal(t["catalog"].as_str().unwrap_or(""), c)
                && equal(t["schema"].as_str().unwrap_or(""), s)
                && equal(t["name"].as_str().unwrap_or(""), n)
        }) {
            return Err(format!(
                "table `{name}` resolved to {c}.{s}.{n}, but is missing or not visible on connection `{profile}`"
            ));
        }
    }
    Ok(
        json!({"schema_version":"dre/catalog/v1","connection":profile,"target":project.target_name,
        "source_type":target.kind,"identifier_quote":dialect.identifier_quote.to_string(),"dialect":dialect.describe(),
        "current_catalog":catalog,"current_schema":schema,"tables":out}),
    )
}
