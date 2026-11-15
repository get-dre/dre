create table region_targets as
select * from (values ('North', 1800.00), ('South', 1000.00)) as t(region, target);
