# JDBC source (development build)

An optional Java subprocess source using DRE's existing protocol and Apache Arrow's JDBC
adapter. No JVM runs inside DRE core. See [the JDBC guide](../../docs/plugin-jdbc.md) for
configuration, supported scope and the tested Oracle and SQL Server fixtures.

Build with Java 17 or newer and Maven 3.9 or newer:

```sh
mvn -B -f java/jdbc/pom.xml package
python3 java/jdbc/test_protocol.py
```

The POSIX development launcher is `java/jdbc/dre-plugin-jdbc`. It uses `JAVA_HOME/bin/java`
when set, otherwise `java` from `PATH`, and reads `target/dre-plugin-jdbc.jar` beside itself.
It can be copied with that JAR into a development plugins directory. Windows packaging,
bundled Java and registry installation are separate work; this is not an installable release.

`test_protocol.py` builds a synthetic driver and exercises the subprocess boundary without a
database. The shared Oracle and SQL Server protocol tests live in
[`crates/dre-protocol/tests/jdbc.rs`](../../crates/dre-protocol/tests/jdbc.rs). Set
`DRE_TEST_JDBC_DATABASE=oracle|sqlserver` with `DRE_TEST_JDBC_URL`, `DRE_TEST_JDBC_JAR`,
`DRE_TEST_JDBC_USER` and `DRE_TEST_JDBC_PASSWORD` to run them. CI runs both against pinned
databases, then `test_jdbc_report.py` checks the ordinary CLI workbook. The test drivers
downloaded by CI are never included in the plugin JAR.

Code is GPL-3.0-only, as with every DRE plugin. Apache Arrow and Jackson use Apache-2.0;
SLF4J uses MIT. Supply appropriately licensed vendor drivers yourself.
