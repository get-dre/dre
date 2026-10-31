---
title: "JDBC (development build)"
description: "Connect an optional JDBC source with user-supplied drivers: development setup, Oracle reporting evidence, supported types and limits."
section: plugins
position: 100
---

# JDBC (development build)

The optional `jdbc` source runs SQL through a vendor JDBC driver and returns typed Arrow
batches to DRE. Preparation and result queries share one database connection. DRE's existing
core handles statement splitting, report orchestration and workbook output.

**Development build only.** This source requires Java 17 or newer and a local build with Maven
3.9 or newer. It has no registry release yet. Bundled Java, install/update packaging and native
Windows launch support are separate work. Core and CLI changes are not needed for this reporting
path. Vendor driver JARs are supplied by the user, not redistributed or downloaded by the plugin.

## Build and run

From the repository root:

```sh
mvn -B -f java/jdbc/pom.xml package
mkdir -p /tmp/jdbc-plugins/target
cp java/jdbc/dre-plugin-jdbc /tmp/jdbc-plugins/
cp java/jdbc/target/dre-plugin-jdbc.jar /tmp/jdbc-plugins/target/
export DRE_PLUGINS_DIR=/tmp/jdbc-plugins
```

Declare `jdbc` and the required output packages in the project's `dependencies.yml`:

```yaml
plugins: [jdbc, xlsx]
```

Keep `DRE_PLUGINS_DIR` set for `dre deps`, `dre validate` and `dre run`; the ordinary dependency
workflow installs other declared plugins there. The manually supplied JDBC development launcher
is discovered by the existing plugin host. Its JAR belongs in the adjacent `target/` directory.

An Oracle profile using a minimal Thin URL and driver properties:

```yaml
connections:
  reporting:
    targets:
      dev:
        type: jdbc
        url: "{{ env_var('JDBC_URL') }}"
        driver_jars: "{{ env_var('JDBC_DRIVER_JAR') }}"
        properties:
          user: "{{ env_var('JDBC_USER') }}"
          password: "{{ env_var('JDBC_PASSWORD') }}"
          oracle.jdbc.fanEnabled: false
```

For example, `JDBC_URL` can be `jdbc:oracle:thin:@//localhost:1521/FREEPDB1`. A complete vendor
URL containing credentials also works, without a `properties` block. Treat the full URL as a
secret. Specify each setting once: JDBC leaves duplicate URL/property precedence to the driver;
DRE does not parse or rebuild vendor URLs or impose a universal override rule.

| Field | Meaning |
| --- | --- |
| `url` | Required JDBC URL beginning with `jdbc:<vendor>:`. Kept unchanged. |
| `driver_jars` | Required readable local JAR path or list of paths. Relative paths use the project directory; `~/` uses the home directory. |
| `driver_class` | Optional fully qualified `java.sql.Driver` implementation; otherwise JDBC service discovery finds a supplied driver accepting the URL. |
| `properties` | Optional case-sensitive map of arbitrary vendor property names. Strings pass unchanged; booleans become `true`/`false`; numbers become decimal strings. Nested blocks, lists and nulls are rejected. |

Driver settings are unrestricted, but DRE-owned fields and their types are checked. Unknown
top-level connection keys warn, matching the existing protocol. A successful static validation
does not prove the driver recognizes every property; drivers may ignore unknown properties.

Run a report containing preparation followed by a query, for example:

```sql
alter session set nls_date_format = 'YYYY/MM/DD';
select cast('000019' as varchar2(20)) as "account_id",
       cast(12.34 as number(18,2)) as "balance",
       to_char(date '2026-01-31') as "session_date"
from dual;
```

Use a named query entry and `output: {format: xlsx}` as in any DRE report. With the same
configuration, `dre run <report> --preview` executes SQL and keeps the workbook locally.
Preparation still executes in preview mode; use database permissions to limit what it can do.

## Types and session limits

The source uses Apache Arrow's JDBC adapter, emitting batches of at most 8,192 rows. Ordinary
strings, numeric values, nulls and millisecond timestamps have Oracle integration coverage.
Text identifiers remain text through Arrow and xlsx output. A bound `row_limit` limits emitted
rows; zero still returns a schema-bearing empty result. It is not a promise that the server
avoids computing the rest of the query.

Decimals must declare precision 1–38 and scale 0–precision. Unconstrained Oracle `NUMBER`,
negative scales and wider precision fail with a request to cast explicitly. Values needing
rounding during conversion also fail. Prefer `cast(amount as number(18,2))` when that precision
matches the business rules; cast to character data when preserving a wider decimal exactly.
Excel's numeric precision remains separate from Arrow's decimal precision.

JDBC `TIME` and `TIMESTAMP` columns with declared fractional precision above 3 are refused in
this version rather than silently truncated. Cast to millisecond precision explicitly if
appropriate, or return exact text. Timestamps without a timezone retain their wall-clock fields
in a timezone-free Arrow timestamp; the isolated JVM uses UTC to make transport independent of
the host's timezone. Timezone-bearing vendor types and broader type fidelity need separate evidence.
Unsupported JDBC conversions fail; cast the column to a supported representation in SQL.

Oracle `DATE` includes time-of-day. The driver's default timestamp mapping preserves it.
If a driver setting such as `oracle.jdbc.mapDateToTimestamp: false` exposes it as JDBC `DATE`,
the source refuses that date-only mapping and asks for an explicit `TIMESTAMP(3)` or text cast.
This avoids silently dropping the clock fields. Oracle `RAW` bytes have Arrow/workbook coverage;
xlsx renders binary values as hexadecimal text. Other databases' `DATE`/`TIME` behavior needs
its own type-fidelity checks.

Only `sessions` and `validate` are advertised. There is no `check`, temporary lookup `load`,
catalog metadata integration or restricted-query capability. An explicit `read_only: true`
protocol request is refused. JDBC read-only hints are not an authorization boundary; configure
a database account with appropriate permissions. Cancellation uses the running JDBC statement's
`cancel()` and checks between batches. Actual server cancellation still depends on the driver.

## Diagnostics and secrets

The URL and all property values are potentially sensitive. The plugin never logs them or sends
raw driver exception text. Driver stdout/stderr is discarded because it can contain credentials
or fragments extracted from a URL. Structured errors retain the failure category, SQLSTATE and
numeric vendor code, plus the relevant configuration/query checks. Look up those codes in the
vendor documentation; do not enable raw driver logging to troubleshoot production credentials.
Authentication failures and a deliberately noisy test driver are checked with secret sentinels.

## Tested Oracle fixture

The reproducible fixture uses Oracle Free `23.26.3.0-lite`, Oracle JDBC `ojdbc11` 23.26.3.0.0,
Apache Arrow Java 19.0.0 and development Oracle JDK 17.0.1. Local verification runs on Apple Silicon;
Linux CI uses the same Oracle release's x86 image. CI's image digest and synthetic account setup
are pinned in [the workflow](../.github/workflows/ci.yml) and
[fixture SQL](../java/jdbc/tests/oracle-setup.sql). This establishes one tested Oracle environment,
not certification of every Oracle version or another vendor's driver.

The [report fixture](../java/jdbc/tests/report/) contains synthetic identifiers, a nullable
decimal, a timestamp and a session-format check. The CLI test checks the resulting workbook and
verifies failed credentials do not reach saved diagnostics. The language-neutral conformance
suite checks framing, negotiation, session results, empty results, errors and cancellation.
No SAS program or SAS parity comparison is involved in this connector fixture.

SQL Server, further JDBC databases, schema discovery and runtime bundling remain separate work.
Generic JDBC reach depends on compatible user-supplied drivers, not on a tested-support claim.

Sources: [Apache Arrow JDBC adapter](https://arrow.apache.org/java/current/jdbc.html),
[Java JDBC URL/property contract](https://docs.oracle.com/en/java/javase/17/docs/api/java.sql/java/sql/DriverManager.html#getConnection(java.lang.String,java.util.Properties)),
[Oracle Thin URL forms](https://docs.oracle.com/en/database/oracle/oracle-database/26/jajdb/oracle/jdbc/OracleDriver.html).

<!-- docs-nav: generated by .github/scripts/docs_sections.py from docs/sections.json -->

---

**Previous:** [Google Chat](plugin-google_chat.md) · **Next:** [CLI reference](cli-reference.md)
