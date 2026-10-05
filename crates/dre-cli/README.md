<!-- Generated from README.md by .github/scripts/package_readme.py sync: edit the README. -->
# dre-cli

**DRE**, the **Declarative Reporting Engine**, is open-source reports as code. You keep each report
as `.sql` and YAML files in git; DRE runs the SQL against your databases (each tab of a workbook can
come from a different one, and tables can be declared as dbt-style sources), writes the result as
csv, delimited, fixed-width, parquet or xlsx (including multi-sheet workbooks, number formats,
formulas and totals rows, and branded Excel templates), and delivers the file wherever it needs
to go: email, SFTP/FTP, S3, GCS, Azure Blob, Databricks Volumes or Slack. A report can also send a
headline [message](https://getdre.com/docs/messages/) built from its results, to Slack or email (Microsoft Teams and
Google Chat are in preview). It runs on whatever scheduler you already have: cron, Airflow, Dagster, Databricks Jobs.

If you know dbt, you already know DRE: a project of SQL and YAML, Jinja and macros, `ref()`,
`source()`, folder config, tags, selectors, profiles and targets. dbt builds your tables; DRE
delivers the last mile, the reports people receive. DRE is inspired by dbt and is an independent
project, not affiliated with or endorsed by dbt Labs, Inc. (dbt is their trademark).
[DRE and dbt, side by side](https://getdre.com/dbt/).

```bash
cargo install dre-cli --locked
dre --help
```

Sources, formats and destinations are plugins that `dre` downloads on demand for the projects that
declare them. Other ways to install DRE (install.sh, pip, Homebrew, Scoop) are in the
[install guide](https://getdre.com/docs/install/).

Documentation: [getdre.com/docs](https://getdre.com/docs/). Source and issues: [github.com/get-dre/dre](https://github.com/get-dre/dre).
