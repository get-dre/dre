# Example: a data-driven subscription becomes `for_each`

`Store statement.rdl` has a `StoreId` parameter. A data-driven subscription runs a query each
month (`select StoreId, ManagerEmail from dbo.Stores where Active = 1`) and emails each manager
the report for their own store as a PDF.

In DRE the report's queries run **once** for every store, and `for_each` splits the rows by
store: one PDF and one email per row of a driver query. So the per-recipient parameter goes
away: the queries return every store's rows, with the key column.

## What was asked

1. The driver: the subscription's query, as `managers.sql` (one row per active store).
2. The key: `store_id`, an opaque number (REP-12), never the email.
3. When a row fails (one manager's address bounces): `continue`, then retry the failures.
4. Review: the statements are client-facing, so `hold: true` and `dre release` (RUN-9).
5. The relay's limit: 60 emails a minute.

## The DRE files

```yaml
# yaml-language-server: $schema=https://getdre.com/schemas/v0.6/report.schema.json
description: Each active store's monthly statement, emailed to its manager.
meta: {ported_from: /Stores/Store statement (data-driven subscription)}
queries: [managers, store_lines]
output:
  - name: statement
    format: pdf
    template: {file: store_statement.html}
    for_each: {query: managers, key: store_id, on_row_error: continue}
  - name: email
    format: message
    for_each: {query: managers, key: store_id}
    title: "{{ each.store_name }}: statement for {{ run.date.prev_month.start.date | date('mmmm yyyy') }}"
    text: "Your store's statement is attached."
    destination:
      - {profile: store_mail, to: "{{ each.manager_email }}", attach: statement, rate: 60/minute, hold: true}
```

```sql
-- managers.sql: the driver, one row per recipient (was the subscription's query)
select store_id, store_name, manager_email from shop.stores where active
```

```sql
-- store_lines.sql: every store's lines at once, with the key (was filtered by @StoreId)
select o.store_id, o.order_date, l.qty * l.unit_price as net
from shop.orders o join shop.order_lines l on l.order_id = o.order_id
where o.order_date between '{{ run.date.prev_month.start.date }}' and '{{ run.date.prev_month.end.date }}'
```

```yaml
# profiles.yml: nothing is sent while developing (RUN-8)
destinations:
  store_mail:
    targets:
      dev: {deliver: false}
      prod: {type: email, host: smtp.example.com, from: statements@example.com}
```

Trying it: `dre run store_statement --preview --only-keys 101,102` renders two stores and sends
nothing; a production run holds every email; `dre release store_statement --run <run id> --by
<reviewer>` sends them, 60 a minute.

## Parity

One store at a time: the old report exported for `StoreId=101`, compared with that store's rows
of DRE's run, which holds every store:

```bash
dre run store_statement --no-deliver --output-json parity
python3 <skill>/scripts/parity.py --dre parity/store_statement/default/store_lines.json \
  --old store_101.csv --map "StoreId=store_id,OrderDate=order_date,Net=net" \
  --only store_id=101 --key store_id,order_date
```

Repeat for two or three stores, including the largest. The driver gets its own check: the old
subscription's query and `managers.json` should list the same stores.
