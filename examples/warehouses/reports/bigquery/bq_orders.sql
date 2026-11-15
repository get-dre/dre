select region, {{ acme_sql.money('sum(amount)') }} as revenue
from unnest([
    struct(5001 as order_id, 'North' as region, 1200.00 as amount),
    struct(5002, 'North', 800.00),
    struct(5003, 'South', 900.00)
])
group by region
order by region
