select * from (values
    ('North', '2026-01', 'Hardware', 1200.00::decimal(12,2)),
    ('North', '2026-01', 'Services', 800.00::decimal(12,2)),
    ('South', '2026-01', 'Hardware', 900.00::decimal(12,2)),
    ('South', '2026-01', 'Services', 600.00::decimal(12,2)),
    ('North', '2026-02', 'Hardware', 1400.00::decimal(12,2)),
    ('South', '2026-02', 'Hardware', 1100.00::decimal(12,2))
) as sales(region, month, category, revenue)
where month = '{{ var("month") }}' and region = '{{ var("region") }}'
order by category;
