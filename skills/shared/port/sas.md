# SAS reporting migration

Use this branch for supplied `.sas` programs, included source and macro definitions. DRE does
not execute SAS. Work toward reviewed database SQL and DRE outputs, with an explicit assessment
of what is supported, needs review or remains blocked (PORT-6). The worked example is
`examples/sas-overdue.md`; its SAS reference is unexecuted.

## Inventory before translation

Read each program and follow supplied `%include` paths, macro definitions/calls, auto-call
library locations, `LIBNAME`, `INFILE` and dataset references. Resolve paths relative to the
actual invocation context; ask about unknown roots. Inspect source as text, including generated
SQL when supplied. Static reading cannot resolve arbitrary macro-generated paths or code.

For each dependency record its source file/location, role, supplied/missing status, output
consumers and impact. Distinguish database tables from WORK intermediates, local `.sas7bdat`
inputs and external CSV files. Record scalar macro values and their origin, formats/informats,
lookup formats and report dates. The existing XML inventory script does not inspect SAS;
add a manual SAS inventory alongside its estate table, rather than pretending an absent row
means no dependency. Ask for missing includes, macro bodies, inputs or business specifications.
Never execute macros to discover their behavior. A missing include that defines a rate or
filter blocks affected outputs; independent sections may progress as explicitly partial work.

## Bounded mappings

Translate only the rows whose semantics are established from supplied source and schema.

| SAS reporting pattern | Database SQL / DRE mapping | Required check |
|---|---|---|
| `PROC SQL` selection, joins, aggregation | one dialect-correct query per output; CTEs or reviewed `tab: false` setup for preparation | join keys/cardinality, NULL handling, grouped records and totals; SAS aggregate remerging is not ordinary GROUP BY |
| row-local DATA step `SET`, `IF`, derived fields, `IF/ELSE` | `WHERE`, expressions, ordered `CASE`; separate queries for explicit output branches | one input row yields known output rows; evaluate filters and exception precedence explicitly |
| scalar `%let report_date=...` used as a value | typed DRE variable with default and `var_schema`; use `run.date` for recurring dates | distinguish a value from a macro that generates identifiers, predicates or code; validate/quote for the target dialect |
| `PROC SORT`, `NODUPKEY`, `BY` with a reviewed `FIRST.key` selection | final `ORDER BY`; `ROW_NUMBER() OVER (PARTITION BY key ORDER BY agreed_priority, unique_tiebreaker)` filtered to 1 | identify the winning row, ties and sort order; reject nondeterministic winners rather than choosing one |
| basic `PROC SUMMARY`/`MEANS` counts and sums, or `PROC SQL GROUP BY` | grouped query; explicit detail and summary tabs | missing class levels, `_TYPE_`/`_FREQ_`, `N` versus row count, all-missing sums, weights and denominators; specialist statistics require separate review |
| `PROC PRINT`, simple tabular `PROC REPORT`, `ODS EXCEL` | named xlsx tabs, column order/formats/totals; an Excel-authored template when needed | authoritative values come from SQL; styles/group breaks can change; complex compute blocks require explicit rules |

For example, a reviewed "latest version" rule becomes this pattern in both databases:

```sql
with ranked as (
  select p.*, row_number() over (
    partition by repayment_id order by version desc
  ) as rn
  from repayments p
)
select loan_id, sum(amount) as paid
from ranked
where rn = 1
group by loan_id
```

This is deterministic only if `(repayment_id, version)` is unique. Check that constraint or
obtain an agreed tie-breaker before using the pattern. Deduplicate before joins/date filtering
when the source rule requires it, as in the overdue-loan example.

## Semantic review before running

Conclude a semantic assessment with the next evidence and execution gates: explain whether
accessible JDBC catalog metadata or supplied DDL will establish names/types/keys, and identify
which generated preparation statements need owner review before execution. This applies even
when the current request is assessment only; keep unknown schema details explicit.

| Topic | What to resolve and exercise |
|---|---|
| Missing values | SAS numeric missing values can satisfy `x < 100`; SQL NULL does not. Decide whether the intended filter is `x IS NULL OR x < 100`, or missing records belong in Exceptions. SAS special missings (`._`, `.A`–`.Z`) need explicit mappings. Character blanks/trailing spaces and Oracle empty strings need review. `SUM(x,y)` is not automatically `x+y`; test partially/all-missing cases. |
| Dates/times | SAS dates count days since 1960-01-01; datetimes count seconds. Identify actual stored types and informats before conversion. Oracle date differences can contain fractional days; SQL Server `DATEDIFF(day, due, report_date)` counts day boundaries. For date-only rules normalize both sides deliberately: Oracle `TRUNC(report_date)-TRUNC(due)`, SQL Server `DATEDIFF(day, CAST(due AS date), CAST(report_date AS date))`. Review time zones, month ends and missing dates. |
| Sorting/deduplication | SQL row order needs `ORDER BY`. Agree collation, case, blank/NULL placement and tie-breakers. SAS `NODUPKEY` can depend on input order/options; it does not necessarily mean DISTINCT or latest version. |
| Match-MERGE | DATA step `MERGE ... BY` and its retained values/`IN=` flags are not an ordinary relational join. Multiple rows per key can produce different cardinality. Request rules and test asymmetric duplicates before proposing SQL; mark unresolved behavior blocked. |
| Rounding/identifiers | Use decimal currency and agree when to round, scale, tie handling and whether summaries sum rounded rows. SAS binary floats and `ROUND(x,0.01)` need boundary checks against Oracle `NUMBER`/SQL Server `DECIMAL`, including negative halves. Preserve identifiers as text, including leading zeros and long values. |
| State/macros | `RETAIN`, sum statements, `LAG` queues, arbitrary `FIRST./LAST.` processing, loops, arrays, hash objects and complex macro expansion require explicit state/ordering rules and review. Specialist statistical procedures, external commands and local SAS datasets are unsupported execution paths; request a database relation or reviewed business rewrite. |

The single reviewed `FIRST.key` deduplication pattern above is a bounded case, not a promise to
translate arbitrary BY-group state. Do not silently turn unresolved state into a window sum.

## Database context and review

Use the JDBC reference for Oracle/SQL Server setup, metadata and limitations. Prefer authorized
`dre catalog` metadata; supplied DDL is sufficient for a draft. Record unknown schemas, types,
keys and permissions. Write separate dialect variants where syntax differs: Oracle private
temporary tables and SQL Server `#temp` tables have different names/lifetimes. Preparation and
final queries must share the report connection. Explain statements and their scope before
ordinary workflow review; run data-changing preparation only after approval. `--no-deliver`
prevents delivery, not SQL side effects.

## Evidence and completion

Use `parity.md` for optional CSV/XLSX comparisons and keyed diffs. Establish matching source
snapshots, parameter values and reporting dates first. Agree keys and per-comparison tolerance;
compare records, classifications, counts and totals rather than trusting totals alone. An
accepted difference needs an explanation, not a larger tolerance chosen to hide it.

With no SAS or reference output, generate independent checks from the business specification:
expected keyed rows, bucket boundaries, join multiplicity, missing inputs, duplicate versions,
dates and money. Report expected versus actual results and unresolved assumptions. This is
business-rule validation; SAS equivalence remains unverified. Configuration validation,
unexecuted reference code and a visually plausible workbook cannot establish SAS parity.
