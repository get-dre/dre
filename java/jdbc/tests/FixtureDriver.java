package fixture;

import java.lang.reflect.Proxy;
import java.sql.*;
import java.util.Properties;
import java.util.logging.Logger;

/** A hostile/no-network driver used only to exercise the public subprocess boundary. */
public final class FixtureDriver implements Driver {
  public boolean acceptsURL(String url) {
    return url.startsWith("jdbc:fixture:");
  }

  public Connection connect(String url, Properties properties) throws SQLException {
    String secret = properties.getProperty("VendorCredential", "fixture-url-secret");
    System.out.println("driver stdout " + secret + " " + url);
    System.err.println("driver stderr " + secret + " " + url);
    if (url.endsWith("failure")) {
      throw new SQLException("authentication refused for " + secret + " at " + url, "28000", 1017);
    }
    if (!"true".equals(properties.getProperty("Mixed.Case"))
        || !"42".equals(properties.getProperty("number"))
        || !"12345678901234567890.125".equals(properties.getProperty("decimal"))) {
      throw new SQLException("properties changed", "HY000", 42);
    }
    DatabaseMetaData meta = (DatabaseMetaData) Proxy.newProxyInstance(
        FixtureDriver.class.getClassLoader(), new Class<?>[]{DatabaseMetaData.class},
        (proxy, method, arguments) -> switch (method.getName()) {
          case "getDatabaseProductName" -> "Fixture";
          case "getDatabaseProductVersion" -> "1.0";
          case "getIdentifierQuoteString" -> "\"";
          case "storesMixedCaseQuotedIdentifiers" -> true;
          case "storesUpperCaseIdentifiers" -> true;
          case "storesLowerCaseIdentifiers" -> false;
          case "getUserName" -> "fixture";
          case "getMaxSchemaNameLength" -> 30;
          case "getMaxTableNameLength" -> 128;
          case "getMaxColumnNameLength" -> 30;
          case "getSchemas" -> {
            if (url.contains("permission")) {
              throw new SQLException("permission denied at " + url, "42501", 1046);
            }
            yield result(new String[]{"TABLE_SCHEM"}, new Object[][]{{"SALES SCHEMA"}});
          }
          case "getTables" -> result(
              new String[]{"TABLE_CAT", "TABLE_SCHEM", "TABLE_NAME", "TABLE_TYPE", "REMARKS"},
              new Object[][]{
                  {"fixture", "SALES SCHEMA", "ORDER ITEMS", "TABLE",
                      url.contains("no-comments") ? null : "mixed case table"},
                  {"fixture", "OTHER", "ORDER ITEMS", "TABLE", "other schema"},
                  {"fixture", "SALES SCHEMA", "order view", "VIEW", "visible view"}});
          case "getColumns" -> result(
              new String[24],
              new Object[][]{
                  {"fixture", "SALES SCHEMA", "ORDER ITEMS", "Order ID", Types.NUMERIC, "NUMBER",
                      18, 0, null, null, null, url.contains("no-comments") ? null : "item key", null, null, null, null, null, null,
                      null, null, null, null, null, null},
                  {"fixture", "SALES SCHEMA", "ORDER ITEMS", "amount", Types.NUMERIC, "NUMBER",
                      18, 2, null, null, null, null, null, null, null, null, null, null,
                      null, null, null, null, null, null}});
          case "getColumnPrivileges", "getTablePrivileges", "getPrimaryKeys" ->
              throw new SQLException("optional metadata unavailable at " + url, "HY000", 777);
          default -> throw new SQLFeatureNotSupportedException("not used by this fixture");
        });
    return (Connection) Proxy.newProxyInstance(FixtureDriver.class.getClassLoader(), new Class<?>[]{Connection.class},
        (proxy, method, arguments) -> switch (method.getName()) {
          case "setAutoCommit", "close" -> null;
          case "getSchema" -> "SALES SCHEMA";
          case "getCatalog" -> "fixture";
          case "getMetaData" -> meta;
          case "createStatement" -> Proxy.newProxyInstance(FixtureDriver.class.getClassLoader(), new Class<?>[]{Statement.class},
              (statement, operation, values) -> switch (operation.getName()) {
                case "setFetchSize" -> null;
                case "execute" -> false;
                case "getLargeUpdateCount" -> 0L;
                case "close" -> throw new SQLException("close failed " + secret, "HY000", 999);
                default -> throw new SQLFeatureNotSupportedException("not used by this fixture");
              });
          default -> throw new SQLFeatureNotSupportedException("not used by this fixture");
        });
  }

  private static ResultSet result(String[] columns, Object[][] rows) {
    java.util.concurrent.atomic.AtomicInteger at = new java.util.concurrent.atomic.AtomicInteger();
    return (ResultSet) Proxy.newProxyInstance(FixtureDriver.class.getClassLoader(), new Class<?>[]{ResultSet.class},
        (proxy, method, arguments) -> {
          return switch (method.getName()) {
            case "next" -> {
              yield at.incrementAndGet() <= rows.length;
            }
            case "getString", "getObject" -> {
              if (at.get() == 0) throw new SQLException("getString before next", "HY000", 1);
              int column = (Integer) arguments[0];
              yield rows[at.get() - 1][column - 1];
            }
            case "getInt" -> {
              if (at.get() == 0) throw new SQLException("getInt before next", "HY000", 1);
              int column = (Integer) arguments[0];
              Object value = rows[at.get() - 1][column - 1];
              yield value == null ? 0 : value instanceof Number number ? number.intValue() : Integer.parseInt(value.toString());
            }
            case "close" -> null;
            default -> throw new SQLFeatureNotSupportedException("not used by this fixture");
          };
        });
  }

  public DriverPropertyInfo[] getPropertyInfo(String url, Properties properties) { return new DriverPropertyInfo[0]; }
  public int getMajorVersion() { return 1; }
  public int getMinorVersion() { return 0; }
  public boolean jdbcCompliant() { return false; }
  public Logger getParentLogger() { return Logger.getLogger("fixture"); }
}
