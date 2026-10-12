---
title: "AI-ready reports"
description: "Turn reviewed report logic and upstream documentation into trustworthy context for AI."
section: build-reports
position: 6
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
queries: [revenue]
```

`var_schema` declares types, defaults, allowed values and descriptions. It is separate from
legacy `vars` values, which remain supported. Supported types are string, integer, number,
boolean, date, array, object and enum. Dates use `YYYY-MM-DD`; use `allowed` to constrain an enum.
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

`dre mcp serve` forwards to the separately installed `dre-mcp serve` executable. Core owns a
generic extension installer; companion implementation remains separately licensed.
Declare an optional exact-version companion under `extensions:` in `dependencies.yml`:

```yaml
extensions:
  - name: dre-mcp
    version: "0.1.0-rc.1"
    # Set only after the authorised owner reads and accepts this EULA version.
    # Treat changes as a configuration PR; installation/update never rewrites it.
    accept_license: "2026-10"
```

`dre deps` installs declared companions separately from plugins. Ordinary report commands do
not install them. Without an acceptance policy, proprietary installation needs explicit
`--accept-proprietary`; that download acknowledgement does not replace the companion's EULA.
A missing MCP binary can be downloaded at launch when the project declares a reviewed policy;
otherwise interactive setup offers installation and unattended launch prints setup instructions.

Read the terms with `dre mcp license show`. Individual setup can use
`dre mcp license accept --version 2026-10`, then `dre mcp serve`. For approved automation,
a pinned `accept_license` applies the owner's declaration without a startup prompt. New terms
reject a stale project value even if the local user accepted them separately. Review the new
EULA and edit that field manually through your repository's review process. No update changes
project YAML or accepts future versions. A local receipt records terms version, SHA-256 and time;
it does not certify PR approval or actual reading. The RC has no automatic expiry.

The companion distribution remains in local development; the EULA draft must be completed
before release. Package installation becomes available once an owner-selected registry exists.

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

The companion is a separately licensed extension. This preview is being built and tested before
its distribution channel is chosen. Team transport, authentication and a hosted service are
outside the personal RC.

