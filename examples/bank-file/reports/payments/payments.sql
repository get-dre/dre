select * from (values
    ('0000000042', 'CLIENT A', 123.45::decimal(12,2), date '2026-01-31'),
    ('0000000099', 'CLIENT B', 67.89::decimal(12,2), date '2026-01-31')
) as payments(account_id, account_name, amount, posted_on)
order by account_id;
