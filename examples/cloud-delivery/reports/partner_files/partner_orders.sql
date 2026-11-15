-- The same orders, with a note column that can hold a quote, a delimiter or a formula.
select * from (values
    (5001, 'North', 1200.00::decimal(12,2), 'Rush order, "priority"'),
    (5002, 'North', 800.00::decimal(12,2), null),
    (5003, 'South', 900.00::decimal(12,2), '=HYPERLINK("x")')
) as orders(order_id, region, amount, note)
order by order_id
