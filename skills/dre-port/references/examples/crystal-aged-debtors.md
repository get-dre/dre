# Example: a Crystal Reports aged debtors report

An XML export of `Aged debtors.rpt`'s definition: tables `Customer` and `Invoice` linked on
`CustomerID`, a record selection `{Invoice.Balance} <> 0`, a parameter `{?AsAt}` (a date),
grouped on `{Customer.Region}` with `Sum ({Invoice.Balance}, {Customer.Region})`, and two
formulas:

```text
{@DaysOverdue}  DateDiff("d", {Invoice.DueDate}, {?AsAt})
{@Bucket}       if {@DaysOverdue} <= 30 then "Current" else if {@DaysOverdue} <= 60 then "31-60" else "60+"
```

## What was asked

1. Where the tables live: Snowflake, `FINANCE.AR.CUSTOMER` and `FINANCE.AR.INVOICE`.
2. `AsAt`: the last day of the previous month unless passed (`var_schema` type `date`).
3. Format: finance work in Excel, so xlsx: a detail tab and a summary tab by region and bucket.

## The DRE files

```yaml
# yaml-language-server: $schema=https://getdre.com/schemas/v0.6/report.schema.json
description: Open invoice balances by customer, aged at a date, with a summary by region and bucket.
meta: {ported_from: Crystal Reports, Aged debtors.rpt}
var_schema:
  as_at: {type: date, description: The ageing date; default the last day of last month}
queries:
  - {query: aged_detail, tab_name: Detail, columns: {balance: {format: "#,##0.00", total: sum}}}
  - {query: aged_summary, tab_name: Summary, columns: {balance: {format: "#,##0.00", total: sum}}}
output:
  format: xlsx
  date_format: "dd/mm/yyyy"
```

```sql
-- aged_detail.sql: the formulas are SQL columns (PORT-1)
{% set as_at = var('as_at', run.date.prev_month.end.date) %}
select c.region, c.name as customer, i.due_date, i.balance,
       datediff(day, i.due_date, '{{ as_at }}'::date) as days_overdue,
       case when datediff(day, i.due_date, '{{ as_at }}'::date) <= 30 then 'Current'
            when datediff(day, i.due_date, '{{ as_at }}'::date) <= 60 then '31-60'
            else '60+' end as bucket
from {{ source('ar', 'invoice') }} i
join {{ source('ar', 'customer') }} c on c.customer_id = i.customer_id     -- the table link
where i.balance <> 0                                                        -- the record selection
order by c.region, c.name, i.due_date
```

`aged_summary.sql` groups the same rows by `region, bucket` (the Crystal group summary).

## Parity

Crystal exported the report for `AsAt = 30/09/2026` as "Excel Workbook Data-only": a title row,
then the header.

```bash
dre run aged_debtors --var as_at=2026-09-30 --no-deliver --output-json parity
python3 <skill>/scripts/parity.py --dre parity/aged_debtors/default/aged_detail.json \
  --old aged_debtors_sep.xlsx --skip-rows 1 --key customer,due_date \
  --map "Name=customer,DaysOverdue=days_overdue,Bucket=bucket"
```

Result: MATCH, with `due_date` differing only as formatting (`30/09/2026` against `2026-09-30`).
