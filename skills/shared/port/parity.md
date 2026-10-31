# The parity check

A port ends with explicit validation evidence (PORT-2). When a comparable old output exists,
`scripts/parity.py` compares its rows with DRE for matching inputs, parameters and dates; the
user reviews the layout. Otherwise use business-rule checks below.

## No reference output, or unexecuted SAS

An old output is optional. Without one, validate against the report specification: expected
keyed records, classifications, counts, totals, nulls, date boundaries, deduplication and
rounding. Save the checks and expected/actual results. Say "business-rule validation passed;
SAS equivalence unverified" when appropriate. Missing rules/dependencies remain explicit
blockers even if configuration validates. Use `sas.md` and `examples/sas-overdue.md` for SAS.

With a supplied SAS CSV/XLSX output, confirm that the source snapshot, parameters and reporting
dates match before using the existing comparison below. For xlsx use `--sheet` for the intended
tab and compare data values; formulas without cached results are not independent evidence.
A synthetic expected output tests business rules; label it accordingly rather than claiming a
SAS execution. Record keys, totals, tolerance and accepted differences in the handover.

## 1. The old report's export

Pick one set of parameter values: recommend the last complete period, with every multi-value
parameter at its usual selection. Ask the user to export the old report with them, as **csv**
(best: no formatting in the way) or xlsx, and as PDF too when the layout matters.

- **SSRS (web portal):** run the report with those values, then Export > CSV (comma delimited)
  or Excel. Or by URL, which records the parameters exactly:
  `https://<server>/ReportServer?/<folder>/<report>&rs:Format=CSV&Month=2026-09-01&Region=NORTH&Region=SOUTH`
  (a multi-value parameter is repeated). `rs:Format=EXCELOPENXML` gives xlsx, `PDF` a PDF.
- **Power BI paginated:** run it in the Power BI service with those values, then Export > CSV
  or Excel.
- **Crystal Reports:** File > Export > Character-separated values (CSV), or Microsoft Excel
  Workbook Data-only. A Crystal XML *data* export works too once saved as csv; the
  definition export doesn't hold rows.
- **SAS:** reuse an existing CSV or XLSX export if available; SAS installation or re-execution is not required. Confirm inputs, parameters and reporting dates first.
- **SQL plus a spreadsheet:** the spreadsheet they send today is the export.

Exports often add things that aren't rows: title lines (skip them with `--skip-rows N`), a grand
total line (`--skip-footer 1`), textbox names as headers (`Textbox12`: rename with `--map`). The
SSRS csv export flattens groups: group values repeat on each detail row, and subtotals may
appear as extra columns; ignore those with `--map "Textbox9="` and compare the totals the script
prints instead.

## 2. DRE's run with the same parameters

```bash
dre run <report> --var month=2026-09-01 --var 'regions=[NORTH, SOUTH]' --no-deliver --output-json parity
```

- `--no-deliver` keeps everything local; `--output-json parity` writes each query's typed rows
  to `parity/<report>/<binding>/<query>.json` (the Binding is `default`, or the Set's name with
  `--set <name>`).
- Not `--preview`: its row limit makes the counts differ.
- A report whose dates come from `run.date`: pin the date instead, `DRE_RUN_DATE=2026-10-01 dre
  run ...`, so `run.date.prev_month` is September.
- It runs against the connection's target, usually `dev`. If the old report read production,
  ask before running against `prod` (RUN-2).

## 3. Compare

```bash
python3 <skill>/scripts/parity.py --dre parity/<report>/default/<query>.json \
  --old old_export.csv --key region,store --map "Textbox12=region"
```

| Option | Use |
|---|---|
| `--key a,b` | the column(s) that identify a row, by DRE's names; without it rows are compared in order |
| `--map "Old=new,Other="` | rename old columns to DRE's; `Old=` ignores a column |
| `--only col=value` | compare only the rows with that value on both sides: one store of a per-row run, one Set's client |
| `--skip-rows N`, `--skip-footer N` | lines before the header, and after the last data row, of the old export |
| `--sheet <name>` | the sheet of an xlsx (default: the first) |
| `--tolerance 0.01` | the largest numeric difference counted as rounding (default 0.005) |
| `--json` | the full result as JSON |

Columns are matched ignoring case, spaces and underscores (`Net Sales` = `net_sales`). DRE's
side can also be a csv or xlsx it wrote (with `--output-json parity`, under
`parity/run/<report>/<binding>/runs/<run id>/`), but prefer the JSON: it holds the typed values,
and an xlsx totals row would count as a row.

Exit codes: `0` MATCH (only rounding and formatting differ), `1` DIFFERENT, `2` a usage error.

## 4. Read the result and explain it

The script prints the row counts, a table of column totals, and the cell differences, each with
a kind:

| Kind | Means | Do |
|---|---|---|
| `value` | a real difference | find out which report is right; usually a filter, a join or a parameter default differs. Fix the SQL, or record why the old one was wrong |
| `rounding` | numbers within the tolerance | round in SQL to the old report's precision, or accept it and say why |
| `formatting` | the same value written another way: `01/10/2026` and `2026-10-01`, `1,234.50` and `1234.5`, case or spaces | fix the output's column formats (REP-5); not a difference in the data |
| missing / extra keys | rows only on one side | a filter, a join type (inner vs left) or the period's bounds differ |
| duplicate keys | the key doesn't identify one row | add a column to `--key` |
| order differs | the same rows in another order (no `--key`) | add an `ORDER BY` if the order matters to readers |

Explain each kind in plain words, with one example row, then fix and run steps 2 and 3 again.
The check passes at MATCH, or when the user has accepted each remaining difference with a reason;
write those reasons in the hand-over.

## 5. The layout

Numbers matching isn't the whole port. For a pdf or a styled xlsx, ask the user to open the old
PDF and DRE's file (under `target/run/`) side by side: headings, column order, number formats,
page breaks, headers and footers. Fix what they point out, using `dre run <report>
--from-results parity` to re-render without querying again.
