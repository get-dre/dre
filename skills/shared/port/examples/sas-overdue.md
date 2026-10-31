# Worked migration: synthetic overdue loans

The public project is [examples/overdue-loans](https://github.com/get-dre/dre/tree/master/examples/overdue-loans).
Use its README and REPORT-SPEC.md with the source and inputs; the repository's example package
script includes everything needed to run independently. The SAS program is **unexecuted**.
The Oracle and SQL Server DRE projects were checked against independent expectations, not SAS
output. This is a reference migration, not a measured automatic conversion or savings claim.

## Read and assess

- `reference.sas` reads `inputs/{customers,branches,loans,repayments}.csv`. Its scalar
  `report_date` is month end; `input_dir` and `workbook` are file locations. There are no missing
  includes/macros in this supplied example. Local CSV loading is a fixture setup, not a new
  runtime for local SAS datasets.
- `REPORT-SPEC.md` independently defines eligibility, due-date buckets, exception precedence,
  latest-version selection, payments through the reporting date and financial rounding.
- `expected/2026-01-31.json` and `expected/2026-02-28.json` contain explicit expected records,
  classifications and totals. January has 8 detail accounts and AUD 3,480.01.

## Map the preparation and output

| SAS source | DRE project evidence |
|---|---|
| CSV DATA steps and declared character identifiers/dates | `prepare.py` writes literal seed SQL, preserving identifiers as text; it performs no reporting calculation |
| SORT by ID/descending version, `FIRST.id` | `oracle/reports/overdue/prepare.sql` and the SQL Server variant use row-number selection before joining or date filtering; fixture versions have an unambiguous winner |
| PROC SQL payment aggregation and eligible-loan joins | same preparation query; aggregate repayments before joining so duplicate matches do not multiply balances |
| row-local DATA step filters, derived balances, exception branches and overdue buckets | dialect-specific CASE expressions, decimal rounding and date arithmetic; missing principal/due and unmatched dimensions become explicit Exceptions |
| scalar reporting date | default plus typed `var_schema` in the project configuration; reruns can change the date |
| PROC SQL basic counts/sums and PROC PRINT/ODS EXCEL | separate Summary, Detail and Exceptions queries; ordinary xlsx plus an Excel-authored presentation template |

Oracle uses private temporary tables, `TO_DATE` and date subtraction; SQL Server uses `#temp`
tables, date conversion and `DATEDIFF`. Preparation stays in the source database and the same
report session. Review both `seed.sql` and `prepare.sql` before a run: they create and populate
temporary relations. Real migration inputs normally already live in the database; fixture
seeding is not an instruction to copy production data locally.

## Validate and run without SAS

Follow the example README to install its dependencies, provide a JDBC vendor driver and
operator-maintained Java 17+, and configure the disposable database environment. From a
checkout of the example root, after reviewing generated statements:

```bash
python3 prepare.py oracle
cd oracle
dre deps
dre validate
dre run overdue
cd ..
python3 check.py oracle --verify oracle
```

Replace `oracle` with `sqlserver` for that database. The checker reuses the existing CSV/XLSX
parity reader and checks every keyed field, type, bucket, null and total against independent
expectations. Use the README's changed-date and failure/rerun instructions as additional checks.
The example's database-backed CI runs both variants; the skill does not manufacture a passing
run when a database/driver is unavailable. Record such a run as pending in a new environment.

Open both simple and presentation workbooks. The example README explains the verified template
features and its illustrative **2x (demo)** formula. An authored workbook layout is an accepted
alternative to exact ODS/PROC REPORT layout. Native Excel objects and arbitrary formulas need
object-specific verification; inspect the actual outputs rather than assuming fidelity.

Report: business-rule validation and actual DRE execution evidence, SAS equivalence unverified.
For a customer's supplied old export, use `../parity.md` with matching inputs/date/parameters,
`loan_id` as detail/exception key, the summary group key, and an agreed money tolerance. Keep
missing dependencies or unresolved SAS semantics visible; they prevent a complete-port claim.
