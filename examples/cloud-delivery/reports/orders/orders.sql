-- Synthetic Acme Corp orders for one day.
select * from (values
    (5001, date '2026-01-31', 'North', 'Hardware', 1200.00::decimal(12,2)),
    (5002, date '2026-01-31', 'North', 'Services', 800.00::decimal(12,2)),
    (5003, date '2026-01-31', 'South', 'Hardware', 900.00::decimal(12,2))
) as orders(order_id, order_date, region, category, amount)
where order_date = date '{{ var("day") }}'
order by order_id
