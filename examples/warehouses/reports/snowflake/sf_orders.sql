select region, {{ acme_sql.money('sum(amount)') }} as revenue
from (values
    (5001, 'North', 1200.00),
    (5002, 'North', 800.00),
    (5003, 'South', 900.00)
) as t(order_id, region, amount)
group by region
order by region
