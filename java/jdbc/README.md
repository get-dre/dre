# JDBC source (development build)

An optional Java subprocess source using DRE's existing protocol and Apache Arrow's JDBC
adapter. No JVM runs inside DRE core. See [the JDBC guide](../../docs/plugin-jdbc.md) for
configuration, supported scope and the Oracle fixture.

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
database. The shared protocol/Oracle integration tests live in
[`crates/dre-protocol/tests/jdbc.rs`](../../crates/dre-protocol/tests/jdbc.rs). CI runs them
against a pinned Oracle Free container, then `test_oracle_report.py` runs the ordinary CLI and
checks the workbook with the existing public-example checker. The test driver downloaded by CI
is never included in the plugin JAR.

Code is GPL-3.0-only, as with every DRE plugin. Apache Arrow and Jackson use Apache-2.0;
SLF4J uses MIT. Supply appropriately licensed vendor drivers yourself.
