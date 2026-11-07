//! Shared catalogue assembly; plugins supply SQL, name resolution and dialect rules.
use crate::sql::{IdentifierCase, SqlDialect};
use serde_json::{Value, json};
use sqlparser::{
    ast::{SetExpr, Statement, TableFactor},
    dialect::GenericDialect,
    parser::Parser,
};

type Wanted = (String, String, String, String);
type Query<'a> = dyn FnMut(&str) -> Result<Vec<Value>, String> + 'a;

/// Vendor SQL owned by one source plugin, never selected by its installed name.
pub struct SqlCatalog {
    pub defaults: &'static str,
    pub tables: &'static str,
    /// Column query with SQL literal placeholders `{catalog}`, `{schema}`, `{table}`.
    pub columns: &'static str,
    /// Optional native name resolution query, with the SQL literal placeholder `{name}`.
    pub resolve: Option<&'static str>,
}
impl SqlCatalog {
    /// Discover comments and columns with the source's native query adapter.
    pub fn build(
        &self,
        dialect: &SqlDialect,
        tables: &[String],
        schemas: &[String],
        query: &mut Query<'_>,
    ) -> Result<Value, String> {
        dialect.validate()?;
        if tables.is_empty() && schemas.is_empty() {
            return Err("catalog request needs at least one table or schema".into());
        }
        let request = Request { tables, schemas };
        let defaults = checked_rows(query, self.defaults, 2)?;
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
            .map(|name| resolve_table(self.resolve, dialect, name, (catalog, schema), query))
            .collect::<Result<Vec<Wanted>, String>>()?;
        let filter = filter(dialect, &wanted, request.schemas, catalog)?;
        let view = self.tables;
        let tables = checked_rows(
            query,
            &format!(
                "select table_catalog, table_schema, table_name, description from {view} where {filter} order by table_catalog, table_schema, table_name"
            ),
            4,
        )?;
        let mut out = Vec::new();
        for table in tables {
            let c = table[0].as_str().ok_or("catalog is missing")?;
            let s = table[1].as_str().ok_or("schema is missing")?;
            let n = table[2].as_str().ok_or("table name is missing")?;
            let sql = column_sql(self.columns, dialect, c, s, n);
            let columns: Vec<Value> = checked_rows(query, &sql, 3)?
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
        check_found(dialect, &wanted, &out, "source")?;
        Ok(
            json!({"schema_version":"dre/catalog/v1", "identifier_quote":dialect.identifier_quote,
        "dialect":dialect.describe(), "current_catalog":catalog,"current_schema":schema,"tables":out}),
        )
    }
}
struct Request<'a> {
    tables: &'a [String],
    schemas: &'a [String],
}
fn checked_rows(query: &mut Query<'_>, sql: &str, width: usize) -> Result<Vec<Value>, String> {
    let rows = query(sql)?;
    if rows.len() >= 100_000 {
        return Err("catalogue exceeds 100000 rows; narrow the requested schema".into());
    }
    if rows.iter().any(|r| r.as_array().is_none_or(|r| r.len() < width)) {
        return Err("source returned incomplete catalogue rows".into());
    }
    Ok(rows)
}
fn parts(name: &str, dialect: &SqlDialect) -> Result<Vec<String>, String> {
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
                .map(|id| match (dialect.identifier_case, id.quote_style) {
                    (IdentifierCase::LowerUnlessQuoted, None) => id.value.to_ascii_lowercase(),
                    (IdentifierCase::UpperUnlessQuoted, None) => id.value.to_ascii_uppercase(),
                    _ => id.value.clone(),
                })
                .ok_or_else(|| "relation identifiers must be literal".into())
        })
        .collect()
}

fn resolve_table(
    resolve_sql: Option<&str>,
    dialect: &SqlDialect,
    name: &str,
    (catalog, schema): (&str, &str),
    query: &mut Query<'_>,
) -> Result<Wanted, String> {
    let p = parts(name, dialect)?;
    let (c, s, n) = match p.as_slice() {
        [n] => (catalog.to_owned(), schema.to_owned(), n.clone()),
        [s, n] => (catalog.to_owned(), s.clone(), n.clone()),
        [c, s, n] => (c.clone(), s.clone(), n.clone()),
        _ => return Err(format!("`{name}` needs one to three name parts")),
    };
    if resolve_sql.is_none() {
        return Ok((name.to_string(), c, s, n));
    }
    if c != catalog {
        return Err("this source does not support cross-database relation context".into());
    }
    let resolved = checked_rows(
        query,
        &resolve_sql
            .expect("checked above")
            .replace("{name}", &dialect.literal(name)),
        3,
    )?;
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

fn filter(
    dialect: &SqlDialect,
    wanted: &[Wanted],
    schemas: &[String],
    catalog: &str,
) -> Result<String, String> {
    let comparison = |column: &str, value: &str| match dialect.identifier_case {
        IdentifierCase::Insensitive => format!("lower({column})=lower({})", dialect.literal(value)),
        IdentifierCase::LowerUnlessQuoted | IdentifierCase::UpperUnlessQuoted => {
            format!("{column}={}", dialect.literal(value))
        }
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

fn check_found(
    dialect: &SqlDialect,
    wanted: &[Wanted],
    found: &[Value],
    profile: &str,
) -> Result<(), String> {
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

// Replace placeholders from the template only: relation names may themselves contain braces.
fn column_sql(template: &str, dialect: &SqlDialect, catalog: &str, schema: &str, table: &str) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        let Some((placeholder, value)) = [("{catalog}", catalog), ("{schema}", schema), ("{table}", table)]
            .into_iter()
            .find(|(placeholder, _)| rest.starts_with(placeholder))
        else {
            out.push('{');
            rest = &rest[1..];
            continue;
        };
        out.push_str(&dialect.literal(value));
        rest = &rest[placeholder.len()..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dialect() -> SqlDialect {
        SqlDialect {
            parser: crate::sql::SqlParser::Duckdb,
            identifier_quote: "\"".into(),
            identifier_case: IdentifierCase::Insensitive,
            literal_style: "doubling".into(),
            hex_literal: None,
            denied_functions: vec![],
            file_formats: vec![],
            read_only_notice: None,
        }
    }

    #[test]
    fn resolves_names_and_rejects_sql_in_identifiers() {
        let mut no_query = |_: &str| -> Result<Vec<Value>, String> { panic!("unexpected database query") };
        let table = resolve_table(
            None,
            &dialect(),
            "sales.orders",
            ("memory", "main"),
            &mut no_query,
        )
        .unwrap();
        assert_eq!(
            table,
            (
                "sales.orders".into(),
                "memory".into(),
                "sales".into(),
                "orders".into()
            )
        );
        for name in [
            "a.b.c.d",
            "orders where 1=1",
            "orders; select 1",
            "orders join private on true",
            "read_csv('private')",
        ] {
            assert!(
                resolve_table(None, &dialect(), name, ("memory", "main"), &mut no_query).is_err(),
                "{name}"
            );
        }
    }

    #[test]
    fn uppercase_dialects_fold_only_unquoted_names() {
        let mut dialect = dialect();
        dialect.identifier_case = IdentifierCase::UpperUnlessQuoted;
        assert_eq!(
            parts("shop.sales.orders", &dialect).unwrap(),
            ["SHOP", "SALES", "ORDERS"]
        );
        assert_eq!(
            parts("\"shop\".\"Sales\".\"orders\"", &dialect).unwrap(),
            ["shop", "Sales", "orders"]
        );
        let filters = filter(&dialect, &[], &["sales".into(), "\"Sales\"".into()], "SHOP").unwrap();
        assert!(filters.contains("table_schema='SALES'"), "{filters}");
        assert!(filters.contains("table_schema='Sales'"), "{filters}");
        let wanted = vec![(
            "\"orders\"".into(),
            "SHOP".into(),
            "SALES".into(),
            "orders".into(),
        )];
        let found = vec![json!({"catalog":"SHOP", "schema":"SALES", "name":"orders"})];
        check_found(&dialect, &wanted, &found, "warehouse").unwrap();
        let wrong_case = vec![json!({"catalog":"SHOP", "schema":"SALES", "name":"ORDERS"})];
        assert!(check_found(&dialect, &wanted, &wrong_case, "warehouse").is_err());
    }

    #[test]
    fn native_resolution_cannot_cross_databases() {
        let mut query = |_: &str| Ok(vec![json!(["shop", "public", "orders"])]);
        let table = resolve_table(
            Some("resolve({name})"),
            &dialect(),
            "orders",
            ("shop", "main"),
            &mut query,
        )
        .unwrap();
        assert_eq!(table.2, "public");
        assert!(
            resolve_table(
                Some("resolve({name})"),
                &dialect(),
                "other.public.orders",
                ("shop", "main"),
                &mut query
            )
            .unwrap_err()
            .contains("cross-database")
        );
    }

    #[test]
    fn literals_and_placeholder_like_names_remain_data() {
        assert_eq!(
            column_sql(
                "c={catalog} and s={schema} and t={table}",
                &dialect(),
                "{schema}",
                "s'\\",
                "t"
            ),
            "c='{schema}' and s='s''\\' and t='t'"
        );
        let filters = filter(&dialect(), &[], &["\"x' or 1=1\"".into()], "memory").unwrap();
        assert!(filters.contains("'x'' or 1=1'"), "{filters}");
    }

    #[test]
    fn catalog_rejects_missing_objects_and_incomplete_rows() {
        let wanted = vec![("orders".into(), "memory".into(), "main".into(), "orders".into())];
        assert!(
            check_found(&dialect(), &wanted, &[], "warehouse")
                .unwrap_err()
                .contains("memory.main.orders")
        );
        assert!(checked_rows(&mut |_| Ok(vec![json!(["only one"])]), "select", 2).is_err());
    }
}
