-- Revenue against each region's target, with the category commission rates from Excel.
select t.region, t.target, t.headcount, sum(s.revenue) as revenue,
       sum(s.revenue * c.commission_rate) as commission
from {{ ref('region_targets') }} t
join {{ source('finance', 'sales') }} s on s.region = t.region and s.month = '{{ var("month") }}'
join {{ ref('commission_rates') }} c on c.category = s.category
where t.active
group by t.region, t.target, t.headcount
order by t.region
