select * from (values
    (1001, 'Growth', 12500.00),
    (1001, 'Cash', 800.00),
    (1002, 'Balanced', 6400.00)
) as h(member_id, fund, balance)
order by member_id, fund
