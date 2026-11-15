select * from (values
    ('north', 'Hardware', 1200.00::decimal(12,2)),
    ('north', 'Services', 800.00::decimal(12,2)),
    ('south', 'Hardware', 900.00::decimal(12,2)),
    ('south', 'Services', 600.00::decimal(12,2))
) as s(region_id, category, revenue)
order by region_id, category
