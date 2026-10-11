select s.*, a.account_code, revenue * 0.1 as commission
from {{ source('finance', 'sales') }} s
join {{ ref('accounts') }} a using (category)
where month = '{{ var("month") }}'
order by region, category;
