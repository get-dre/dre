---
title: "Your first report in 10 minutes"
description: "Build an Excel report with DuckDB, then add variables, tabs, formats, Sets, a schedule and local delivery."
section: get-started
position: 3
---

# Your first report in 10 minutes

Build a real Excel workbook from six synthetic sales rows. Everything runs locally: no database
server, password or account. You need Python 3.9+ and Git to follow the commands below.
On systems where Python is called `python3` or `py`, use that command instead of `python`.
Excel, LibreOffice or another spreadsheet viewer can open the result.

The [complete tutorial project](https://github.com/get-dre/dre/blob/master/examples/tutorial/README.md) and every intermediate step are
tested in CI. Each step below names the exact source files and expected output. A small helper
copies those files into your project, so there is no large SQL block to type.

## 1. Install and run one query

Open a terminal in a working directory. These commands work in PowerShell, Bash and Zsh:

```bash
python -m pip install dre-cli
dre --version
git clone --depth 1 https://github.com/get-dre/dre.git dre-tutorial-source
dre new first-report --profile sample
python dre-tutorial-source/examples/tutorial/prepare.py 1 first-report
cd first-report
dre validate
dre run sales
```

The first run downloads the DuckDB and Excel plugins. Subsequent runs reuse them.
`dre new` also creates a hello report; it stays available, but this tutorial selects `sales`.

The helper creates this project connection:

```yaml
# profiles.yml
connections:
  sample:
    targets:
      dev: {type: duckdb}
```

With no `path:`, DuckDB runs in memory. The exact project files are
[dependencies.yml](https://github.com/get-dre/dre/blob/master/examples/tutorial/dependencies.yml),
[sales.sql](https://github.com/get-dre/dre/blob/master/examples/tutorial/.steps/01-first-report/reports/sales/sales.sql) and
[sales.yml](https://github.com/get-dre/dre/blob/master/examples/tutorial/.steps/01-first-report/reports/sales/sales.yml):

```yaml
# dre_project.yml
name: first_report
default_profile: sample
```

```yaml
# reports/sales/sales.yml
queries: [sales]
output:
  format: xlsx
```

`sales.sql` selects the four January rows from an inline six-row dataset. The columns are
`region`, `month`, `category` and `revenue`. The run prints the workbook path under
`target/run/sales/default/runs/<run-id>/`; open that file.

Expected `sales` tab:

| region | month | category | revenue |
|---|---|---|---|
| North | 2026-01 | Hardware | 1200 |
| North | 2026-01 | Services | 800 |
| South | 2026-01 | Hardware | 900 |
| South | 2026-01 | Services | 600 |

Validation ends with `Validation passed: 0 errors, 0 warnings`. The run ends with one
succeeded report and zero failures. Nothing has been delivered outside your project.

## 2. Change the month with a variable

```bash
python ../dre-tutorial-source/examples/tutorial/prepare.py 2 .
dre run sales --var month=2026-02
```

Changed files: [dre_project.yml](https://github.com/get-dre/dre/blob/master/examples/tutorial/.steps/02-variable/dre_project.yml) and
[sales.sql](https://github.com/get-dre/dre/blob/master/examples/tutorial/.steps/02-variable/reports/sales/sales.sql).
The project now defines:

```yaml
vars:
  month: '2026-01'
```

SQL reads it with:

```sql
where month = '{{ var("month") }}'
```

Expected output: the same four columns, now with two February Hardware rows: North 1,400
and South 1,100. `--var` changes this run only; the file still defaults to January.

## 3. Add a Summary tab

```bash
python ../dre-tutorial-source/examples/tutorial/prepare.py 3 .
dre run sales --accept-schema-change
```

Changed files: [sales.yml](https://github.com/get-dre/dre/blob/master/examples/tutorial/.steps/03-second-tab/reports/sales/sales.yml) and
the new [summary.sql](https://github.com/get-dre/dre/blob/master/examples/tutorial/.steps/03-second-tab/reports/sales/summary.sql).

```yaml
queries:
  - {query: sales, tab_name: Sales}
  - {query: summary, tab_name: Summary}
output:
  format: xlsx
```

Expected output: a January workbook with Sales and Summary tabs. Summary has two columns,
`region` and `revenue`: North 2,000, South 1,500. Sales has the four January detail rows again.

Adding and renaming tabs changes the result schema. `--accept-schema-change` accepts this
intentional change; later runs do not need it. See [Run options](cli-reference.md#dre-run).

## 4. Format revenue as a number

```bash
python ../dre-tutorial-source/examples/tutorial/prepare.py 4 .
dre run sales
```

Changed file: [sales.yml](https://github.com/get-dre/dre/blob/master/examples/tutorial/.steps/04-number-format/reports/sales/sales.yml).
Both query entries now set:

```yaml
columns:
  revenue: {format: "#,##0.00"}
```

Expected output: the same rows and values, displayed as `1,200.00`, `800.00`,
`2,000.00` and so on. Cells remain numbers, so Excel can sum and sort them.

## 5. Make one workbook per region

```bash
python ../dre-tutorial-source/examples/tutorial/prepare.py 5 .
dre run sales --set all
```

Changed files: [sales.yml](https://github.com/get-dre/dre/blob/master/examples/tutorial/.steps/05-sets/reports/sales/sales.yml),
[sales.sql](https://github.com/get-dre/dre/blob/master/examples/tutorial/.steps/05-sets/reports/sales/sales.sql) and
[summary.sql](https://github.com/get-dre/dre/blob/master/examples/tutorial/.steps/05-sets/reports/sales/summary.sql).
The report declares:

```yaml
sets:
  - {name: north, vars: {region: North}}
  - {name: south, vars: {region: South}}
default_set: north
```

Both SQL files now filter on `region = '{{ var("region") }}'`.

Expected output: two successful Bindings. Each has its own workbook under
`target/run/sales/north/runs/<run-id>/` or `target/run/sales/south/runs/<run-id>/`.
North has two Sales rows and a Summary total of 2,000; South has two rows and a total of 1,500.
A plain `dre run sales` uses North; `--set south` runs only South.

## 6. Name a schedule

```bash
python ../dre-tutorial-source/examples/tutorial/prepare.py 6 .
dre validate
dre schedule ls
```

New file: [schedules.yml](https://github.com/get-dre/dre/blob/master/examples/tutorial/.steps/06-schedule/schedules.yml).

```yaml
- name: weekday_sales
  report: sales
  set: north
  cron: '0 9 * * 1-5'
  timezone: UTC
```

Expected output: `weekday_sales` appears with its next weekday occurrences at 09:00 UTC.
The dates depend on when you run the command. The existing workbooks stay as they are.
Listing schedules does not start a background process. See [Scheduling](schedules.md)
to run the foreground scheduler under a process manager.

## 7. Deliver to a local folder

```bash
python ../dre-tutorial-source/examples/tutorial/prepare.py 7 .
dre run sales --set all
```

Changed file: [sales.yml](https://github.com/get-dre/dre/blob/master/examples/tutorial/.steps/07-local-delivery/reports/sales/sales.yml).
The output gains:

```yaml
destination: {profile: local, path: "out/{{ var('region') }}.xlsx"}
```

Expected files: `out/North.xlsx` and `out/South.xlsx`, each with Sales and Summary tabs.
North still totals 2,000; South 1,500. DRE also keeps each run's original output and
`run_results.json` under `target/`.

`local` is built in and needs no destination profile. When you are ready, use an
[SFTP profile](plugin-sftp.md), [S3 profile](plugin-s3.md) or [Slack profile](plugin-slack.md)
instead. Their [tested examples](examples.md) start with remote delivery disabled and
read credentials from environment variables.

## Next

Try the [monthly finance pack](https://github.com/get-dre/dre/blob/master/examples/monthly-finance/README.md) for formulas, totals,
source declarations, account mappings and a filled Excel template. Read
[Build and run reports](building-reports.md) for the general workflow,
[Concepts](concepts.md) for the terminology and [CLI reference](cli-reference.md)
for all commands.

<!-- docs-nav: generated by .github/scripts/docs_sections.py from docs/sections.json -->

---

**Previous:** [Install](install.md) · **Next:** [Concepts](concepts.md)
