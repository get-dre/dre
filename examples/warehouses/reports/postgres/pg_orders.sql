select region, {{ acme_sql.money('sum(amount)') }} as {{ dre_utils.quote_identifier('Revenue') }}
from {{ source('pg_session', 'orders') }}
group by region
order by region
