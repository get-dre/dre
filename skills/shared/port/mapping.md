# Mapping reference: SAS, SSRS, Power BI paginated and Crystal to DRE

Read the part for the report's tool, then the shared parts (parameters, layout, delivery). Where
a row says "ask", the old report doesn't decide it: ask the user, with the recommendation given.
The practice IDs are in `practices.md`.

## SAS reporting (`.sas`)

Read `sas.md` before translating: dependency inventory, bounded mappings, Oracle/SQL Server
semantics and completion evidence live there. Use `examples/sas-overdue.md` for a worked
migration from unexecuted reference source to runnable DRE projects. CSV/XLSX outputs are
optional; `parity.md` covers supplied-reference checks and no-reference validation.

## SSRS and Power BI paginated (`.rdl`, `.rdlc`)

Power BI paginated reports and Report Builder use the same RDL format as SSRS; everything here
applies to all three.

| RDL | DRE |
|---|---|
| `DataSource` (embedded `ConnectString`, or a shared `DataSourceReference`) | a connection in `profiles.yml` (server and database only; the password through `env_var()`), named once and used by every ported report; the project's `default_profile` when there's one |
| `DataSet` with `CommandType` Text | a `.sql` file in the report folder; `@Param` becomes `{{ var('param') }}` |
| `DataSet` with `CommandType` StoredProcedure | the procedure's SELECT logic, translated, in `.sql` files (temp tables in a `tab: false` setup file, REP-3). Ask for the procedure's source (`sp_helptext`, or the DBA). On Postgres, a set-returning function can be called as `select * from schema.fn(...)` |
| `SharedDataSet` | ask for the shared dataset's `.rsd` file (its query is inside), then as a DataSet |
| dataset `Filters` | a `WHERE` (or `HAVING`) in the SQL |
| `Fields` with `DataField` | the query's column aliases; keep the field names readable (`as region_name`) |
| calculated fields (`Field` with a `Value` expression) | a column in the SQL |
| `ReportParameter` | a variable: a default under `vars:` and a `var_schema` entry (see Parameters) |
| `Tablix` (table, list) | a tab of an xlsx output (one `.sql` per tab, REP-1), or a table in a pdf template |
| `Tablix` row groups with group footers (subtotals) | `GROUP BY ROLLUP (...)` with a label column; or a pdf template that prints a heading and a subtotal row per group (see Layout) |
| `Tablix` column groups (a matrix) | pivot in SQL with `sum(case when ... then ... end)` per known column; ask when the columns come from the data |
| `Textbox` expressions | SQL columns (see Expressions), or Jinja in a pdf template for text around the data |
| `NoRowsMessage` | `on_empty`, or an empty-state block in a pdf template (`topics/no-data.md`) |
| `PageHeader`, `PageFooter`, `Globals!PageNumber`, `Globals!TotalPages` | a pdf template's `@page` margin boxes: `@bottom-center { content: "Page " counter(page) " of " counter(pages); }` |
| `PageBreak` on a group (`BreakLocation`) | `break-before: page` in the template's CSS, on the group's heading |
| `Subreport` | another query of the same report, shown as a section of the template; or a separate report when it has its own audience |
| `Chart`, `GaugePanel`, `Map` | DRE draws no charts. Ask: drop it, keep the table under it, or draw a simple bar chart as inline `<svg>` in a pdf template (a Jinja loop over the rows) |
| `Image` (embedded or external) | an image file in the project, `<img src="logo.png">` in the template |
| `Code` (custom VB), `CodeModules` | rewrite as SQL. Logic that builds SQL can be a macro in `macros/` |
| interactive sort, drill-through (`Drillthrough`), toggles (`Visibility` with `ToggleItem`), document map | no equivalent: DRE makes files. Ask whether a second tab or a second report covers it |
| `Language`, number formats (`Format` property: `C2`, `N0`, `d`) | column formats in the output (`#,##0.00`, `dd/mm/yyyy`) and `locale:` (REP-5) |
| report execution snapshots, caching | nothing to port: each DRE run queries fresh, and its run folder keeps what it made |

### Expressions

| SSRS expression | DRE |
|---|---|
| `=Fields!Amount.Value` | the column `amount` |
| `=Parameters!Region.Value` | `{{ var('region') }}` in SQL; `var('region')` in a template |
| `=IIf(cond, a, b)`, `=Switch(c1, a, c2, b)`, `=Choose(...)` | `case when cond then a else b end` in SQL (PORT-1) |
| `=Sum(Fields!X.Value)` in a group footer | `ROLLUP` in SQL, or `total: sum` on the xlsx column for the grand total |
| `=Sum(...)` in the report footer only | `total: sum` (xlsx); a one-row summary query in a pdf (REP-10) |
| `=Count(...)`, `=CountDistinct(...)`, `=CountRows()` | `count(*)`, `count(distinct x)` in SQL |
| `=RunningValue(X, Sum, scope)` | `sum(x) over (partition by ... order by ...)` |
| `=Previous(Fields!X.Value)` | `lag(x) over (order by ...)` |
| `=RowNumber(scope)` | `row_number() over (partition by ... order by ...)` |
| `=Lookup(...)`, `=LookupSet(...)`, `=MultiLookup(...)` | a `join` in SQL (a lookup table for a mapping, REP-6) |
| `=Format(X, "C2")`, `=FormatCurrency(X)`, `=FormatNumber(X, 2)` | the column's `format: "$#,##0.00"` (xlsx), `row.fmt('x')` or `| currency('USD')` in a template |
| `=Format(D, "dd/MM/yyyy")`, `=FormatDateTime(D)` | `date_format: "dd/mm/yyyy"` (xlsx), `| date('dd/mm/yyyy')` in a template |
| `=Today()`, `=Now()`, `Globals!ExecutionTime` | `run.date`, `run.now` (pinned by `DRE_RUN_AT` on reruns) |
| `=DateAdd("m", -1, Today())`, first of last month | `run.date.prev_month.start.date`, `run.date.prev_month.end.date`, or `period('last_month')` |
| `Globals!ReportName` | `run.report` |
| `User!UserID`, `User!Language` | no equivalent: DRE runs unattended. Ask what it was used for (often a filter: make it a variable or a Set) |
| `=Join(Parameters!P.Value, ", ")` | `{{ var('p') | join(', ') }}` |
| `=Code.MyFunction(...)` | rewrite in SQL; ask for the code if the `.rdl` doesn't hold it |

## Crystal Reports (XML or text exports)

| Crystal | DRE |
|---|---|
| Database tables and their links (`TableLinks`: Equal, LeftOuter...) | a `.sql` with the joins (`join`, `left join`) |
| a "SQL Command" table | its SQL in a `.sql` file; `{?Param}` becomes `{{ var('param') }}` |
| record selection formula | `WHERE` |
| group selection formula | `HAVING` (or a filter over an aggregating subquery) |
| `{@formula}` fields | SQL expressions (PORT-1). Crystal syntax: `if ... then ... else` is `case when`; `ToText(x, 2)` is a column format; `DateDiff("d", a, b)` is the warehouse's date difference |
| running totals | window functions |
| summaries per group (`Sum ({x}, {group})`) | `ROLLUP` in SQL, or per-group subtotal rows in a pdf template |
| `{?Parameter}` fields | variables with `var_schema` (see Parameters); `AllowMultipleValue` is an `array` |
| groups (`Group` on a field), "Keep group together", "New page after" | the group column in SQL; `break-inside: avoid` and `break-before: page` in a pdf template |
| sections (Report/Page Header, Group Header, Details, Group Footer, Page Footer), "Suppress" | blocks of the pdf template; a suppress formula is an `{% if %}` |
| subreports | another query of the report, as a template section |
| shared variables (`Shared NumberVar`) | SQL; a value carried between sections becomes a column or a summary query |
| Crystal Server/BusinessObjects schedules, "publications" with bursting per recipient | `schedules.yml`, and `for_each` for a burst (`topics/per-row.md`) |
| export formats (PDF, Excel, CSV) | the output's `format` |

## Parameters

| Source type | `var_schema` type | Note |
|---|---|---|
| String, `StringParameter` | `string` | constrain it: `allowed`, or `pattern` and `max_length` |
| Integer, `NumberParameter` (whole numbers) | `integer` | |
| Float, `CurrencyParameter` | `number` | |
| DateTime, `DateParameter`, `DateTimeParameter` | `date` | a time of day is rare in report parameters; ask |
| Boolean, `BooleanParameter` | `boolean` | |
| multi-value, `AllowMultipleValue` | `array` | `allowed` on an `array` lists whole lists, not items: `allowed: [[APAC], [APAC, EMEA]]`. Usually leave it out and escape each item in SQL (below) |
| available values from a list | `enum` with `allowed` | |
| available values from a query | ask: an `allowed` list now, or a lookup table (REP-6) when it changes |
| a default of `Today()`, last month, a fiscal period | no fixed default: `run.date`, `period()` (REP-2) |

```yaml
vars:
  regions: [NORTH, SOUTH, EAST, WEST]
var_schema:
  regions:
    type: array
    description: Region codes to include (NORTH, SOUTH, EAST, WEST)
  as_at:
    type: date
    description: The day balances are taken
```

DRE checks an `array` variable's `allowed` against the whole list, not each item, so it can't
say "any of these codes". Write each item into the SQL as an escaped literal instead, and filter
through the lookup or dimension table so an unknown code matches nothing:

```sql
where r.region_code in (
  {%- for c in var('regions') %}'{{ c | replace("'", "''") }}'{% if not loop.last %}, {% endif %}{% endfor -%}
)
```

On Databricks escape with a backslash instead (see the templates docs, "String literals and
Databricks"). A free-form array also keeps a report from being exposed to AI agents (AI-6); a
few fixed selections can instead be Sets (REP-4).

## T-SQL to the warehouse

Oracle and SQL Server can remain the source through JDBC (`plugins/source-jdbc.md`).
Keep native T-SQL when the target is SQL Server; review its report-date defaults, permissions
and temporary preparation. For Oracle use the reviewed dialect patterns in `sas.md` and the
overdue-loan example. When moving to another warehouse, translate using this table:

| T-SQL | Postgres, DuckDB | Databricks | Snowflake | BigQuery | ClickHouse |
|---|---|---|---|---|---|
| `TOP 10` | `limit 10` | `limit 10` | `limit 10` | `limit 10` | `limit 10` |
| `ISNULL(a, b)` | `coalesce(a, b)` | `coalesce` | `coalesce` | `coalesce` | `coalesce` |
| `GETDATE()` | `{{ run.now }}` or `run.date` (REP-2) | same | same | same | same |
| `DATEADD(day, n, d)` | `d + interval 'n days'` | `date_add(d, n)` | `dateadd(day, n, d)` | `date_add(d, interval n day)` | `d + interval n day` |
| `DATEDIFF(day, a, b)` | `b - a` (dates) | `datediff(b, a)` | `datediff(day, a, b)` | `date_diff(b, a, day)` | `dateDiff('day', a, b)` |
| `'a' + 'b'` | `'a' \|\| 'b'` | `concat('a', 'b')` | `'a' \|\| 'b'` | `concat('a', 'b')` | `concat('a', 'b')` |
| `[Order Date]` | `"Order Date"` | `` `Order Date` `` | `"Order Date"` | `` `Order Date` `` | `` `Order Date` `` |
| `#temp` tables | `create temp table` in a `tab: false` file | `create temporary view` | `create temporary table` | a CTE, or a temp table in a script | a CTE |
| `WITH (NOLOCK)` | drop it | drop it | drop it | drop it | drop it |
| `CONVERT(varchar, d, 103)`, `FORMAT(...)` | keep the value typed; format in the output (REP-5) | same | same | same | same |
| `CAST(x AS money)` | `numeric(19, 4)` | `decimal(19, 4)` | `number(19, 4)` | `numeric` | `Decimal(19, 4)` |

Check names and types against the warehouse (`dre catalog`, or a query the user approves): a
replicated table is often renamed or lower-cased.

## Layout

- **A plain table:** one xlsx tab, or csv for a system.
- **Groups with subtotals, in xlsx:** one query with `ROLLUP` and a label, so each subtotal is a
  row:

  ```sql
  select
    case when grouping(region_name) = 1 then 'Total'
         when grouping(store_name) = 1 then region_name || ' total'
         else region_name end as region,
    case when grouping(store_name) = 1 then null else store_name end as store,
    sum(amount) as amount
  from invoices
  group by rollup (region_name, store_name)
  order by grouping(region_name), region_name, grouping(store_name), store_name
  ```

  Bold the subtotal rows only in a template; otherwise say they're plain rows.
- **Groups with subtotals, in pdf:** the detail rows plus a subtotal query (REP-10), and a
  template that loops the detail and prints a heading when the group changes:

  ```html
  {% set ns = namespace(group=none) %}
  {% for row in results.invoices.rows %}
    {% if row.region_name != ns.group %}
      {% set ns.group = row.region_name %}
      <h2>{{ row.region_name }}</h2>
    {% endif %}
    ...
  {% endfor %}
  ```
- **A matrix:** pivot in SQL for known columns; when the columns come from the data, ask
  (xlsx tabs per value via Sets, REP-4, or a long table).
- **Page setup, headers, footers, page numbers, breaks:** CSS in a pdf template
  (`topics/documents.md`).

## Delivery

| Old | DRE |
|---|---|
| an email subscription (To, Cc, Subject, render format, "include report" / "include link") | an `email` destination entry with `to`, `cc`, `subject`; the format as the output's `format`; a link becomes a file destination plus a message with `outputs.<name>.location` |
| a file share subscription (`\\server\share`) | ask: an `sftp` destination, an object store, or the built-in `local` destination on a mounted share |
| a schedule: "Monthly on day 1 at 06:00", "Weekly on Monday at 7:00" | `cron: "0 6 1 * *"`, `cron: "0 7 * * MON"`; ordinal days ("2nd Tuesday", "last weekday") as `rrule`, with `at` |
| a shared schedule used by several reports | a timing in `timings.yml`, referred to by each schedule (SCH-2) |
| the report server's time zone | each schedule's `timezone` (SCH-1); ask, recommending the server's |
| a data-driven subscription (a query lists recipients, each with its own parameter values) | `for_each`: the subscription's query becomes the driver (one row per recipient, with an opaque key, REP-12); the report's queries return **every** recipient's rows with that key column, instead of filtering on the per-recipient parameter; the address is `to: "{{ each.email }}"` |
| one report per customer, each with its own settings | Sets (REP-4) |
| "send only if there's data" | `on_empty: skip`, or `when:` on a message |

Start the first real deliveries held or on `dev` (PORT-4), and keep the old subscription on until
a parallel cycle matches (PORT-2).
