select bucket, loans, balance
from #aging
order by case bucket when 'Current' then 0 when '1-30' then 1 when '31-60' then 2
                     when '61-90' then 3 else 4 end
