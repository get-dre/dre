# JDBC source

An optional Java subprocess source using DRE's existing protocol and Apache Arrow's JDBC
adapter. The user supplies Java 17 or newer; no JVM runs inside DRE core.
See [the JDBC guide](../../docs/plugin-jdbc.md) for installation, configuration, supported scope
and the tested Oracle and SQL Server fixtures.

Install it from a project's `dependencies.yml` like any other plugin:

```yaml
plugins: [jdbc]
```

```sh
dre deps
```

The release packages support macOS and Linux on x86_64 or aarch64, and Windows x64. They contain
the launcher, plugin JAR and notices. Windows ARM64 has no package for this release.
`JAVA_HOME/bin/java` wins when set, otherwise the launcher uses `java` from `PATH`. Compatible
Java versions from 17 upward are accepted without an upper cap; check the vendor driver's own
Java requirements. The operator installs and patches Java; the plugin never downloads it.

Vendor JDBC driver JARs are always supplied by the user. See the guide for the exact profile
fields and tested dialect examples.

## Build and test

Build with Java 17 or newer and Maven 3.9 or newer:

```sh
mvn -B -f java/jdbc/pom.xml package
python3 java/jdbc/test_protocol.py
```

The POSIX development launcher is `java/jdbc/dre-plugin-jdbc`. It uses `JAVA_HOME/bin/java`
when set, otherwise `java` from `PATH`, and reads `target/dre-plugin-jdbc.jar` beside itself.
It can be copied with that JAR into a development plugins directory. The packaged launcher is
`java/jdbc/launcher/dre-plugin-jdbc`: it selects installed Java the same way, checks the minimum
version, and reads `dre-plugin-jdbc.jar` beside itself. Windows uses the native launcher in
the same directory.

Release packaging is `.github/scripts/package_jdbc.py`; run `python3 java/jdbc/test_package.py`
to check the deterministic archive and launcher contract without a database. The release job
uses the runner's Java to build with Maven 3.9.11 and packages the launcher, JAR and notices.
Release checks unpack and launch each final archive using installed Java. A Linux
install/update test runs a report using a test-only H2 driver through the normal registry and
lock workflow. Grype scans the shaded Java dependencies in final archive SBOMs before
publication. Windows ties Java to the launcher's job so forced
termination also ends the runtime and its database connections.

`test_protocol.py` builds a synthetic driver and exercises the subprocess boundary without a
database, including catalog metadata and diagnostics. The shared Oracle and SQL Server protocol
tests live in
[`crates/dre-protocol/tests/jdbc.rs`](../../crates/dre-protocol/tests/jdbc.rs). Set
`DRE_TEST_JDBC_DATABASE=oracle|sqlserver` with `DRE_TEST_JDBC_URL`, `DRE_TEST_JDBC_JAR`,
`DRE_TEST_JDBC_USER` and `DRE_TEST_JDBC_PASSWORD` to run them. CI runs both against pinned
databases, then `test_jdbc_report.py` checks the ordinary CLI workbook. The test drivers
downloaded by CI are never included in the plugin JAR.
The report fixture uses a preparation query followed by two result queries. It checks the
same database session ID and session settings in both outputs, including Oracle's timezone
and SQL Server's date settings.

Code is GPL-3.0-only, as with every DRE plugin. Apache Arrow and Jackson use Apache-2.0;
SLF4J uses MIT. Other bundled component notices are in
[`THIRD_PARTY_NOTICES.md`](../../THIRD_PARTY_NOTICES.md). Supply appropriately licensed Java
runtimes and vendor drivers yourself.

## Release preparation

The version comes from this directory's `pom.xml` and is released by the `jdbc-v1.0.0` tag. Tags
are not part of this preparation. A dry run builds the five supported JDBC archives and retains
the normal checksum, SBOM and provenance checks; it publishes nothing.
