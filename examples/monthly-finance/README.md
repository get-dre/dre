# Monthly finance pack

Synthetic Acme Corp sales; no external database. `setup.sql` creates a temporary table
on the report's DuckDB session. The source declaration names it; a typed YAML lookup maps
Hardware and Services to account codes. Detail and Summary queries share that session.

```bash
dre validate
dre run finance
```

Expected files:

- `out/finance-2026-01.xlsx`: Detail and Summary tabs; revenue 3,500 total; comma-separated
  decimals, styled headers, a commission formula on every detail row and cached totals.
- `out/presentation-2026-01.xlsx`: the supplied Excel template filled with North 2,000
  and South 1,500. DRE expands its `SUM(B5:B5)` total to `SUM(B5:B6)`.

Commission is an illustrative 10% of revenue, selected in SQL as the formula's cached
result. The template's total formula recalculates when Excel opens it; automated checks
assert its expanded formula, while the normal workbook's totals have cached values.

Change the month:

```bash
dre run finance --var month=2026-02
```

February totals 2,500 and writes new month-labelled files. The committed
`templates/finance.xlsx` is a small editable template: title in A1, header in A4:B4,
one data row in A5:B5, and a total in A6:B6. Edit it in Excel to change branding and layout.
Keep the query binding anchor aligned with the template.
