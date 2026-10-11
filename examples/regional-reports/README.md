# One workbook per region

Two Sets use the same SQL and supply different `region` variables.

```bash
dre validate
dre run regional --set all
dre schedule ls
```

Expected files: `out/North.xlsx` (1,200 + 800; total 2,000) and `out/South.xlsx`
(900 + 600; total 1,500). Each has a Sales tab, formatted revenue and a totals row.

`dre run regional` uses the default North Set. To rerun only South:

```bash
dre run regional --set south
```

Two Monday schedules name the report and the appropriate Set. `dre schedule ls` lists
upcoming UTC occurrences; `dre schedule run` is the foreground scheduler and belongs
under your process manager when used in production.
