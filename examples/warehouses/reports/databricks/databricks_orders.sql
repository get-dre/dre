-- Synthetic Acme Corp orders in a temporary view, then revenue by region.
create or replace temporary view acme_orders as
select * from values
    (5001, 'North', 1200.00),
    (5002, 'North', 800.00),
    (5003, 'South', 900.00)
as t(order_id, region, amount);
select region, {{ acme_sql.money('sum(amount)') }} as {{ dre_utils.quote_identifier('Revenue') }}
from acme_orders
group by region
order by region
