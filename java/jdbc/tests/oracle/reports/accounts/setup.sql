alter session set nls_date_format = 'YYYY/MM/DD';

delete from dre_fixture.dre_fixture_data;

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
