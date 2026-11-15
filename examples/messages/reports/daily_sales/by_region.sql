-- Synthetic Acme Corp sales for one day.
select * from (values
    ('North', 2000.00::decimal(12,2), 2),
    ('South', 1500.00::decimal(12,2), 2)
) as sales(region, revenue, orders)
order by region
