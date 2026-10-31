select loan_id, exception from #prepared where exception is not null order by loan_id;
