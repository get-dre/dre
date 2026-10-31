whenever sqlerror exit failure
alter session set container=FREEPDB1;
create user dre_fixture identified by DreFixture39_x;
grant create session to dre_fixture;
exit
