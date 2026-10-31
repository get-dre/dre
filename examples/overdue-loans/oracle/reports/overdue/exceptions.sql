select loan_id as "loan_id", exception as "exception"
from ora$ptt_prepared where exception is not null order by loan_id;
