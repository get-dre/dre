# Your first report

A completed, self-contained DuckDB project. Six synthetic sales rows are embedded in SQL;
no database file, login or seed command is required.

```bash
dre validate
dre run sales --set all
dre schedule ls
```

Expected local delivery: `out/North.xlsx` and `out/South.xlsx`, each with Sales and Summary
tabs. January revenue is 2,000 for North and 1,500 for South. The weekday schedule lists
its next UTC occurrences; listing does not start a scheduler.

Follow [Your first report in 10 minutes](../../docs/first-report.md) to build it one change
at a time. `.steps/` holds the exact changed files for each stage; its leading dot keeps
those snapshots out of DRE's project discovery. `prepare.py` copies them into a separate
tutorial project, preserving outputs and run history between steps.

The final project and each stage are exercised by the examples CI runner.
