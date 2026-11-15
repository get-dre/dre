-- Install once, as the reporting account, with SQL*Plus or SQL Developer (needs CREATE TABLE
-- and CREATE PROCEDURE). DRE runs SQL statement by statement, so a PL/SQL body is created
-- here and only called from the report.
create global temporary table acme_aging (
    bucket varchar2(10) not null,
    loans number(10,0) not null,
    balance number(18,2) not null
) on commit preserve rows;

-- Fills this session's acme_aging from the report's prepared relation. The private temporary
-- table exists only in the calling session, so it is read through dynamic SQL.
create or replace procedure acme_prepare_aging as
begin
    delete from acme_aging;
    execute immediate q'[
        insert into acme_aging (bucket, loans, balance)
        select bucket, count(*), cast(sum(balance) as number(18,2))
        from ora$ptt_prepared
        where exception is null and balance > 0
        group by bucket]';
end;
/
