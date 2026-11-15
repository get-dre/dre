-- Synthetic Acme Corp orders in a session temporary table, then revenue by region.
create temporary table acme_orders engine = Memory as
select * from (
    select 5001 as order_id, 'North' as region, 1200.00 as amount
    union all select 5002, 'North', 800.00
    union all select 5003, 'South', 900.00
);
select region, {{ acme_sql.money('sum(amount)') }} as {{ dre_utils.quote_identifier('Revenue') }}
from acme_orders
group by region
order by region
