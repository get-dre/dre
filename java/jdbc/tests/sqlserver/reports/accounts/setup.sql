set nocount on;
set datefirst 1;
set dateformat dmy;
if object_id('tempdb..#dre_fixture') is not null drop table #dre_fixture;
create table #dre_fixture (
  account_id varchar(20) not null,
  wide_id varchar(40) not null,
  amount decimal(18,2) not null,
  wide_amount decimal(38,18) not null,
  opened datetime2(3) not null,
  prepared varchar(10) not null,
  bytes varbinary(8) null,
  optional decimal(18,2) null
);

insert into #dre_fixture values
  ('000019', '1234567890123456789012345678901234567890', 12.34,
   12345678901234567890.123456789012345678, '2026-01-31T01:02:03.123',
   '2026/01/31', 0x002AFF, null),
  ('000020', '9876543210987654321098765432109876543210', 0.00,
   -0.000000000000000001, '2026-02-01T00:00:00.000',
   '2026/02/01', null, 99.99);
