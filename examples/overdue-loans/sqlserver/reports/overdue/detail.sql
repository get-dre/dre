select loan_id, customer_id, customer_name, branch_name as branch, product,
       convert(varchar(10), due, 23) as due_date, days_overdue, bucket,
       cast(balance as decimal(18,2)) as balance
from #prepared where exception is null and balance > 0 order by loan_id;
