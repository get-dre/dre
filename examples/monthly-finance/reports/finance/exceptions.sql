-- Rows with a negative revenue. Normally none, so the tab is left out.
select * from {{ source('finance', 'sales') }} where revenue < 0
