select sum(amount) as value, count(*) as orders
from (values (1200.00), (800.00), (900.00), (600.00)) as t(amount)
