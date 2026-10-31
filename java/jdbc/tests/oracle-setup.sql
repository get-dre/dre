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
exit
