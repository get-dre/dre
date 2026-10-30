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

fn rows(ctx: &crate::Context, project: &Project, profile: &str, sql: &str) -> Result<Vec<Value>, String> {
    let value = crate::query::execute(ctx, project, profile, sql, 100_000)?;
    if value["truncated"] == true {
        return Err("catalogue exceeds 100000 rows; narrow the requested schema".into());
    }
    Ok(value["rows"].as_array().cloned().unwrap_or_default())
}

/// One requested table, resolved: what was asked, and its catalog, schema and name.
type Wanted = (String, String, String, String);

/// The live catalogue of the requested tables and schemas on connection `profile`: each
/// table's description and columns, as the database has them.
pub fn build(
    ctx: &crate::Context,
    project: &Project,
    profile: &str,
    request: &Request,
) -> Result<Value, String> {
    let Entry::Use(target) = project.profiles().entry(Role::Connection, profile) else {
        return Err(format!("connection `{profile}` is unavailable"));
    };
    let kind = target.kind.as_str();
    let unsupported = || {
        format!(
            "live catalogue comments are not yet supported for `{kind}`; supported: duckdb, postgres, databricks"
        )
    };
    let dialect = crate::dialect::of(kind).ok_or_else(unsupported)?;
    let query = |sql: &str| rows(ctx, project, profile, sql);
    let defaults = query(defaults_sql(kind).ok_or_else(unsupported)?)?;
    let first = defaults.first();
    let catalog = first
        .and_then(|r| r[0].as_str())
        .ok_or("the connection returned no current catalog")?;
    let schema = first
        .and_then(|r| r[1].as_str())
        .ok_or("the connection returned no current schema")?;
    let wanted = request
        .tables
        .iter()
        .map(|name| resolve_table(kind, dialect, name, (catalog, schema), &query))
        .collect::<Result<Vec<Wanted>, String>>()?;
    let filter = filter(dialect, &wanted, &request.schemas, catalog)?;
    let view = tables_view(kind).ok_or_else(unsupported)?;
    let tables = query(&format!(
        "select table_catalog, table_schema, table_name, description from {view} where {filter} order by table_catalog, table_schema, table_name"
    ))?;
    let mut out = Vec::new();
    for table in tables {
        let c = table[0].as_str().ok_or("catalog is missing")?;
        let s = table[1].as_str().ok_or("schema is missing")?;
        let n = table[2].as_str().ok_or("table name is missing")?;
        let sql = columns_sql(kind, dialect, (c, s, n)).ok_or_else(unsupported)?;
        let columns: Vec<Value> = query(&sql)?
            .iter()
            .map(|r| json!({"name":r[0], "type":r[1], "description":r[2]}))
            .collect();
        let requested: Vec<_> = wanted
            .iter()
            .filter(|(_, wc, ws, wn)| {
                dialect.same_name(wc, c) && dialect.same_name(ws, s) && dialect.same_name(wn, n)
            })
            .map(|(name, _, _, _)| name.clone())
            .collect();
        out.push(json!({"catalog":c,"schema":s,"name":n,"qualified_name":[c,s,n],"description":table[3], "description_source":"database", "columns":columns,"requested":requested}));
    }
    check_found(dialect, &wanted, &out, profile)?;
    Ok(
        json!({"schema_version":"dre/catalog/v1","connection":profile,"target":project.target_name(),
        "source_type":kind,"identifier_quote":dialect.identifier_quote.to_string(),"dialect":dialect.describe(),
        "current_catalog":catalog,"current_schema":schema,"tables":out}),
    )
}

/// The SQL that asks a connection for its current catalog and schema.
fn defaults_sql(kind: &str) -> Option<&'static str> {
    match kind {
        "duckdb" | "postgres" => Some("select current_database(), current_schema()"),
        "databricks" => Some("select current_catalog(), current_schema()"),
        _ => None,
    }
}

/// A requested table's catalog, schema and name, the current ones filling in what it leaves
/// out. PostgreSQL resolves it itself (its `search_path` and quoted names).
fn resolve_table(
    kind: &str,
    dialect: &Dialect,
    name: &str,
    (catalog, schema): (&str, &str),
    query: &dyn Fn(&str) -> Result<Vec<Value>, String>,
) -> Result<Wanted, String> {
    let p = parts(name, dialect)?;
    let (c, s, n) = match p.as_slice() {
        [n] => (catalog.to_owned(), schema.to_owned(), n.clone()),
        [s, n] => (catalog.to_owned(), s.clone(), n.clone()),
        [c, s, n] => (c.clone(), s.clone(), n.clone()),
        _ => return Err(format!("`{name}` needs one to three name parts")),
    };
    if kind != "postgres" {
        return Ok((name.to_string(), c, s, n));
    }
    if c != catalog {
        return Err("Postgres does not support cross-database relation context".into());
    }
    let resolved = query(&format!(
        "select current_database(), ns.nspname, cl.relname from pg_class cl join pg_namespace ns on ns.oid=cl.relnamespace where cl.oid=to_regclass({})",
        dialect.literal(name)
    ))?;
    let row = resolved
        .first()
        .ok_or_else(|| format!("table `{name}` is missing or not visible"))?;
    Ok((
        name.to_string(),
        row[0].as_str().ok_or("catalog missing")?.to_owned(),
        row[1].as_str().ok_or("schema missing")?.to_owned(),
        row[2].as_str().ok_or("name missing")?.to_owned(),
    ))
}

/// The `where` clause picking the requested tables and schemas (`false` for none).
fn filter(dialect: &Dialect, wanted: &[Wanted], schemas: &[String], catalog: &str) -> Result<String, String> {
    let comparison = |column: &str, value: &str| match dialect.identifier_case {
        IdentifierCase::Insensitive => format!("lower({column})=lower({})", dialect.literal(value)),
        IdentifierCase::LowerUnlessQuoted => format!("{column}={}", dialect.literal(value)),
    };
    let mut filters = Vec::new();
    for (_, c, s, n) in wanted {
        filters.push(format!(
            "({} and {} and {})",
            comparison("table_catalog", c),
            comparison("table_schema", s),
            comparison("table_name", n)
        ));
    }
    for name in schemas {
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
    Ok(if filters.is_empty() {
        "false".into()
    } else {
        filters.join(" or ")
    })
}

/// Every table and view with its description, as `table_catalog`, `table_schema`,
/// `table_name` and `description`.
fn tables_view(kind: &str) -> Option<&'static str> {
    match kind {
        "duckdb" => Some(
            "(select database_name as table_catalog, schema_name as table_schema, table_name, comment as description from duckdb_tables() union all select database_name, schema_name, view_name, comment from duckdb_views()) t",
        ),
        "postgres" => Some(
            "(select current_database() as table_catalog, n.nspname as table_schema, c.relname as table_name, obj_description(c.oid, 'pg_class') as description from pg_class c join pg_namespace n on n.oid=c.relnamespace where c.relkind in ('r','v','m','f','p')) t",
        ),
        "databricks" => Some(
            "(select table_catalog, table_schema, table_name, comment as description from system.information_schema.tables) t",
        ),
        _ => None,
    }
}

/// The SQL listing one table's columns: name, type and comment, in order.
fn columns_sql(kind: &str, dialect: &Dialect, (c, s, n): (&str, &str, &str)) -> Option<String> {
    let lit = |v: &str| dialect.literal(v);
    Some(match kind {
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
        _ => return None,
    })
}

/// Every requested table must be among those found.
fn check_found(dialect: &Dialect, wanted: &[Wanted], found: &[Value], profile: &str) -> Result<(), String> {
    for (name, c, s, n) in wanted {
        let seen = found.iter().any(|t| {
            dialect.same_name(t["catalog"].as_str().unwrap_or(""), c)
                && dialect.same_name(t["schema"].as_str().unwrap_or(""), s)
                && dialect.same_name(t["name"].as_str().unwrap_or(""), n)
        });
        if !seen {
            return Err(format!(
                "table `{name}` resolved to {c}.{s}.{n}, but is missing or not visible on connection `{profile}`"
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn duckdb() -> &'static Dialect {
        crate::dialect::of("duckdb").unwrap()
    }

    fn no_query(_: &str) -> Result<Vec<Value>, String> {
        Err("no database here".into())
    }

    #[test]
    fn a_table_takes_the_current_catalog_and_schema_it_leaves_out() {
        let at = ("memory", "main");
        let t = resolve_table("duckdb", duckdb(), "orders", at, &no_query).unwrap();
        assert_eq!(
            t,
            ("orders".into(), "memory".into(), "main".into(), "orders".into())
        );
        let t = resolve_table("duckdb", duckdb(), "sales.orders", at, &no_query).unwrap();
        assert_eq!((t.1.as_str(), t.2.as_str()), ("memory", "sales"));
        assert!(resolve_table("duckdb", duckdb(), "a.b.c.d", at, &no_query).is_err());
    }

    #[test]
    fn postgres_resolves_a_table_itself_and_stays_in_its_database() {
        let pg = crate::dialect::of("postgres").unwrap();
        let query = |_: &str| Ok(vec![json!(["shop", "public", "orders"])]);
        let t = resolve_table("postgres", pg, "orders", ("shop", "public"), &query).unwrap();
        assert_eq!(t.2, "public");
        let other = resolve_table("postgres", pg, "other.public.orders", ("shop", "public"), &query);
        assert!(other.unwrap_err().contains("cross-database"));
    }

    #[test]
    fn the_filter_picks_each_table_and_schema_and_nothing_without_them() {
        let wanted = vec![(
            "orders".to_string(),
            "memory".into(),
            "main".into(),
            "orders".into(),
        )];
        let f = filter(duckdb(), &wanted, &["sales".to_string()], "memory").unwrap();
        assert_eq!(f.matches(" or ").count(), 1, "{f}");
        assert!(f.contains("'orders'") && f.contains("'sales'"), "{f}");
        assert_eq!(filter(duckdb(), &[], &[], "memory").unwrap(), "false");
    }

    #[test]
    fn only_supported_sources_have_catalogue_sql() {
        for kind in ["duckdb", "postgres", "databricks"] {
            assert!(
                defaults_sql(kind).is_some() && tables_view(kind).is_some(),
                "{kind}"
            );
            assert!(columns_sql(kind, duckdb(), ("c", "s", "t")).is_some(), "{kind}");
        }
        assert!(defaults_sql("clickhouse").is_none() && tables_view("clickhouse").is_none());
        assert!(columns_sql("clickhouse", duckdb(), ("c", "s", "t")).is_none());
    }

    #[test]
    fn a_requested_table_the_database_doesnt_have_is_an_error() {
        let wanted = vec![(
            "orders".to_string(),
            "memory".into(),
            "main".into(),
            "orders".into(),
        )];
        let found = vec![json!({"catalog": "memory", "schema": "main", "name": "orders"})];
        assert!(check_found(duckdb(), &wanted, &found, "warehouse").is_ok());
        let e = check_found(duckdb(), &wanted, &[], "warehouse").unwrap_err();
        assert!(
            e.contains("memory.main.orders") && e.contains("`warehouse`"),
            "{e}"
        );
    }
}
