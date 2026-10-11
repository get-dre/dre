---
title: "Build and run reports"
description: "Commands, the report YAML, profiles, dependencies and what a run does."
section: build-reports
position: 1
---

# Build and run reports

Use reports for repeatable exports with named queries, outputs and delivery settings.
Start with the [first report tutorial](first-report.md); the complete
[monthly finance example](https://github.com/get-dre/dre/blob/master/examples/monthly-finance/) adds a styled workbook and lookup data.
The [report reference](reference-report.md) lists keys, and the [glossary](glossary.md) defines
reports, Sets, Bindings and outputs. Shared defaults belong in [project configuration](project-configuration.md).

```bash
dre init                 # pick a source, enter its connection, optionally start a project
cd my_reports
dre validate             # check the project and compile its SQL
dre validate -s monthly  # ...and show where monthly's output would go
dre compile -s daily,monthly       # render the SQL into target/compiled/ and list the files
dre run                  # run every report; output lands in target/run/ (one folder per run)
dre run monthly --preview 50       # sample 50 rows, never delivered
dre run -s tag:regulatory --set all  # every regulatory report, for every Set
dre validate --live      # check every statement against the database without running it
dre validate --all-targets  # also check the settings of every profile entry, used or not
dre ls --schedule close_monthly    # list the Bindings a schedule runs (--output json for tools)
dre clean                # remove the target folder
dre system update        # update DRE itself
```

A report is a YAML file next to its `.sql` files:

```yaml
# reports/finance/monthly/monthly.yml
queries:
  - {query: setup_temp_accounts, tab: false}   # CREATE TEMP TABLE: runs first, no tab
  - {query: summary, tab_name: Summary}
  - detail
output:
  format: xlsx
  destination:
    profile: reports_s3
    path: "s3://reports/{{ var('client') }}/monthly-{{ run.date.yyyymmdd }}.xlsx"
sets: [client_a, client_b]
default_set: client_a
```

Queries run one after another in the order listed, each on its connection's session (one per
connection the report uses), so a temp table made by one is there for the next query on the same
connection. A query can run on its own connection with `profile:`, or by reading a
[source](sources.md) that names one; see [Connections and targets](connections.md). Each `.sql` file makes one tab (a sheet in xlsx, or one file
for csv, parquet and the other single-table formats), named by `tab_name` or else the file's
name, in the same order. The YAML decides the tabs, not the data:

- A file can hold several statements; the last one is the tab and the earlier ones prepare data.
  A second `SELECT` in a tab file is an error: give each tab its own `.sql` file.
- `tab: false` runs a file only for what it does (temp tables, `SET`s) and discards any result.
- A tab whose query returns no rows still appears, with its column names.
- A tab file whose last statement returns no result set at all is an error that points at
  `tab: false`.

The output file is named after the report (`monthly.xlsx`), or after the first destination's
`path`. `extension:` changes the extension for text formats that feed other systems, e.g. a bank
file that must end in `.aba`, or drops it with `extension: ""`:

```yaml
output:
  format: fixed_width
  extension: aba          # payments.aba instead of payments.txt; "" for no extension
  columns: [...]
```

`output:` can also be a list: the queries run once and each output is made from the same results,
from the queries its `queries:` names (all of them by default). A named output writes
`<name>.<ext>`, and a Set changes one output by giving its `name:`, or replaces the whole list.
File outputs are delivered first, then messages:

```yaml
output:
  - name: workbook
    format: xlsx
    queries: [summary, detail]
    destination: {profile: reports_s3, path: "s3://reports/monthly.xlsx"}
  - name: headline
    format: message          # a short headline built from the results; see Messages
    queries: [summary]
    destination: {profile: team_slack, channel: "#finance"}
```

`--output-name` and `--output-path` apply to the first output. See [messages](messages.md) for
message outputs, `when:` and `attach:`.

> **Changed in 0.2.** Connections are under `connections:` (was `sources:`). In 0.2.1 each
> profile has its own default `target:` again, and a missing entry is an error. See
> [Upgrading to 0.2](migrating-to-0.2.md).

Connections live in `profiles.yml`. DRE looks for it, in order, in `--profiles-dir`,
`DRE_PROFILES_DIR`, the project directory (next to `dre_project.yml`), and `~/.dre`, the same order
as dbt; `dre validate` and `dre run -v` say which file they used. Database connections go under
`connections:` and delivery targets under `destinations:`, each with one entry per target
(environment):

```yaml
connections:
  warehouse:
    targets:
      dev: {type: duckdb, path: dev.duckdb}
      prod: {type: postgres, host: db.internal, user: reports, password: "{{ env_var('PG_PASSWORD') }}"}
destinations:
  reports_s3:
    targets:
      dev: {deliver: false}
      prod: {type: s3, bucket: reports}
```

Each profile the run uses picks its entry: `--target`, else `DRE_TARGET` (either sets every
profile), else the profile's own `target:`, else `dev`. A used profile without that entry is an
error before anything runs; `dev: {deliver: false}` says `reports_s3` delivers nowhere on a dev
run, and the output stays in the target folder. See [Connections and targets](connections.md).

`dre init` writes this file for you, in `~/.dre` (never into a project). A `profiles.yml` kept
in the project, e.g. for CI, a container or a Databricks job, should take every secret from
`env_var()` so nothing secret is committed.

Targets can share settings with YAML anchors and merge keys, in `profiles.yml` and every other
YAML file DRE reads; keys written out win over merged ones:

```yaml
connections:
  warehouse:
    targets:
      dev: &pg {type: postgres, host: db.internal, user: reports, database: shop}
      prod:
        <<: *pg
        database: shop_prod
```

Plugin packages and macro packages are declared in `dependencies.yml` (or `packages.yml`, or
both):

```yaml
plugins:
  - duckdb
  - xlsx
  - object_store      # the s3, gcs and azure_blob destinations
packages:
  - git: https://github.com/acme/finance_macros.git
    revision: v2.3.0
```

`dre run`, `dre validate` and `dre compile` install what's missing into the project's
`dre_deps/` folder before they start, so there's nothing to run first. `dre deps` installs
everything and refreshes the lock on purpose, e.g. as a separate CI step. Exact versions and
commits are pinned in `dre.lock`. Package macros are called through the package's name
(`{{ dre_utils.star(ref('customers'), except=['ssn']) }}`), and `dispatch()` lets a package offer per-database variants
that a project can override. See [the registry docs](registry.md).

## Inspect a selection before running

Use `dre ls` to see what selectors match; listing defaults to every declared Binding, whereas
a plain run uses each report's default Set. `--set all` makes the run use every variant:

```bash
dre ls -s tag:finance --set all --output json
dre validate -s tag:finance --set domestic --strict --all-targets
dre compile -s tag:finance --set all --var region=AU --timezone Australia/Sydney
```

`--var` overrides every YAML level. Quote lists or maps for the shell, and quote a YAML string
inside the shell argument when it resembles a boolean: `--var flag='"false"'` keeps text.
`--timezone` overrides report timezones. `--all-targets` checks even unused profile entries;
`--strict` also fails on warnings. `--json` on validation gives tools structured problems,
while `--log-format json` gives a stream of diagnostic events.

Run the tutorial example from its project directory:

```bash
cd examples/tutorial
dre validate --profiles-dir . --json
dre --log-format json --color never run --preview 5
```

From another directory, use an absolute `--project-dir` path. `--profiles-dir` names the directory
containing `profiles.yml`. `--target` picks an environment;
`--target-path` chooses artifact storage. A misspelled environment fails with `missing-target-entry`.
Use `-v` or `--verbose` for each step (`--log-level debug` is equivalent), `-q` or `--quiet`
for only errors and the final summary, or `--log-level info` for normal output. `--color always`
keeps ANSI color for a terminal recorder; `--color never` keeps saved logs plain.

## Preview, backfill and accept an intentional schema change

`dre run monthly --dry-run` renders SQL without executing report queries. Templates calling
`run_query()` or `columns()` can still connect while rendering. Use `--preview` (100 rows by
default) or `--preview 10` to execute a sample and inspect files without delivery.

For a backfill, pin the date rather than changing SQL. Keep its artifacts separate while checking:

```bash
DRE_RUN_DATE=2026-09-01 dre run monthly --preview 10 --target-path target-backfill
```

Inspect the query columns against the previous successful output before accepting drift.
`--accept-schema-change` permits delivery and saves the new schema; use it only after the
recipient accepts the intentional column change. A new target path has no previous snapshot,
so use the normal persistent target path for the accepted run.

`--output-name corrected.xlsx` changes the first output's filename; `--output-path` replaces
its delivery path. With multiple outputs these flags do not rename every output.
`--profile alternate` replaces only the inherited connection: an explicit query/source profile
still wins. For an ad hoc variant, pass `--set trial --profile warehouse --var region=AU`.

Bound unattended runs with `--timeout 30m`, retain their artifacts with `--keep-runs 7`, and
use `--threads 2` to run up to two Bindings at once. Queries within a Binding remain ordered.
See [parallel Bindings](connections.md#running-bindings-at-once) and
[run history](target-path.md#runs-and-runs-that-overlap).

## Fix a report before delivery

- `unknown-query`: match `queries` to SQL file stems; query names are unique across the project.
- `no-result-set`: mark setup-only queries `tab: false`, or end a tab query with a result-producing statement.
- `duplicate-output`: give outputs unique `name` values so filenames do not collide.
- `connection-conflict`: split a query reading sources on two connections into separate queries.
- `compile-needs-run`: a template needs database results; check with `dre validate --live` or a preview run.

These [error codes](reference-error-codes.md) are stable even when the detailed message names
different files or columns. `dre explain unknown-query` gives the matching repair instructions.

<!-- docs-nav: generated by .github/scripts/docs_sections.py from docs/sections.json -->

---

**Previous:** [Concepts](concepts.md) · **Next:** [Templates](templates.md)
