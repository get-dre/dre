-- Demo pipeline output. In production, dbt or another pipeline owns these comments.
create table orders(order_id integer, customer_id integer, net_amount decimal(12,2));
create table customers(customer_id integer, region varchar);
insert into orders values (1001, 1, 10.50), (1002, 1, 20.25), (1003, 2, 5.00);
insert into customers values (1, 'North'), (2, 'South');
comment on table orders is 'Harbour Outfitters pipeline model. Grain: one row per order. Net of GST; currency AUD.';
comment on column orders.order_id is 'Unique order identifier';
comment on column orders.customer_id is 'Customer key; joins customers.customer_id';
comment on column orders.net_amount is 'Order net revenue in AUD, excluding GST';
comment on table customers is 'Harbour Outfitters pipeline dimension. Grain: one row per customer.';
comment on column customers.customer_id is 'Unique customer key';
comment on column customers.region is 'Customer sales region at reporting time';

-- An optional extra pipeline table: not referenced by the reviewed revenue report.
-- It becomes available to MCP only if explicitly allowlisted in .dre-mcp.yml.
create table region_targets(region varchar, target_amount decimal(12,2));
insert into region_targets values ('North', 50.00), ('South', 10.00);
comment on table region_targets is 'Pipeline planning model. Grain: one row per region. Targets are net revenue in AUD excluding GST.';
comment on column region_targets.region is 'Region key matching customers.region';
comment on column region_targets.target_amount is 'Planned net revenue target in AUD excluding GST';

select 1 as ready;
