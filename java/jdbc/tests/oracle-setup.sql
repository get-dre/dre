whenever sqlerror exit failure
alter session set container=FREEPDB1;
create user dre_fixture identified by DreFixture39_x;
grant create session, create table to dre_fixture;
create global temporary table dre_fixture.dre_fixture_data (
  account_id varchar2(40 char),
  wide_id varchar2(40 char),
  amount number(18,2),
  wide_amount number(38,18),
  opened timestamp(3),
  prepared varchar2(20),
  bytes raw(3),
  optional_amount number(18,2)
) on commit preserve rows;
comment on table dre_fixture.dre_fixture_data is 'visible fixture table';
comment on column dre_fixture.dre_fixture_data.amount is 'fixture amount';
create table dre_fixture."Order Items" (
  "Order ID" number(18,0),
  amount number(18,2)
);
comment on table dre_fixture."Order Items" is 'mixed case table';
comment on column dre_fixture."Order Items".amount is 'mixed case amount';
create view dre_fixture."Order View" as select "Order ID" from dre_fixture."Order Items";
exit
