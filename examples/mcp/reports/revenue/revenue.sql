-- DRE resolves source() and the declared region variable before MCP extracts context.
select c.region, sum(o.net_amount) as revenue
from {{ source('shop', 'orders') }} o
join {{ source('shop', 'customers') }} c on o.customer_id = c.customer_id
where '{{ var('region') }}' = 'all' or c.region = '{{ var('region') }}'
group by c.region
order by c.region
