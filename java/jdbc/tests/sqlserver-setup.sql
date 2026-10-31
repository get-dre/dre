IF SUSER_ID(N'dre_fixture') IS NULL
    CREATE LOGIN dre_fixture WITH PASSWORD = 'DreFixture39_x!', CHECK_POLICY = OFF;
GO
IF USER_ID(N'dre_fixture') IS NULL
    CREATE USER dre_fixture FOR LOGIN dre_fixture;
GO
ALTER ROLE db_datareader ADD MEMBER dre_fixture;
GO
IF SCHEMA_ID(N'Sales Schema') IS NULL EXEC(N'CREATE SCHEMA [Sales Schema]');
GO
IF OBJECT_ID(N'[Sales Schema].[Order Items]', N'U') IS NOT NULL DROP TABLE [Sales Schema].[Order Items];
GO
IF OBJECT_ID(N'[Sales Schema].[Order View]', N'V') IS NOT NULL DROP VIEW [Sales Schema].[Order View];
GO
CREATE TABLE [Sales Schema].[Order Items] (
  [Order ID] decimal(18,0) NULL,
  amount decimal(18,2) NULL
);
GO
EXEC sys.sp_addextendedproperty @name=N'MS_Description', @value=N'mixed case table', @level0type=N'SCHEMA', @level0name=N'Sales Schema', @level1type=N'TABLE', @level1name=N'Order Items';
EXEC sys.sp_addextendedproperty @name=N'MS_Description', @value=N'mixed case amount', @level0type=N'SCHEMA', @level0name=N'Sales Schema', @level1type=N'TABLE', @level1name=N'Order Items', @level2type=N'COLUMN', @level2name=N'amount';
GO
CREATE VIEW [Sales Schema].[Order View] AS SELECT [Order ID] FROM [Sales Schema].[Order Items];
GO
