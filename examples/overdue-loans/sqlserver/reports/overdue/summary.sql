select concat(branch_name, ' / ', product, ' / ', bucket) as branch_product_bucket,
       count(*) as accounts, cast(sum(balance) as decimal(18,2)) as balance
from #prepared where exception is null and balance > 0
group by branch_name, product, bucket order by branch_name, product, bucket;
