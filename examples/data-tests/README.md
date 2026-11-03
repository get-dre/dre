# Query data tests

A complete project with synthetic values and custom Jinja tests. Local checks use the query's
saved Arrow rows through embedded core DuckDB. The DuckDB source plugin in this example is
needed to execute its report SQL, independently of that local test engine. No server or account
is needed for `checked` or `diagnostics`.

From this folder, with a DRE build that supports query data tests:

```sh
dre deps
dre validate
dre test checked
dre build --schedule checked_daily
dre run checked
```

`test` runs fresh queries and checks without report files or delivery. `build` produces
`out/checked.csv` only after checks pass. `run` intentionally skips attached checks.
`-s tag:data_checks` selects the same `checked` report. External orchestration chooses the
command and invokes the Schedule; DRE supplies no scheduler.

`checked` saves hidden expected totals, reconciles the detail result, checks a typed empty
result and verifies a sequence reaches three after the two detail rows. Multiple checks inspect
those saved rows without re-executing the report queries. Macro arguments such as tolerances
remain in YAML. These definitions are project-owned examples, not built-in tests.

## Investigate failures

```sh
dre build diagnostics --set all       # expected nonzero: some Bindings fail
dre test diagnostics --set warning_only  # succeeds with warning-level bad data
dre history diagnostics --binding data_failure --latest --path
```

The `data_failure` Binding collects both checks on its current query, skips the later query
and blocks report files and delivery. It records two failure rows while retaining one CSV
example. `warning_only` continues and can deliver its CSV. `execution_error` uses a missing
runtime column with warning severity: it still errors and skips remaining checks immediately.
`no_evidence` finds bad data without retaining any CSV. `independent` succeeds despite another
Binding failing. The overall command returns nonzero when any selected Binding fails.

Inspect `tests` and `skipped_queries` in the run folder's `run_results.json`, or use
`--log-format json`. Default diagnostics contain no row samples. Opt-in evidence remains under
the Target path, outside report outputs and delivery; `dre clean` removes it. CSV contains text,
a header and empty fields for nulls; it loses Arrow type information. Import untrusted values
as text in spreadsheets to avoid formula evaluation or automatic number/date conversion.

## Optional PostgreSQL source session

Edit connection `source` in `profiles.yml` for a disposable PostgreSQL test database, and supply
`DRE_SECRET_PG_PASSWORD` through your environment. The sample port is 55432. Then:

```sh
dre test source_checks
```

The first query creates a temporary table and view. Source checks inspect those explicit
relations through that same connection/session before the next query reads them. The returned
rows then receive a local check. No persistent objects are created by this example. A view can
re-evaluate its underlying SQL, and a failed check does not roll back earlier source side
effects. The plugin must support sessions; local tests never upload saved rows to the source
or silently substitute an execution engine.

Reusable definitions can also come from Git/local packages declared in `packages.yml` and
installed with `dre deps`; qualify them as `package.test_name`. Missing definitions fail before
source SQL where detectable. Familiar `test` blocks do not promise general dbt package or
adapter compatibility. See [the full guide](../../docs/building-reports.md).
