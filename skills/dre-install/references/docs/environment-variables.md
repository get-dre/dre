---
title: "Environment variables"
description: "Every DRE_* environment variable and what it does, including DRE_RUN_AT to give a scheduled run or a late rerun a fixed, reproducible reporting period."
section: reference
position: 14
---

# Environment variables

| Variable | Effect |
|---|---|
| `DRE_PROFILES_DIR` | Directory holding `profiles.yml` (default: the project directory if it has one, else `~/.dre`). `--profiles-dir` overrides it. |
| `DRE_PLUGINS_DIR` | One plugins directory for every project, instead of each project's `dre_deps/plugins`. |
| `DRE_REGISTRY_URL` | The plugin package registry index (a URL or a local path). |
| `DRE_TARGET` | The run's target (environment), `target.name`: every profile uses its entry for it. `--target` overrides it; without either, each profile uses its own `target:` in `profiles.yml`, else `dev`. New in 0.2. |
| `DRE_TARGET_PATH` | Where DRE writes its generated files (default: `target/` in the project). `--target-path` overrides it; it overrides `target_path:` in `dre_project.yml`. |
| `GITHUB_TOKEN` | Sent to GitHub by `dre system update` and `github:` plugin sources (private repositories, rate limits). |
| `DRE_HTTP_TIMEOUT` | Seconds a download (the plugin registry, a plugin package, a DRE update) may receive nothing before it's tried again (default 60; 3 tries in all, on connection errors, timeouts, 429 and 5xx). Connecting gives up after 30 seconds. It wins over `flags: http_timeout` in `dre_project.yml`. New in 0.4. |
| `DRE_RUN_TIMEOUT` | How long a `dre run` may take before it's stopped (its Bindings recorded as `timed_out`, exit code 124): a duration such as `2h` or `90m`, or seconds. `--timeout` overrides it; it overrides `flags: run_timeout`. Off by default. New in 0.4. |
| `DRE_KEEP_RUNS` | How many runs of each report and Set to keep in the target path (default 1; the current run always stays). `--keep-runs` overrides it; it overrides `flags: keep_runs`. New in 0.4. |
| `DRE_THREADS` | How many Bindings may run at once in a `dre run`, over each connection entry's `threads:` (see [Running Bindings at once](connections.md#running-bindings-at-once)). `--threads` overrides it. New in 0.4. |
| `DRE_ROW_CONCURRENCY` | How many rows of a [per-row output](per-row-outputs.md) render and deliver at once, over `for_each.concurrency` (default 4). `--row-concurrency` overrides it. New in 0.6. |
| `DRE_GITHUB_API_URL` | The GitHub API for `github:` plugin sources and `dre system update` (GitHub Enterprise, a mirror). |
| `DRE_RUN_DATE` | The run date (`YYYY-MM-DD`) behind `run.date`, instead of today. It wins over `DRE_RUN_AT`'s date. Any other value refuses to start (exit 2). |
| `DRE_RUN_AT` | The instant a run was scheduled for (RFC 3339, e.g. `2026-09-01T06:00:00Z`). It becomes `run.now` and `run.scheduled_at`, gives `run.date` (that instant's date in the run's timezone) when `DRE_RUN_DATE` isn't set, and is recorded as `scheduled_at` in `run_results.json`. `dre schedule ls` puts it in every occurrence's command. |
| `DRE_TIMEZONE` | The run's timezone (IANA name), above every `timezone:` setting. `--timezone` overrides it. |
| `DRE_PLUGIN_HANDSHAKE_TIMEOUT_MS` | How long to wait for a plugin to start (default 30,000). |
| `NO_COLOR` | Turns off coloured output. |
| `DRE_SECRET_*` | Values are masked as `*****` in the console, logs, JSON events, the manifest, `run_results.json`, schema snapshots and `target/compiled/`. If a source rejects SQL containing a secret, its opaque error detail is omitted because databases may truncate or transform the value. Variables that `profiles.yml` reads through `env_var()` into a secret field (`password`, `token`, `api_key`, or a field the plugin marks secret) are masked the same way; paths and hosts are not. A value under 4 characters is masked only where it stands alone (not inside a longer word or number). `mask_secrets: false` in `dre_project.yml` turns masking off. Query result files are not altered. |
| `DRE_HOST_NAME` | Overrides the host identity recorded in Binding locks. A dead local process's lock can be reclaimed only when its host identity matches. Share an identity only within the same machine/process namespace; use `dre unlock` for a lock from another host. |
| `DRE_<COMMAND>_BIN` | Runs this executable for an extension command, e.g. `DRE_MCP_BIN` for `dre mcp`. This explicit override bypasses registry download and extension signature verification. Use only a trusted executable. |
| `DRE_INTERACTIVE` | Passed to plugins unchanged when set; otherwise DRE sets `1` for a terminal and `0` for unattended execution. Plugins use it to allow interactive sign-in or fail instead of waiting. |
| `DRE_NO_BROWSER` | Any nonempty value stops the Databricks plugin opening a browser; it prints the sign-in URL instead. It does not make browser sign-in suitable for an unattended runner. |

## Installer and development variables

These are separate from normal project settings:

| Variable | Effect |
|---|---|
| `DRE_VERSION` | Pins the Unix `install.sh` download to a release tag instead of the latest release. |
| `DRE_INSTALL_DIR` | Overrides the Unix install script's destination (default `~/.local/bin`). |
| `DRE_VOLUMES_ROOT`, `DRE_WORKSPACE_ROOT` | Databricks plugin test overrides for the root of mounted Volumes and Workspace paths; used only when `DATABRICKS_RUNTIME_VERSION` is set. Leave unset in production. |
| `DRE_TEST_*` | Internal test controls, service credentials and fixtures, not supported user settings. Includes prebuilt test binaries, extension signing test keys and font staging overrides. |
| `DRE_UPDATE_DOCS`, `DRE_UPDATE_DESCRIBE`, `DRE_UPDATE_SCHEMA`, `DRE_UPDATE_SCHEMAS` | Maintainer test switches that rewrite generated CLI, plugin, protocol and project-schema fixtures. |
| `DRE_SCHEMA_EXTRA_PROJECTS` | Extra project directories for the schema test, separated by the platform's path-list separator. |

Other `DRE_*` names in example SQL, test fixtures or build tooling are fixture data or local
script inputs, not additional CLI settings. `env_var()` may read any user-chosen variable name.

## Give a scheduled run stable inputs

For the complete [regional reports project](https://github.com/get-dre/dre/blob/master/examples/regional-reports/), persist artifacts
and pass the intended instant rather than the retry's wall clock:

```bash
export DRE_TARGET_PATH=/mnt/shared/dre/target
export DRE_KEEP_RUNS=7
export DRE_RUN_TIMEOUT=30m
cd examples/regional-reports
DRE_RUN_AT=2026-10-05T09:00:00Z dre run --schedule monday_north --preview 5
```

`monday_north` is that project's named schedule. Command flags override their matching
environment variables; those override YAML. `DRE_RUN_DATE` overrides the date derived from
`DRE_RUN_AT`, so leave it unset when the scheduled instant should decide the date.

Configure secrets in the runner's secret store and use `env_var('DRE_SECRET_NAME')` in profiles.
Check only whether they exist, never print their values. `unset-env-var` means a required value
was not available in the process that ran DRE; setting it in another terminal is not enough.
`missing-target-entry` means `DRE_TARGET` named an entry absent from a used profile.
See [orchestration](orchestration.md), [CLI flags](cli-reference.md),
[error codes](reference-error-codes.md), [target](glossary.md#target) and
[target path](glossary.md#target-path).

<!-- docs-nav: generated by .github/scripts/docs_sections.py from docs/sections.json -->

---

**Previous:** [Exit codes](exit-codes.md) · **Next:** [The manifest and `run_results.json`](manifest.md)
