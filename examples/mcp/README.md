# MCP context from recurring reports

A complete, fictional Harbour Outfitters example. Ordinary DRE works without MCP.
All YAML explains which fields are optional MCP context and which are normal report config.
The MCP companion is separately licensed; this directory contains only public example files.

From this directory, with DRE 0.4.1 or later:

```sh
dre deps --profiles-dir profiles
# Once on a fresh database: represents pipeline outputs, including model/column comments.
dre run setup --profiles-dir profiles --no-deliver
dre validate --profiles-dir profiles
dre run revenue --profiles-dir profiles --no-deliver
dre run revenue --profiles-dir profiles --no-deliver --var region=North
```

Expected revenue: North **30.75 AUD**, South **5.00 AUD**. Net of GST; one row per order.
The setup report creates tables; to recreate this fictional database, remove harbour.duckdb
and its WAL before running setup again. Never expose setup to MCP.

## Enable MCP deliberately

MCP exposure starts disabled. Review the data scope, then change the revenue report's
`meta.ai.expose` to true yourself. With a local companion build or an installed companion:

```sh
# For an unreleased local build, set DRE_MCP_BIN to its executable first.
dre mcp --project-dir . --profiles-dir profiles validate
dre mcp --project-dir . --profiles-dir profiles test
dre mcp --project-dir . --profiles-dir profiles access
dre mcp --project-dir . --profiles-dir profiles serve
```

An AI client can find the revenue report and run it, download its exact workbook, or derive
an aggregate from its saved rows (**35.75 AUD** total). Cards label reviewed execution
**Verified report**, local transformations **Derived from verified result**, and new queries
**Exploratory (unverified)**. Rendered/generated SQL is reviewable and downloadable.

Report-linked orders/customers tables are exposed by default when revenue is exposed. Turn
that off using `expose_report_tables: false`, or the companion's `--no-report-tables` flag.
Additional tables/schemas always require opt-in. The seeded region_targets table demonstrates
an extra table: uncomment its tables entry only if you want to expose it. Exclusions override
both inherited and explicit exposure.

Use refresh_context after changing reports, exposure or upstream comments; the server keeps
serving the previous valid snapshot if refresh fails. Input-specific compiled SQL records the
resolved source context. A report join is evidence of that report's logic, not a guarantee
about every new question.

## Upstream context

Most of the trustworthy context already lives in recurring reports. DRE adds reviewed report
logic to table and column comments supplied by dbt or another data pipeline. This example's
setup query stands in for that pipeline so the demo is reproducible; production documentation
belongs upstream. See [AI-ready reports](../../docs/ai-ready-reports.md).
