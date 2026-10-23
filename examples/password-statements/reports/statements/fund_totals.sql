select fund, sum(balance) as balance
from (values ('Growth', 12500.00), ('Cash', 800.00), ('Balanced', 6400.00)) as h(fund, balance)
group by fund
order by fund
