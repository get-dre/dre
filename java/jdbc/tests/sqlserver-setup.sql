IF SUSER_ID(N'dre_fixture') IS NULL
    CREATE LOGIN dre_fixture WITH PASSWORD = 'DreFixture39_x!', CHECK_POLICY = OFF;
GO
IF USER_ID(N'dre_fixture') IS NULL
    CREATE USER dre_fixture FOR LOGIN dre_fixture;
GO
ALTER ROLE db_datareader ADD MEMBER dre_fixture;
GO
