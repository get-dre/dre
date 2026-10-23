---
title: "Moving from SSRS or Crystal"
description: "Port reports from SSRS, Power BI paginated, Crystal Reports or SQL scripts with the dre-port agent skill, and prove each one matches with a parity check."
section: build-reports
position: 11
---

# Moving from SSRS or Crystal

Most teams that adopt DRE already have reports somewhere else: SQL Server Reporting Services
(SSRS), Power BI paginated reports, Crystal Reports, or a SQL script and a spreadsheet someone
fills in by hand. The [`dre-port` agent skill](../skills/README.md) moves them over with you, one
report at a time, and finishes each one with a **parity check**: the new report's rows compared
with an export of the old one, for the same parameters. *New in skills 4.0.*

DRE has no converter command. Your coding agent reads the old report, translates it with the
skill's mapping reference, asks you about anything the files don't say, and writes ordinary DRE
files you can read and change.

## What it reads

| You have | What the agent reads |
|---|---|
| SSRS, Power BI paginated or Report Builder `.rdl` / `.rdlc` files | datasets (query text or stored procedure), parameters, groups, expressions, page header and footer |
| SSRS subscriptions | a csv you export from the report server's catalog database (the skill has the read-only query) |
| Crystal Reports | an XML export of the report definition, or a "Report Definition" text export. Not a binary `.rpt`: Crystal's file format needs SAP's SDK, so export it first |
| anything else (Jasper, BI Publisher, Access, an Excel macro) | the SQL, a screenshot or PDF of the output, its parameters, and who gets it when; the agent asks for the rest |

## Where your data lives

DRE reads DuckDB, Postgres, Databricks, Snowflake, BigQuery and ClickHouse, and **has no SQL
Server source**. A report that reads SQL Server can be ported when the same tables are in one of
those (replicated, or moved): the agent translates the T-SQL to that warehouse's dialect. When the
data is only in SQL Server, the agent says so and moves on to the next report.

## How a report maps

| Old report | DRE |
|---|---|
| a dataset's query | a `.sql` file in the report folder |
| a stored procedure | its logic as SQL (temp tables in a setup file) |
| parameters, with defaults and available values | [variables](templates.md) with `var_schema`; "last month" defaults come from `run.date` |
| a table or list | an xlsx tab, or a table in a [PDF template](plugin-document.md) |
| groups with subtotals | `ROLLUP` in SQL, or a PDF template with a heading and subtotal per group |
| expressions and formulas (`IIf`, `Format`, Crystal `{@formulas}`) | SQL columns, and column formats in the output |
| page header, footer, "Page N of M" | the PDF template's `@page` rules |
| "no rows" message | [`on_empty`](building-reports.md#when-theres-no-data) |
| email and file share subscriptions, schedules | destinations and `schedules.yml` |
| data-driven subscriptions, bursting per customer | [per-row outputs](per-row-outputs.md) (`for_each`) |
| one copy of a report per region or client | one report with [Sets](building-reports.md) |
| interactive sort, drill-through, toggles, charts | no direct equivalent: DRE makes files. The agent asks what to do instead |

## The parity check

For each report you export the old one for one set of parameter values (the last complete
month, say) as csv or xlsx. The agent runs the DRE report with the same values, writing its rows
with `dre run <report> --no-deliver --output-json parity`, and compares the two with a small
script bundled with the skill. It reports:

- **row counts** on each side;
- **column totals** for every numeric column;
- a **cell diff** keyed on the columns that identify a row, each difference explained as a real
  *value* difference, *rounding* (within a tolerance), *formatting* (the same date or number
  written another way) or *order*;
- rows only on one side, and duplicate keys.

Real differences are fixed (or one of the two reports is shown to be wrong) and the check runs
again, until it matches. For a PDF or a styled workbook you also compare the old and new files
side by side.

## A whole estate

Pointed at a folder of `.rdl` files and Crystal exports, the skill first lists every report: its
sources, parameters, subscriptions and an effort estimate (S to XL, with the reasons, such as
stored procedures, custom code or charts). It groups near-duplicates (a copy per region) as
candidates for one report with Sets, and suggests an order, simplest first. Then reports are
ported one at a time.

## Practices

The skill follows the porting rules in [DRE practices](practices.md#porting-reports-from-other-tools):
logic in SQL, not templates (PORT-1); parity before switching over, with the old report running
for one more cycle (PORT-2); one report at a time, simplest first (PORT-3); first deliveries held
or on dev (PORT-4); and a record of where each report came from (PORT-5).

## Try it

Install the [agent skills](../skills/README.md), then ask your agent something like "port this
SSRS report to DRE" with the `.rdl` file in your project, or "list the reports in ./ssrs-export
and suggest where to start".

<!-- docs-nav: generated by .github/scripts/docs_sections.py from docs/sections.json -->

---

**Previous:** [Tested examples](examples.md) · **Next:** [Connections and targets](connections.md)
