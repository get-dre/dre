whenever sqlerror exit failure
alter session set container=FREEPDB1;
create user dre_fixture identified by DreFixture39_x;
grant create session, create table to dre_fixture;
alter user dre_fixture quota unlimited on users;
create table dre_fixture.dre_fixture_data (
  account_id varchar2(40 char),
  wide_id varchar2(40 char),
  amount number(18,2),
  wide_amount number(38,18),
  opened timestamp(3),
  prepared varchar2(20),
  bytes raw(3),
  optional_amount number(18,2)
);
insert into dre_fixture.dre_fixture_data values (
  '000019', '1234567890123456789012345678901234567890', 12.34,
  12345678901234567890.123456789012345678, timestamp '2026-01-31 01:02:03.123',
  '2026/01/31', hextoraw('002AFF'), null
);
insert into dre_fixture.dre_fixture_data values (
  '000020', '9876543210987654321098765432109876543210', 0.00,
  -0.000000000000000001, timestamp '2026-02-01 00:00:00.000',
  '2026/02/01', null, 99.99
);
commit;
exit
