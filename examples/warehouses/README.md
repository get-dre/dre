# Warehouses

The same synthetic Acme Corp revenue query on Postgres, ClickHouse, Databricks, BigQuery and
Snowflake, with every way of connecting to each. Each report creates its own sample rows on
its session, so no tables need to exist. Databricks uses a hidden setup query to create its
temporary view before the visible revenue query reads it on the same session.

| Connection | Shows |
|---|---|
| `pg_direct`, `pg_tls`, `pg_bastion` | Postgres without TLS, every `sslmode`, a CA file, `schema` and `role`, an SSH bastion with a key file and passphrase or your SSH agent |
| `ch_http`, `ch_native`, `ch_bastion` | ClickHouse over HTTP and native TCP, with and without TLS, mutual TLS, every `compression`, an SSH bastion |
| `databricks`, `databricks_pat`, `databricks_auto` | Databricks browser OAuth, a service principal, a personal access token and `auto` with a `~/.databrickscfg` profile |
| `bigquery*`, `snowflake*` | Every sign-in method; validated only |
| `volume_*` destinations | Writing back to a Unity Catalog Volume with each sign-in |

The project also shows macro packages (a pinned git package and a local one, with `dispatch`
choosing per database), Sets from `sets.yml`, folder config, `flags` and a source declared in
`dre_project.yml`.

The dependency declarations also show a GitHub release source (`duckdb`) and a local CSV
executable (`.plugins/dre-plugin-csv.exe`). Before validating, install DuckDB from the default
registry with `dre plugin install duckdb` in a directory outside this project. Set
`DRE_PLUGINS_DIR` to the directory holding that install (normally `~/.dre/plugins`, expanded to
an absolute path). Copy a CSV plugin executable you built or extracted from a published release
to `.plugins/dre-plugin-csv.exe` in this project.
Use that filename on every OS (Unix executables can also have an `.exe` suffix). A local plugin
path must name the executable, not its folder. Keep that file available while using the project.

CI supplies these installed executables before validation. It checks the source declarations
and the real plugins' configuration, including every target; it does not test downloading a
plugin through GitHub releases. The remaining packages install from their declared registry.

```bash
dre validate --all-targets
dre run local                       # DuckDB only: needs no server
```

The other reports need a server and your environment variables (`PGHOST`, `PGUSER`,
`DRE_SECRET_PG_PASSWORD`, `CLICKHOUSE_HOST`, `DATABRICKS_HOST`, `DATABRICKS_HTTP_PATH`, ...):

```bash
dre run postgres --set all          # direct, TLS and through the bastion
dre run clickhouse
dre run databricks                  # dev delivers nothing to the Volume; --target prod does
```

BigQuery and Snowflake need a Google Cloud project or a Snowflake account; their reports are
checked with `dre validate`, never run, in this repository.
