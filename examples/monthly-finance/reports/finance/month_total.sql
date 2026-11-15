select sum(revenue) as revenue from {{ source('finance', 'sales') }} where month = '{{ var("month") }}'
