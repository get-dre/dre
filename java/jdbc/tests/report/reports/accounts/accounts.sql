-- The final column proves preparation and results use the same session.
alter session set nls_date_format = 'YYYY/MM/DD';

select
    cast(case when level = 1 then '000019' else '12345678901234567890' end as varchar2(20)) as "account_id",
    cast(case when level = 1 then 12.34 else null end as number(18,2)) as "balance",
    cast(date '2026-01-31' as timestamp(3)) as "opened",
    to_char(date '2026-01-31') as "session_date"
from dual
connect by level <= 2;
