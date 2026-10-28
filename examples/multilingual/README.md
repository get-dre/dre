# Reports in any language

Nine city offices, each named in its own language and script: Japanese, Chinese, Korean,
Russian, Arabic, Greek, Hindi, Portuguese and German, with staff names and roles in the same
language. One run writes a workbook per office, named after the city and holding only that
office's staff, and one workbook with every office.

```bash
dre validate
dre run cities
```

Expected files: `out/offices/東京.xlsx`, `out/offices/서울.xlsx`, `out/offices/القاهرة.xlsx`,
`out/offices/मुंबई.xlsx`, `out/offices/São Paulo.xlsx` and the rest, one per office with its
two staff, plus `out/all-offices.xlsx` with all nine offices.

Text from the database keeps its script end to end: cell values, sheet contents and file names.
The file name here comes from `path:` (`{{ each.city }}`). Without a `path`, a per-row file is
named after the row's key, with letters and digits of any script kept; see
[per-row outputs](../../docs/per-row-outputs.md#files).
