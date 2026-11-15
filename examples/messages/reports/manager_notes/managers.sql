-- One row per regional manager: the driver of the per-row message.
select * from (values
    ('north', 'North', 'north.manager@acme.example'),
    ('south', 'South', 'south.manager@acme.example')
) as m(region_id, region, email)
order by region_id
