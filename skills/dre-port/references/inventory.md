# Inventory: a folder of reports

Before porting an estate (tens or hundreds of reports), list them, so the user sees the size of
the job, what's similar, and where to start.

```bash
python3 <skill>/scripts/inventory.py <folder> --subscriptions subscriptions.csv
python3 <skill>/scripts/inventory.py <folder> --json        # everything, for you to read
```

It reads every `.rdl` and `.rdlc` (SSRS, Power BI paginated, Report Builder; any RDL version)
and every Crystal XML export of a report definition under the folder, and lists binary `.rpt`
files without reading them. For each report:

- **sources:** its data sources (a shared reference or a server), or Crystal's tables;
- **datasets:** each query, and whether it's text, a stored procedure or a shared dataset;
- **parameters:** `[]` marks a multi-value one;
- **groups**, expressions, Crystal formulas, page header/footer, subreports, charts;
- **subscriptions**, from the csv;
- **effort:** S, M, L or XL, with the score and its reasons (stored procedures, custom code,
  heavy expressions such as `Lookup` or `RunningValue`, charts, data-driven subscriptions).

Then **candidate Sets**, reports whose datasets, fields, parameters and groups are nearly the
same (a copy per region or client), which become one DRE report with a Set each (REP-4); and a
**suggested order**: the easiest first (an early win, and the shared connection and conventions
are settled on a simple report), each group together (PORT-3).

The effort is a score from the file, not a promise. Say so, and adjust it with what the user
knows ("that one's used by the regulator", "nobody opens that one").

## Ask what's still used

Before porting everything, ask whether they know which reports are still used (the SSRS
execution log: `ExecutionLog3` in the catalog database has each run's report, user and time).
Recommend porting only what ran in the last few months.

## Exporting SSRS subscriptions

SSRS keeps subscriptions and schedules in its catalog database (`ReportServer`), not in the
`.rdl` files. The user, or their DBA, runs this read-only query there and saves the result as
`subscriptions.csv`. Don't run it against their server yourself, and don't ask for its
credentials. The catalog's tables are internal to SSRS and can differ between versions: if the
query fails, ask for the subscription list another way (the web portal's "My subscriptions"
page, or a screenshot).

```sql
select
  c.Path as report,
  s.Description as description,
  case when s.DeliveryExtension like '%Email%' then 'email'
       when s.DeliveryExtension like '%FileShare%' then 'file share'
       else s.DeliveryExtension end as delivery,
  sch.Name as schedule,
  case when s.DataSettings is null then 'false' else 'true' end as data_driven,
  cast(s.ExtensionSettings as nvarchar(max)) as recipients
from dbo.Subscriptions s
join dbo.Catalog c on c.ItemID = s.Report_OID
left join dbo.ReportSchedule rs on rs.SubscriptionID = s.SubscriptionID
left join dbo.Schedule sch on sch.ScheduleID = rs.ScheduleID
order by c.Path
```

`recipients` holds the subscription's settings as XML (the `TO`, `CC` and `Subject` values, or
the file share path); read them from there when porting. The schedule's name is often a shared
schedule ("Monthly 6am"); ask for its exact recurrence and time zone when porting.

A Crystal Server or BusinessObjects estate: ask for the schedules and publications as a list or
screenshots; the csv columns above are the shape to fill.
