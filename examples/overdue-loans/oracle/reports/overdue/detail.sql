select loan_id as "loan_id", customer_id as "customer_id", customer_name as "customer_name",
       branch_name as "branch", product as "product", to_char(due, 'YYYY-MM-DD') as "due_date",
       cast(days_overdue as number(10,0)) as "days_overdue", bucket as "bucket",
       cast(balance as number(18,2)) as "balance"
from ora$ptt_prepared
where exception is null and balance > 0
order by loan_id;
