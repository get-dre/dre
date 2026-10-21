# Example: groups, subtotals, a stored procedure and a multi-value parameter

`Invoice summary.rdl`: a table grouped by region then store with a subtotal per group, a
stored procedure as its dataset, a `Month` parameter defaulting to last month, a multi-value
`Region` parameter whose values come from a query, `Format(..., "C2")` and an `IIf` in its cells,
and "Page N of M" in the footer. It's sent as a PDF.

## What was asked

1. The procedure's source (the DBA sent `dbo.usp_InvoiceSummary`): it filters invoices by month
   and a comma-separated region list, joins stores and regions, and returns detail rows. Its
   logic becomes SQL on the warehouse (Postgres here).
2. Format: the report is read on paper, so pdf, with the groups and subtotals as in the old
   layout (the user sent the old PDF).
3. `Region`: a list variable defaulting to every region. DRE's `allowed` checks a list as a
   whole, so each code is escaped in the SQL instead (see the mapping's Parameters).
4. `Month`: always last month in the subscription, so `period('last_month')` from `run.date`
   (REP-2). A rerun for another month pins the date: `DRE_RUN_DATE=2026-10-01` gives September.

## The DRE files

```yaml
# yaml-language-server: $schema=https://getdre.com/schemas/v0.6/report.schema.json
description: Invoices for one month by region and store, with subtotals; credits marked.
meta: {ported_from: /Finance/Invoice summary}
vars:
  regions: [NORTH, SOUTH, EAST, WEST]
var_schema:
  regions: {type: array, description: "Region codes to include: NORTH, SOUTH, EAST, WEST"}
queries:
  - query: invoices
    columns: {amount: {format: "$#,##0.00"}}      # was Format(..., "C2"); table() and row.fmt() use it
  - query: subtotals
    columns: {amount: {format: "$#,##0.00"}}
output:
  format: pdf
  template: {file: invoice_summary.html}
  on_empty: skip
```

```sql
-- invoices.sql: the detail rows, one per invoice
{% set m = period('last_month') %}
select r.region_name, s.store_name, i.invoice_no, i.amount,
       case when i.amount < 0 then 'Credit' else 'Invoice' end as kind      -- was IIf(...)
from billing.invoices i
join billing.stores s on s.store_id = i.store_id
join billing.regions r on r.region_id = s.region_id
where i.invoice_date between '{{ m.start.date }}' and '{{ m.end.date }}'
  and r.region_code in ({% for c in var('regions') %}'{{ c | replace("'", "''") }}'{% if not loop.last %}, {% endif %}{% endfor %})
  and r.region_name <> 'Head office'                                         -- was the dataset filter
order by r.region_name, s.store_name, i.invoice_no
```

```sql
-- subtotals.sql: one row per store, per region and the grand total (REP-10)
{% set m = period('last_month') %}
select r.region_name, s.store_name, sum(i.amount) as amount,
       grouping(r.region_name) as is_total, grouping(s.store_name) as is_region
from billing.invoices i
join billing.stores s on s.store_id = i.store_id
join billing.regions r on r.region_id = s.region_id
where i.invoice_date between '{{ m.start.date }}' and '{{ m.end.date }}'
  and r.region_code in ({% for c in var('regions') %}'{{ c | replace("'", "''") }}'{% if not loop.last %}, {% endif %}{% endfor %})
  and r.region_name <> 'Head office'
group by rollup (r.region_name, s.store_name)
```

The template (`invoice_summary.html`) loops `results.invoices.rows`, prints a region heading and
a store heading when they change, and after each store's rows looks up that store's subtotal in
`results.subtotals.rows` (and the region's, and the grand total at the end), showing each with
`row.fmt('amount')`. Its `@page` rule has `@bottom-center { content: "Page " counter(page) " of "
counter(pages); }`, and `break-before: page` on each region heading, as the old report did.

For a fiscal calendar, ask how its months are defined: `period()` knows calendar periods only.

## Parity

The old export: the report run for September with every region, exported as csv. Its csv
flattened the groups and added the subtotals as extra columns (`Textbox7`), so the detail is
compared on `invoice_no` and the subtotal columns are ignored:

```bash
DRE_RUN_DATE=2026-10-01 dre run invoice_summary --no-deliver --output-json parity
python3 <skill>/scripts/parity.py --dre parity/invoice_summary/default/invoices.json \
  --old invoice_summary_sep.csv --key invoice_no --map "Textbox7=,Textbox8=amount,Textbox9=kind"
```

First result: DIFFERENT, 3 keys only in the old export. They were invoices dated on 30
September at 23:30 local time: the warehouse stores UTC, so they fell into October. The fix
compares the local date (`invoice_date_local`); the second run matched.
