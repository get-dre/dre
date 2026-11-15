-- Synthetic Acme Corp orders, created on the report session.
create or replace temporary view acme_orders as
select * from values
    (5001, 'North', 1200.00),
    (5002, 'North', 800.00),
    (5003, 'South', 900.00)
as t(order_id, region, amount);
