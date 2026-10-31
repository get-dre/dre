- Declare `jdbc` under `plugins:` and run `dre deps`. The user supplies Java 17 or newer;
  `JAVA_HOME/bin/java` wins when set, otherwise the launcher uses `java` from `PATH`. There is
  no upper-version cap, but the vendor driver must support that Java version. The plugin does
  not install, download or update Java. Missing/older Java produces setup guidance.
- Supported release packages: macOS and Linux on x86_64 or aarch64, and Windows x64. The JDBC
  package contains the launcher, JAR and notices. There is no Windows ARM64 package for 1.0.0.
- `driver_jars` is required and always user-supplied: a local path or list of paths. Keep it
  outside the project's generated target files and use `env_var()` for a machine-specific path.
- Put the vendor URL in `url` and credentials or vendor settings in `properties`, all from
  `env_var()` (SEC-2, SEC-3). Prefer a minimal URL plus `user` and `password`; a URL containing
  credentials is also secret. Don't try to infer a vendor's property precedence.
- Leave `driver_class` out unless service discovery is ambiguous; then give its fully qualified
  class name.
- If a decimal or timestamp type is refused for precision loss, cast to the supported precision
  in SQL. Don't promise broader JDBC type fidelity than the tested fixture.
- Cancellation calls JDBC `Statement.cancel()` and checks between batches; server behavior still
  belongs to the driver. Use database permissions for read-only access, not the JDBC hint.
