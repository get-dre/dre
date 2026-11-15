select region, {{ acme_sql.money('target') }} as target
from {{ source('local_targets', 'targets') }}
order by region
