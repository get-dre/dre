select region, sum(revenue) as revenue
from {{ source('finance', 'sales') }}
where month = '{{ var("month") }}'
group by region order by region;
