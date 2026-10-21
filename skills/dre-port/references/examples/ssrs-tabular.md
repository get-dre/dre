# Example: a tabular SSRS report

`Sales by region.rdl`: one dataset on SQL Server, two date parameters, one table, emailed weekly.

```xml
<DataSet Name="Sales">
  <Query>
    <DataSourceName>Warehouse</DataSourceName>
    <CommandText>SELECT r.RegionName, COUNT(*) AS Orders, SUM(l.Qty * l.UnitPrice) AS Net
FROM dbo.Orders o JOIN dbo.OrderLines l ON l.OrderId = o.OrderId
JOIN dbo.Regions r ON r.RegionId = o.RegionId
WHERE o.OrderDate &gt;= @StartDate AND o.OrderDate &lt; @EndDate
GROUP BY r.RegionName</CommandText>
  </Query>
</DataSet>
<ReportParameter Name="StartDate"><DataType>DateTime</DataType></ReportParameter>
<ReportParameter Name="EndDate"><DataType>DateTime</DataType></ReportParameter>
```

Subscription (from the catalog export): email to `sales@example.com`, "Weekly on Monday at
07:00", rendered as Excel.

## What was asked

- Where the tables live: the same tables are replicated to Postgres (`sales.orders`, ...).
- The dates: the subscription always ran for the previous week, so the parameters become
  `run.date.prev_week` (REP-2) rather than variables someone has to set. The user agreed.
- Format: xlsx, as the subscription sent.

## The DRE files

```yaml
# yaml-language-server: $schema=https://getdre.com/schemas/v0.6/report.schema.json
# reports/sales/sales_by_region/sales_by_region.yml
description: Orders and net sales by region for the previous week (Monday to Sunday).
meta: {ported_from: /Sales/Sales by region}
queries:
  - query: sales_by_region
    tab_name: Sales
    columns:
      net: {format: "#,##0.00", total: sum}
      orders: {total: sum}
output:
  format: xlsx
  destination:
    - profile: sales_mail
      to: [sales@example.com]
      subject: "Sales by region, week of {{ run.date.prev_week.start.date }}"
      hold: true
```

```yaml
# sources/sales.yml: the replicated tables, on the `warehouse` connection (REP-7)
sources:
  - name: sales
    profile: warehouse
    tables: [{name: orders}, {name: order_lines}, {name: regions}]
```

```sql
-- reports/sales/sales_by_region/sales_by_region.sql
select r.region_name, count(*) as orders, sum(l.qty * l.unit_price) as net
from {{ source('sales', 'orders') }} o
join {{ source('sales', 'order_lines') }} l on l.order_id = o.order_id
join {{ source('sales', 'regions') }} r on r.region_id = o.region_id
where o.order_date >= '{{ run.date.prev_week.start.date }}'
  and o.order_date <= '{{ run.date.prev_week.end.date }}'
group by r.region_name
order by r.region_name
```

```yaml
# schedules.yml
- {name: sales_by_region_weekly, report: sales_by_region, cron: "0 7 * * MON", timezone: Australia/Sydney}
```

`hold: true` stays until the owner signs off the first runs (PORT-4); the old subscription stays
on for one parallel week (PORT-2).

## Parity

```bash
DRE_RUN_DATE=2026-10-05 dre run sales_by_region --no-deliver --output-json parity
python3 <skill>/scripts/parity.py --dre parity/sales_by_region/default/sales_by_region.json \
  --old "Sales by region.csv" --key region_name --map "RegionName=region_name"
```

The old export ran with `StartDate=2026-09-28`, `EndDate=2026-10-05` (exclusive), the same week.
Result: MATCH, with `net` differing only as formatting (`1,234.50` against `1234.5`).
