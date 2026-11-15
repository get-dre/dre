-- Plain values only, for feeds that can't quote.
select order_id, region, amount from (values
    (5001, 'North', 1200.00::decimal(12,2)),
    (5002, 'North', 800.00::decimal(12,2)),
    (5003, 'South', 900.00::decimal(12,2))
) as orders(order_id, region, amount)
order by order_id
