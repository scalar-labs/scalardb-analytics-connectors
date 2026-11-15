import java.sql.Connection;
import java.sql.DriverManager;
import java.sql.ResultSet;
import java.sql.Statement;
import java.util.Properties;

/**
 * Loads the packaged DuckDB extension through DuckDB's JDBC driver, a host that
 * exports none of DuckDB's C++ symbols, and checks that DuckDB reports it
 * loaded.
 *
 * <p>Usage: {@code java -cp <duckdb_jdbc jar> LoadExtension.java <extension file>}
 */
public class LoadExtension {
  public static void main(String[] args) throws Exception {
    Properties properties = new Properties();
    properties.setProperty("allow_unsigned_extensions", "true");
    try (Connection connection = DriverManager.getConnection("jdbc:duckdb:", properties);
        Statement statement = connection.createStatement()) {
      statement.execute("LOAD '" + args[0].replace("'", "''") + "'");
      try (ResultSet result =
          statement.executeQuery(
              "SELECT loaded FROM duckdb_extensions()"
                  + " WHERE extension_name = 'scalardb_analytics_connectors'")) {
        if (!result.next() || !result.getBoolean(1)) {
          throw new IllegalStateException("DuckDB does not report the extension loaded");
        }
      }
    }
  }
}
