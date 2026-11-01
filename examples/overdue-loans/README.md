# Monthly overdue-loan reporting

A complete synthetic reporting project for Oracle and SQL Server. Preparation runs in the
source database; DRE writes a simple workbook and a workbook filled from an Excel-authored
template. Both contain Summary, Detail and Exceptions. The template also retains Notes.
Read [the business rules](REPORT-SPEC.md), inspect the small [CSV inputs](inputs/) and
[literal expected results](expected/), then run either database variant. No customer data
or SAS installation is required.

**The SAS reference is unexecuted.** DRE is checked against independent business expectations,
not against established SAS output parity. This demonstrates principal reporting, not a
regulatory arrears model, statistical modelling or replacement of the full SAS suite.

## Requirements and connection

- DRE **0.10.0 or newer**, JDBC **1.0.0 or newer**, and XLSX **1.1.3 or newer**. Earlier XLSX
  template versions can alter text identifiers or expansion ranges. See the [JDBC plugin
  guide](https://github.com/get-dre/dre/blob/master/docs/plugin-jdbc.md) for installation and
  configuration.
- User-installed and maintained **Java 17+**, selected through `JAVA_HOME/bin/java`, otherwise
  `java` on `PATH`. A compatible vendor driver may impose its own minimum. The plugin does
  not bundle, download, install, patch or update Java.
- A user-supplied Oracle Thin or Microsoft SQL Server JDBC driver JAR. Drivers are not included
  or downloaded by this project. Python 3.8+ is needed for preparation and checks.
- An existing empty test account/database. Oracle needs `CREATE SESSION` and `CREATE TABLE`
  for private temporary tables; SQL Server needs connection permission and local temporary
  table creation in tempdb. Use synthetic data and review the SQL first. Tables are private
  to the report connection and disappear when it closes, including after failure.

The engine's pinned integration environments are Oracle Free **23.26.3.0-lite**, Oracle JDBC
**23.26.3.0.0**, and SQL Server **2022 CU27 (16.0.4295.3)** on x86-64 Linux with Microsoft JDBC
**13.6.1.jre11**, using Java 17 and Arrow Java 19.0.0. The example's own CI runs both variants
in those environments. Check the PR's completed CI results for this revision's evidence;
these are tested fixtures, not certification of every version. Local Apple Silicon SQL Server
emulation is not support evidence. SAS has not been run.

Choose one subdirectory. Supply connection values using environment variables; do not put
credentials in SQL, shell history, a committed profile or JDBC URLs shown in logs. For example,
in Bash/Zsh, set a non-secret URL/JAR/user and enter the password interactively:

```bash
export JAVA_HOME=/path/to/your/jdk
export DRE_TEST_JDBC_JAR=/absolute/path/to/your/driver.jar
export DRE_TEST_JDBC_USER=synthetic_reporting
# Oracle:
export DRE_TEST_JDBC_URL='jdbc:oracle:thin:@//localhost:1521/FREEPDB1'
# Or SQL Server (set the catalog too):
# export DRE_TEST_JDBC_URL='jdbc:sqlserver://localhost:1433'
# export DRE_TEST_JDBC_CATALOG=synthetic_reporting
read -rs 'DRE_TEST_JDBC_PASSWORD?Database password: '; echo
export DRE_TEST_JDBC_PASSWORD
```

The password prompt syntax above is Zsh. In Bash use
`read -r -s -p 'Database password: ' DRE_TEST_JDBC_PASSWORD; echo` instead.
SQL Server defaults to encrypted connections with certificate validation. Configure trusted
certificates for your environment. Only for a disposable local test server with its self-signed
certificate, set `DRE_TEST_JDBC_TRUST_CERTIFICATE=true` (and, if required by that test server,
`DRE_TEST_JDBC_ENCRYPT=false`). Never treat those test settings as production guidance.
Specify each driver setting once; URL/property precedence belongs to the vendor driver.

## Run and inspect

From the example root, substitute `sqlserver` for `oracle` throughout when appropriate:

```bash
python3 prepare.py oracle
cd oracle
dre deps
dre validate
dre run overdue
cd ..
python3 check.py oracle --verify oracle
```

`prepare.py` converts the CSV fixtures into a `seed.sql` file. It does no report calculation
and opens no database connection. `seed.sql` creates private source tables and inserts the
fixture records inside the source database. `prepare.sql` deduplicates, aggregates, joins and
classifies them in that database. Final SQL queries read its prepared relation on the same
session. Python only prepares literal inputs and checks saved outputs.

Open `oracle/out/simple-2026-01-31.xlsx` and `oracle/out/presentation-2026-01-31.xlsx`.
Check the long loan ID and leading zeros, the null name, six grouped balances, and five
exception reasons. January has **8 detail accounts and AUD 3,480.01**. The checker compares
every keyed field, bucket, null and amount, checks numeric/text workbook cell types, rejects
duplicate keys, and checks totals. It uses the existing report-porting CSV/XLSX reader
(`parity.py` is included unchanged in downloads), with stricter fixture-specific assertions.

The supplied template is the engine's Microsoft Excel for Mac-authored `reporting.xlsx`
fixture, reused unchanged. On Summary it reserves one data row below row 4. It has a logo,
merged title, wrapped text, frozen panes, filter, conditional highlighting above 150, A4
landscape printing, repeating headings, and expanding formulas. DRE replaces the headings
and fills that row block. Detail is bound into the existing Detail sheet; Exceptions is
appended as an ordinary sheet. Notes is retained. Template column D is explicitly labelled
**2x (demo)**: an inherited illustrative formula, not a banking measure. The total
footer and illustrative formula recalculate in Excel; their saved caches are not authoritative.
SQL balances and the simple workbook remain authoritative for automated comparisons.

Separate sheets and SQL groups replace exact SAS group-break layouts. Use the ordinary
workbook for simple formatting, or edit the template in Excel to change its presentation.
Do not assume native charts, PivotTables, slicers, macros or external connections will be
created, refreshed or preserved. The template integration tests check expanding ranges and
formulas; this example checks business values. Open output in your intended Excel version
before adopting a changed template. The inherited template's existing print range is a demo
layout, not a guarantee that arbitrary larger reports fit on one page.

## Change date, run unattended, recover

```bash
cd oracle
dre run overdue --var report_date=2026-02-28
cd ..
python3 check.py oracle --verify oracle --date 2026-02-28
```

February includes the newly opened loan and the later payment: **9 accounts, AUD 4,375.01**.
For unattended operation, inject the same environment from your scheduler's secret store,
set its working directory to `oracle` or `sqlserver`, and invoke the same command. DRE exits
nonzero on failure; alert on that exit status. `dre deps` is a provisioning step, not required
for every scheduled run. Keep each scheduled invocation's logs and run records. Avoid
overlapping jobs that deliver the same dated filenames.

A useful failure needs no credentials change:

```bash
cd oracle
dre run overdue --var report_date=invalid-date  # must exit nonzero in source SQL
dre run overdue                              # rerun succeeds with fresh session tables
cd ..
python3 check.py oracle --verify oracle
```

The last successful dated files can still exist after a failed attempt. File existence alone
does not prove the latest job succeeded; check its exit status and run record.

## Larger fixture and clean-run checks

```bash
python3 prepare.py oracle --copies 100
cd oracle && dre run overdue && cd ..
python3 check.py oracle --verify oracle --copies 100
python3 prepare.py oracle  # restore the small fixture
python3 check.py oracle    # isolated clean scratch project: both dates, failure/rerun, larger fixture
```

The larger input contains 1,600 loans and 400 repayment versions. The clean check prints actual
CLI wall time and data volumes, including connection, preparation and both workbook writes.
It removes scratch outputs on exit. Timing depends on machine, database, drivers and network;
it is not a universal speed, savings or SAS performance comparison. Pass `--large-copies 10`
for a smaller experiment or up to 1,000 for more rows. The submitted change's execution logs
record observed runtime and exact versions; the checker measures your environment afresh.

For a retained sample, run the ordinary commands above; `check.py` intentionally removes its
own temporary outputs. A report has no persistent customer tables to drop. Remove only your
example `out/`, `target/`, generated `reports/overdue/seed.sql` and local `dre.lock` when done.

## Download and reference source

Maintainers can prepare the website asset without publishing a site or release:

```bash
python3 package.py --output /tmp/overdue-loans.tar.gz
```

The deterministic archive and adjacent SHA-256 file include both complete projects, template
files, inputs, independent expectations, tools, GPL licence and [unexecuted SAS reference](reference.sas).
The archive contains no Java runtime, vendor driver, output, lockfile or credential file.
Extract it into an empty directory, then follow this README; no repository checkout is needed.
The integration test runs the extracted download, not a special database-only imitation.

`reference.sas` is a reasonable Base SAS/PROC SQL/ODS EXCEL illustration of the same rules,
not deliberately expanded comparison code. It reads the small CSV fixture with explicit text
identifier lengths and date informats. Its supported syntax, missing-value behavior and
workbook output still require a SAS operator to validate. It has never been executed here.
