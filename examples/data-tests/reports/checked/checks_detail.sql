select nextval('checks_calls') as execution, amount from (values (10), (20)) t(amount)
