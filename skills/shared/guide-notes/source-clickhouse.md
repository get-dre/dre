- Default to `driver: http`: its sessions survive a failed statement, so temp tables and `SET`
  from `tab: false` queries stay. Use `driver: native` only when the HTTP port isn't reachable.
- ClickHouse Cloud: `secure: true`, the service's host, a user and `password` from `env_var()`
  (SEC-2); the port follows `driver` and `secure` (8443 HTTP, 9440 native).
- A self-hosted server with a private CA: `ca_cert`; behind a bastion: `ssh` (the postgres
  source's settings). Never suggest `verify: false` except for a local test server.
- `schema` is the ClickHouse database. A copied dbt-clickhouse profile works as it is.
- For limits on agent or report queries, use `custom_settings` (`max_execution_time`,
  `max_memory_usage`, `max_result_rows`): they apply even in read-only sessions.
- To try ClickHouse locally, a `clickhouse/clickhouse-server` Docker container works.
