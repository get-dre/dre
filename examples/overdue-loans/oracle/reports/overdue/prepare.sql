create private temporary table ora$ptt_prepared on commit preserve definition as
with branch_versions as (
    select b.*, row_number() over (partition by branch_id order by version desc) as rn
    from ora$ptt_branches b
), payment_versions as (
    select p.*, row_number() over (partition by repayment_id order by version desc) as rn
    from ora$ptt_repayments p
), payments as (
    select loan_id, sum(amount) as paid
    from payment_versions
    where rn = 1 and paid_on <= to_date('{{ var("report_date") }}', 'FXYYYY-MM-DD')
    group by loan_id
), eligible as (
    select l.*, c.customer_name, b.branch_name,
           cast(round(l.principal - coalesce(p.paid, 0), 2) as number(18,2)) as balance,
           cast(greatest(0, to_date('{{ var("report_date") }}', 'FXYYYY-MM-DD') - l.due) as number(10,0)) as days_overdue,
           case when l.principal is null then 'MISSING_PRINCIPAL'
                when l.principal < 0 then 'NEGATIVE_PRINCIPAL'
                when l.due is null then 'MISSING_DUE'
                when c.customer_id is null then 'MISSING_CUSTOMER'
                when b.branch_id is null then 'MISSING_BRANCH'
                when l.principal - coalesce(p.paid, 0) < 0 then 'OVERPAID'
           end as exception
    from ora$ptt_loans l
    left join ora$ptt_customers c on c.customer_id = l.customer_id
    left join branch_versions b on b.branch_id = l.branch_id and b.rn = 1
    left join payments p on p.loan_id = l.loan_id
    where l.opened <= to_date('{{ var("report_date") }}', 'FXYYYY-MM-DD')
      and (l.closed is null or l.closed > to_date('{{ var("report_date") }}', 'FXYYYY-MM-DD'))
)
select e.*,
       case when days_overdue = 0 then 'Current'
            when days_overdue <= 30 then '1-30'
            when days_overdue <= 60 then '31-60'
            when days_overdue <= 90 then '61-90'
            else 'Over 90' end as bucket
from eligible e;
