select account_id as "account_id",
       wide_id as "wide_id",
       amount as "amount",
       wide_amount as "wide_amount",
       opened as "opened",
       prepared as "prepared",
       bytes as "bytes",
       optional_amount as "optional",
       sessiontimezone as "session_setting",
       sys_context('USERENV', 'SID') as "session_id"
from dre_fixture.dre_fixture_data
order by account_id;
