select account_id,
       wide_id,
       amount,
       wide_amount,
       opened,
       prepared,
       bytes,
       optional
from #dre_fixture
order by account_id;
