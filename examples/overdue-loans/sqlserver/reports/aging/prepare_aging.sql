-- A temporary stored procedure (#name: this session only, no CREATE PROCEDURE permission
-- needed) fills a temporary table that the next query reads. Each statement below runs as
-- its own batch, so the procedure body uses no semicolons.
set nocount on;
create table #aging (bucket varchar(10) not null, loans int not null, balance decimal(18,2) not null);
create procedure #prepare_aging as
begin
    set nocount on
    insert into #aging (bucket, loans, balance)
    select bucket, count(*), cast(sum(balance) as decimal(18,2))
    from #prepared
    where exception is null and balance > 0
    group by bucket
end;
exec #prepare_aging;
