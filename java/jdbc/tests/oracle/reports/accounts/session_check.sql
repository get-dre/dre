select sessiontimezone as "session_setting",
       sys_context('USERENV', 'SID') as "session_id",
       to_char(date '2026-01-31') as "prepared_date"
from dual;
