# Topic: output formats

Load this when choosing or changing a report's `output.format`, or its column formats, totals,
formulas or template. Use only options listed in `../plugins/format-<format>.md`.

## Ask (one at a time, only what's still open)

1. **Who reads it?** Recommend from the answer:

   | Reader | Format | Package (`dependencies.yml`) |
   |---|---|---|
   | people working with the numbers | `xlsx` (recommended for people) | `xlsx` |
   | people reading a branded document: a statement, a board pack | `pdf` (or `html` for a web page) | `document` |
   | another system | `csv`, or `delimited` for another separator | `csv` |
   | a system with a fixed record layout | `fixed_width` | `fixed_width` |
   | data tools (DuckDB, Spark, pandas) | `parquet` | `parquet` |
   | people who only want the number | a `message` output (no package; see `delivery.md`) | |

   A report can have several outputs (an xlsx and a message, say): see `delivery.md`.
2. **For xlsx:** the tabs in order (one `.sql` per tab, REP-1), the number and date formats per
   column, totals rows, and whether they have an Excel file to fill (`template:`).
3. **For a system:** ask for its spec (free text or the file). Delimiter, quoting, header,
   encoding and line endings for csv; every column's width or picture, alignment, padding,
   decimals and sign for fixed-width. Confirm the layout back as a table.
4. **For pdf/html:** load `documents.md`.

## Write

```yaml
queries:
  - query: sales
    columns:
      net:   {format: "#,##0.00", total: sum}     # a bold totals row under the data
      share: {format: "0.0%"}
output:
  format: xlsx
  date_format: "dd/mm/yyyy"
```

- Keep values typed in SQL and format them here (REP-5): the value stays a number Excel can sum.
- `total:` is `sum`, `average`, `count`, `min`, `max` or a formula over `{column:*}`. It makes one
  totals row per sheet: **subtotals per group** come from SQL (`GROUP BY ROLLUP (...)` or
  `GROUPING SETS`, with a label column), or from a pdf/html template.
- Row formulas: the SQL selects a placeholder column; `formula: "={qty}*{price}"` fills it.
- A branded workbook: `template: {file: monthly.xlsx}`, found next to the report's YAML, then in
  each parent folder, then anywhere in the project where it's unique.

## Check

`dre validate -s <report>` rejects unknown options and malformed format codes. A run fails,
naming the sheet and column, when a format doesn't fit its column (a date code on a number).

Docs: [Excel (xlsx)](../docs/plugin-xlsx.md), [CSV and delimited](../docs/plugin-csv.md),
[Fixed-width](../docs/plugin-fixed_width.md), [Parquet](../docs/plugin-parquet.md).
