select account_id,
       wide_id,
       amount,
       wide_amount,
       opened,
       prepared,
       bytes,
       optional_amount as optional
from dre_fixture.dre_fixture_data
order by account_id;
