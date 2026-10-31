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
    return (Connection) Proxy.newProxyInstance(FixtureDriver.class.getClassLoader(), new Class<?>[]{Connection.class},
        (proxy, method, arguments) -> switch (method.getName()) {
          case "setAutoCommit", "close" -> null;
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

  public DriverPropertyInfo[] getPropertyInfo(String url, Properties properties) { return new DriverPropertyInfo[0]; }
  public int getMajorVersion() { return 1; }
  public int getMinorVersion() { return 0; }
  public boolean jdbcCompliant() { return false; }
  public Logger getParentLogger() { return Logger.getLogger("fixture"); }
}
