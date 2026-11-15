-- Revenue by region from the temporary view prepared on this session.
select region, {{ acme_sql.money('sum(amount)') }} as {{ dre_utils.quote_identifier('Revenue') }}
from acme_orders
group by region
order by region
