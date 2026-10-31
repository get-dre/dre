select account_id,
       wide_id,
       amount,
       wide_amount,
       opened,
       prepared,
       bytes,
       optional,
       @@datefirst as session_setting,
       cast(@@spid as varchar(20)) as session_id
from #dre_fixture
order by account_id;
