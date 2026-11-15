# Monthly finance pack

Synthetic Acme Corp sales; no external database. `setup.sql` creates a temporary table
on the report's DuckDB session. The source declaration names it; a typed YAML lookup maps
Hardware and Services to account codes. Detail and Summary queries share that session. DRE 0.11+ runs the attached data tests.

```bash
dre validate
dre test finance
dre build finance
```

`dre test finance` executes the queries and their checks without report files or delivery.
`dre build finance` checks the data before writing either workbook. `dre run finance` skips
attached checks; use `build` when publishing this pack.

The reusable Jinja test blocks in `macros/checks.sql` reject negative revenue and reconcile
Detail's revenue with Summary's earlier saved result, within one cent. Summary totals the
source directly; Detail joins the account mapping. If the Services mapping is missing from
`lookups/accounts.yml`, its revenue disappears from Detail, the reconciliation fails, and
`build` writes or delivers neither workbook. The tests inspect saved query results without
rerunning the source SQL. Test tolerance is supplied by YAML, not hard-coded in the macro.

Expected files after a successful build:

- `out/finance-2026-01.xlsx`: Detail, Summary, Targets and Calendar tabs; revenue 3,500
  total; comma-separated decimals, styled tabs, a commission formula on every detail row and
  cached totals. Targets joins a CSV lookup (`lookups/region_targets.csv`, typed and loaded
  into a temp table) and an Excel one (`lookups/commission_rates.xlsx`, its `Rates` sheet).
  Calendar starts at B3 with no header row, in the workbook's date and time formats. The
  Exceptions tab is left out because it has no rows (`on_empty: skip`).
- `out/presentation-2026-01.xlsx`: the supplied Excel template filled with North 2,000
  and South 1,500, and the month's total 3,500 in B2. DRE expands its `SUM(B5:B5)` total to `SUM(B5:B6)`.

Commission is an illustrative 10% of revenue, selected in SQL as the formula's cached
result. The template's total formula recalculates when Excel opens it; automated checks
assert its expanded formula, while the normal workbook's totals have cached values.

Change the month:

```bash
dre build finance --var month=2026-02
```

February totals 2,500 and writes new month-labelled files. The committed
`templates/finance.xlsx` is a small editable template: title in A1, header in A4:B4,
one data row in A5:B5, and a total in A6:B6. Edit it in Excel to change branding and layout.
Keep the query binding anchor aligned with the template.

`sources/sales.yml` shows every dbt source key: a dbt `sources.yml` pastes in unchanged, and
`dre validate` notes the keys DRE doesn't use yet. `finance.yml` declares its variables
(`var_schema`), so `--var month=2026-2` is refused before anything runs.
