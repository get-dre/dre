select branch_name || ' / ' || product || ' / ' || bucket as "branch_product_bucket",
       cast(count(*) as number(10,0)) as "accounts", cast(sum(balance) as number(18,2)) as "balance"
from ora$ptt_prepared
where exception is null and balance > 0
group by branch_name, product, bucket
order by branch_name, product, bucket;
