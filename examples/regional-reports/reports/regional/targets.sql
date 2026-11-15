select * from (values
    ('North', 1800.00::decimal(12,2), 'north.manager@acme.example'),
    ('South', 1600.00::decimal(12,2), 'south.manager@acme.example')
) as targets(region, target, manager_email)
where region = '{{ var("region") }}' or '{{ var("region") }}' = 'All'
order by region;
