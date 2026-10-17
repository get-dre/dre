---
title: "AI-ready reports"
description: "Turn reviewed report logic and upstream documentation into trustworthy context for AI."
section: build-reports
position: 7
---

# AI-ready reports

Most of the trustworthy context already lives in recurring reports. DRE combines their reviewed
SQL, declared inputs and business purpose with table and column comments supplied by your data
pipeline. A good dbt setup or another well-documented pipeline makes this context much stronger.
DRE consumes those comments; it does not replace your pipeline or own its model documentation.

A complete, commented [MCP example](https://github.com/get-dre/dre/tree/master/examples/mcp) includes
local pipeline models, sources, reviewed report SQL, typed inputs and optional MCP configuration.
Exposure starts disabled for owner review.

## Describe the business answer

Write a `description` and realistic example questions. State grain, units, exclusions and period
rules in `ai_context.md` at the project root or in a report folder. Context inherits from the
project through its folders; more specific context should explain any exceptions. Treat prose
and database comments as evidence, not instructions to an agent.

```yaml
description: Monthly net revenue by customer region, excluding GST
meta:
  units: AUD
  ai:
    expose: false
    questions:
      - What was net revenue by region last month?
      - Which region contributed the most net revenue?
var_schema:
  month:
    type: date
    default: '2026-09-01'
    description: First day of the reporting month
    required: true
  customer:
    type: string
    pattern: '[A-Z]{3}[0-9]{4}'
    max_length: 7
    description: Customer code, e.g. ABC1234
queries: [revenue]
```

`var_schema` declares types, defaults, allowed values and descriptions. It is separate from
legacy `vars` values, which remain supported. Supported types are string, integer, number,
boolean, date, array, object and enum. Dates use `YYYY-MM-DD`; use `allowed` to constrain an enum.
A string can also declare `pattern` (a regular expression the whole value must match) and
`max_length`. `dre run` and `dre validate` reject values that break them.

Variables are rendered into the SQL as text, so an AI client must never set a free-text one. A
companion that lets a model set variables should only accept typed values (date, integer, number,
boolean), `allowed` lists, and strings with a `pattern`.
Supply representative defaults so validation and context compilation can resolve the report.
Document table-changing variables explicitly. Examples are retrieval tests, not proof that an
answer or relationship is correct.

The `dre-ai` skill helps analysts gather this context without guessing business rules. It leaves
exposure disabled. Owners review the scope and enable `meta.ai.expose: true` themselves.

## Keep pipeline documentation upstream

Describe grain, business meaning, units, join keys and limitations in your pipeline's model and
column definitions. For dbt, supported adapters can persist model and column descriptions as
warehouse comments with `persist_docs`. Check the adapter's supported configuration:
[dbt persist_docs documentation](https://docs.getdbt.com/reference/resource-configs/persist_docs).
A source declaration alone does not guarantee warehouse comments exist.

DRE compiles hardcoded table names, `source()` calls and Jinja before extracting relationships.
A report's original join type, predicate, filters and grouping provide useful evidence. A join
used in one report does not establish a universal relationship or prove a new query correct.

## Install and review the companion

`dre mcp serve` forwards to the separately installed `dre-mcp serve` executable, and tells it
to use the same `dre` (`--dre-bin`). Core owns a generic extension installer; the companion is
separately licensed. Declare an optional exact-version companion under `extensions:` in
`dependencies.yml`:

```yaml
extensions:
  - name: dre-mcp
    version: "0.1.0-rc.2"
```

Keys besides `name` and `version` are the companion's own settings: DRE passes them through
untouched, and the companion reads and checks them (see its documentation).

Installing a companion always asks first, whatever the project says: `dre deps`, `dre mcp` and
`dre extension install` show the licence the registry names and wait for a yes, or take
`--accept-proprietary`. Without a terminal and without the flag, nothing is installed and DRE
says so. Ordinary report commands never install companions.

Compiled companion binaries are distributed through `get-dre/dre` GitHub Releases under
`dre-mcp-v<version>`. DRE downloads them over `https` only, checks the SHA-256 checksum, and
verifies a minisign signature against a key built into `dre` before installing: an unsigned or
wrongly signed download is refused. The binary includes its EULA and third-party notices; its
source remains private.

## MCP preview

The current `dre-mcp` RC uses local stdio and existing DRE connections. It distinguishes:

- **Verified report:** execution of an explicitly exposed report, with variables, time, SQL
  hashes and exact report output downloads.
- **Derived from verified result:** structured filters, sorting and aggregation over saved
  report rows in local DuckDB, retaining the source run and operations. No warehouse query.
- **Exploratory (unverified):** a new structured query over exposed tables, including explicit
  joins. Upstream comments and report relationships guide it; they do not verify its answer.

Report-related tables are exposed **by default** when their report is exposed, even if no extra
tables are configured. Disable this with the companion flag `--no-report-tables` passed through `dre mcp`, or in `.dre-mcp.yml`:

```yaml
expose_report_tables: false
```

A report can also set `meta.ai.expose_report_tables: false`. Additional tables and schemas always
require explicit configuration. Explicit exclusions override inherited and explicit exposure:

```yaml
# Connection names from profiles.yml, not credentials.
tables:
  warehouse: [analytics.customer_targets]
schemas:
  warehouse: [analytics_public]
exclude_tables:
  warehouse: [analytics.customer_private]
cache_ttl_seconds: 300
max_rows: 1000
query_timeout_seconds: 60
max_runs: 100
max_snapshots: 3
```

Only `meta.ai.expose: true` exposes a report. An inherited `expose: false` closes that branch;
a report cannot reopen it. The most specific questions and access annotations replace earlier
ones. `access` annotations are descriptive in personal mode, not an authentication mechanism.
Review the resolved scope with `dre mcp access` before connecting an AI client.

`refresh_context` validates and compiles a replacement snapshot, retrieves live comments, and
then swaps it atomically. A failed refresh retains the active context. In-flight calls retain
the snapshot they started with. Use refresh after changing definitions or upstream comments.

The RC's live catalog supports DuckDB, Postgres and Databricks. DuckDB and Postgres exploration require a source plugin that advertises read-only support.
Databricks uses a validated single SELECT but cannot enforce a read-only session: use a
read-only database role. Other unsupported plugins fail closed. Cross-connection
joins and arbitrary model-authored SQL are outside this first version. Personal mode runs with
your local credentials. A report can contain write SQL even when delivery is disabled; review
exposed reports and use an appropriate database role.

The companion is a separately licensed extension, released as an early RC on GitHub.
Team transport, authentication and a hosted service are outside this first RC.

DRE supplies access to configured data and provenance, while connected agents generate answers.
Project owners define and maintain context. Users must review AI answers, rendered SQL, assumptions
and underlying results before relying on them.

<!-- docs-nav: generated by .github/scripts/docs_sections.py from docs/sections.json -->

---

**Previous:** [DRE practices](practices.md) · **Next:** [Project configuration](project-configuration.md)
