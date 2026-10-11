---
title: "CLI reference"
description: "Every dre command, subcommand and flag, with defaults and environment variables."
section: reference
position: 1
---

# CLI reference

<!-- Generated from the CLI definitions by crates/dre-cli/src/cli_reference.rs. Edit the code, not this page. -->

Every `dre` command and flag. `dre <command> --help` prints the same text. Every command exits with one of the documented [exit codes](exit-codes.md); each problem it reports has an [error code](reference-error-codes.md). Settings that also come from the environment are listed with their variable; every variable is on the [environment variables](environment-variables.md) page.

## Global options

These work with every command.

| Option | Default | Description |
|---|---|---|
| `-v, --verbose` |  | Show every step (same as `--log-level debug`). |
| `-q, --quiet` |  | Only show errors and the final summary. |
| `--log-level <LOG_LEVEL>` |  | How much to show. One of `quiet`, `info`, `debug`. |
| `--log-format <LOG_FORMAT>` | `text` | `text` for people, `json` (one object per line) for CI and tooling. One of `text`, `json`. |
| `--color <COLOR>` | `auto` | Colour output: auto (default; off when NO_COLOR is set or output isn't a terminal), always or never. One of `auto`, `always`, `never`. |

## `dre validate`

Check the project (config, references, templates, schedules) and compile its SQL.

```text
dre validate [OPTIONS] [SELECTOR]...
```

| Argument | Description |
|---|---|
| `SELECTOR...` | Which reports to compile and check (same selectors as `dre run`; default: all). With a selector, validate also shows where each selected Binding's output would go. Reports whose templates query the database (`run_query()`, `columns()`) connect, and may sign in, to compile. |

| Option | Default | Description |
|---|---|---|
| `-s, --select <SELECTOR>` |  | What to select (dbt's `--select`): report names, `tag:<tag>`, folder names or dotted folder paths. Several match any of them: `-s a b`, `-s a,b`, `-s a -s b` (or `"a;b"`). |
| `--project-dir <PROJECT_DIR>` | `.` | Project directory (default: the current directory). |
| `--profiles-dir <PROFILES_DIR>` |  | Directory holding profiles.yml (default: $DRE_PROFILES_DIR, then the project directory if it has a profiles.yml, then ~/.dre). Environment: `DRE_PROFILES_DIR`. |
| `--no-auto-install` |  | Fail instead of installing declared plugins that are missing. |
| `--target <TARGET>` |  | The run's target (environment), `target.name` in templates: every profile uses its entry for it (default: $DRE_TARGET; without either, each profile uses its own `target:`, else `dev`). Environment: `DRE_TARGET`. |
| `--target-path <PATH>` |  | Where DRE writes its generated files (default: $DRE_TARGET_PATH, then `target_path` in dre_project.yml, then target/). A local or mounted path, absolute or relative to the project root. Unrelated to `--target`. Environment: `DRE_TARGET_PATH`. |
| `--var <NAME=VALUE>` |  | Set a variable for `var()`, overriding every other level: `--var name=value`. The value is YAML 1.2 (`false`, `5`, `[a, b]` are typed); quote it to keep text: `--var x='"false"'`. |
| `--timezone <TIMEZONE>` |  | The run's timezone (IANA name, e.g. Australia/Sydney), above every `timezone:` setting (default: $DRE_TIMEZONE). Environment: `DRE_TIMEZONE`. |
| `--json` |  | Emit machine-readable JSON instead of text. |
| `--live` |  | After the offline checks, connect to each Binding's source and check every rendered statement without executing it (EXPLAIN or the dialect's equivalent). |
| `--set <SET>` |  | Compile (and with --live, check) one Set instead of every Set. |
| `--strict` |  | Treat warnings as errors: exit 1 when there are any. |
| `--all-targets` |  | Check the connection settings of every entry in profiles.yml, not only the entries the selected reports would use with these flags. |

Example:

```bash
dre validate --live monthly_revenue
```

## `dre run`

Run reports: render, execute, format into the target folder, and deliver.

```text
dre run [OPTIONS] [SELECTOR]...
```

| Argument | Description |
|---|---|
| `SELECTOR...` | What to run: report names, `tag:<tag>`, folder names or dotted folder paths (`dre run a b` runs both). Runs every report when omitted. |

| Option | Default | Description |
|---|---|---|
| `-s, --select <SELECTOR>` |  | What to select (dbt's `--select`): report names, `tag:<tag>`, folder names or dotted folder paths. Several match any of them: `-s a b`, `-s a,b`, `-s a -s b` (or `"a;b"`). |
| `--project-dir <PROJECT_DIR>` | `.` | Project directory (default: the current directory). |
| `--profiles-dir <PROFILES_DIR>` |  | Directory holding profiles.yml (default: $DRE_PROFILES_DIR, then the project directory if it has a profiles.yml, then ~/.dre). Environment: `DRE_PROFILES_DIR`. |
| `--no-auto-install` |  | Fail instead of installing declared plugins that are missing. |
| `--target <TARGET>` |  | The run's target (environment), `target.name` in templates: every profile uses its entry for it (default: $DRE_TARGET; without either, each profile uses its own `target:`, else `dev`). Environment: `DRE_TARGET`. |
| `--target-path <PATH>` |  | Where DRE writes its generated files (default: $DRE_TARGET_PATH, then `target_path` in dre_project.yml, then target/). A local or mounted path, absolute or relative to the project root. Unrelated to `--target`. Environment: `DRE_TARGET_PATH`. |
| `--var <NAME=VALUE>` |  | Set a variable for `var()`, overriding every other level: `--var name=value`. The value is YAML 1.2 (`false`, `5`, `[a, b]` are typed); quote it to keep text: `--var x='"false"'`. |
| `--timezone <TIMEZONE>` |  | The run's timezone (IANA name, e.g. Australia/Sydney), above every `timezone:` setting (default: $DRE_TIMEZONE). Environment: `DRE_TIMEZONE`. |
| `--set <SET>` |  | Run one Set (declared or ad hoc), or `all` of a report's Sets. |
| `--schedule <NAME>` |  | Run the Bindings a schedules.yml entry targets, with its vars. Pass the scheduled instant through DRE_RUN_AT (or the date through DRE_RUN_DATE) so reruns render the same. With a selector and/or --set, run just those of its Bindings. |
| `--profile <PROFILE>` |  | Use this connection instead of the inherited one (report, Set, folder `+profile`, `default_profile`), e.g. for an ad hoc Set. A query's own `profile:` or a source's wins. |
| `--output-name <OUTPUT_NAME>` |  | Override the output file name for this run (the first output, with several). |
| `--output-path <OUTPUT_PATH>` |  | Override the full output (delivery) path for this run (the first output, with several). |
| `--dry-run` |  | Render SQL into target/compiled/ and stop; no report query is executed. |
| `--preview [<ROWS>]` |  | Execute with a row limit (default 100); output stays in target/ and is never delivered. |
| `--accept-schema-change` |  | Deliver even if the output schema changed since the last successful run, and accept the new schema. Snapshots live in the target path, so a fresh CI runner has no history unless `--target-path` (or DRE_TARGET_PATH) points at a folder that persists. |
| `--timeout <DURATION>` |  | Stop the run if it takes longer than this: a duration such as `2h` or `90m`, or seconds (default: $DRE_RUN_TIMEOUT, then `flags: run_timeout` in dre_project.yml; off without any). Its Bindings are then recorded as `timed_out`, and `dre` exits 124. |
| `--keep-runs <N>` |  | How many runs of each report and Set to keep in the target path, the current one included (default: $DRE_KEEP_RUNS, then `flags: keep_runs` in dre_project.yml, then 1). |
| `--threads <N>` |  | How many Bindings may run at once in this run (default: $DRE_THREADS; without it, each connection entry's `threads:`, default 1). `--threads 1` runs them one at a time. |

Example:

```bash
dre run monthly_revenue --set client_a --target prod
```

## `dre compile`

Render reports' SQL into <target path>/compiled/ without running it, and list the files.

```text
dre compile [OPTIONS] [SELECTOR]...
```

| Argument | Description |
|---|---|
| `SELECTOR...` | What to compile: report names, `tag:<tag>`, folder names or dotted folder paths. Compiles every report when omitted. |

| Option | Default | Description |
|---|---|---|
| `-s, --select <SELECTOR>` |  | What to select (dbt's `--select`): report names, `tag:<tag>`, folder names or dotted folder paths. Several match any of them: `-s a b`, `-s a,b`, `-s a -s b` (or `"a;b"`). |
| `--project-dir <PROJECT_DIR>` | `.` | Project directory (default: the current directory). |
| `--profiles-dir <PROFILES_DIR>` |  | Directory holding profiles.yml (default: $DRE_PROFILES_DIR, then the project directory if it has a profiles.yml, then ~/.dre). Environment: `DRE_PROFILES_DIR`. |
| `--no-auto-install` |  | Fail instead of installing declared plugins that are missing. |
| `--target <TARGET>` |  | The run's target (environment), `target.name` in templates: every profile uses its entry for it (default: $DRE_TARGET; without either, each profile uses its own `target:`, else `dev`). Environment: `DRE_TARGET`. |
| `--target-path <PATH>` |  | Where DRE writes its generated files (default: $DRE_TARGET_PATH, then `target_path` in dre_project.yml, then target/). A local or mounted path, absolute or relative to the project root. Unrelated to `--target`. Environment: `DRE_TARGET_PATH`. |
| `--var <NAME=VALUE>` |  | Set a variable for `var()`, overriding every other level: `--var name=value`. The value is YAML 1.2 (`false`, `5`, `[a, b]` are typed); quote it to keep text: `--var x='"false"'`. |
| `--timezone <TIMEZONE>` |  | The run's timezone (IANA name, e.g. Australia/Sydney), above every `timezone:` setting (default: $DRE_TIMEZONE). Environment: `DRE_TIMEZONE`. |
| `--set <SET>` |  | Compile one Set (declared or ad hoc), or `all` of a report's Sets. |

Example:

```bash
dre compile tag:finance
```

## `dre clean`

Remove the target folder (compiled SQL, run outputs, schema snapshots, the manifest).

```text
dre clean [OPTIONS]
```

| Option | Default | Description |
|---|---|---|
| `--project-dir <PROJECT_DIR>` | `.` | Project directory (default: the current directory). |
| `--prune` |  | Don't delete the folder: only remove the runs beyond `keep_runs` (and unfinished ones) from every report and Set, keeping each current run. |
| `--keep-runs <N>` |  | With --prune, keep this many runs of each (default: $DRE_KEEP_RUNS, then `flags: keep_runs`, then 1). |
| `--target-path <PATH>` |  | The folder to clean (default: $DRE_TARGET_PATH, then `target_path` in dre_project.yml, then target/). Only a folder DRE created is deleted. Environment: `DRE_TARGET_PATH`. |

Example:

```bash
dre clean
```

## `dre ls`

List the reports and Bindings a selection or schedule covers, without running anything.

```text
dre ls [OPTIONS] [SELECTOR]...
```

| Argument | Description |
|---|---|
| `SELECTOR...` | What to list: report names, `tag:<tag>`, folder names or dotted folder paths. Lists every report when omitted. |

| Option | Default | Description |
|---|---|---|
| `-s, --select <SELECTOR>` |  | What to select (dbt's `--select`), as on `dre run`. |
| `--set <SET>` |  | Only this Set's Bindings (`all`: every Set). Without it, every declared Binding of each selected report is listed, not only the default Set a plain `dre run` would pick. |
| `--schedule <NAME>` |  | The Bindings a schedules.yml entry runs. |
| `--output <OUTPUT>` | `text` | `text` for people, `json` for tools (the manifest's shape, holding only what matched). One of `text`, `json`. |
| `--resource-type <RESOURCE_TYPE>` | `report` | What to list: reports (default), or the declared sources (each table, its connection, and the reports that read it; unused ones are flagged). One of `report`, `source`. |
| `--target <TARGET>` |  | The run's target (environment), which sets every profile's entry (default: $DRE_TARGET; without either, each profile uses its own `target:`, else `dev`). Environment: `DRE_TARGET`. |
| `--var <NAME=VALUE>` |  | Set a variable for `var()`, as on `dre run`. |
| `--project-dir <PROJECT_DIR>` | `.` | Project directory (default: the current directory). |
| `--profiles-dir <PROFILES_DIR>` |  | Directory holding profiles.yml (not needed; accepted as on the other project commands). Environment: `DRE_PROFILES_DIR`. |
| `--target-path <PATH>` |  | The target path, accepted as on the other project commands; `ls` writes nothing to it. Environment: `DRE_TARGET_PATH`. |

Example:

```bash
dre ls --schedule daily
```

## `dre schedule`

Work out when schedules fire, for people and orchestrators.

Subcommands:

- [`dre schedule ls`](#dre-schedule-ls): List when schedules fire (occurrences) within a window, with the command to run each one.

## `dre schedule ls`

List when schedules fire (occurrences) within a window, with the command to run each one.

```text
dre schedule ls [OPTIONS]
```

| Option | Default | Description |
|---|---|---|
| `--schedule <NAME>` |  | Only this schedule (repeatable). |
| `-s, --select <SELECTOR>` |  | Only schedules that run one of these reports: report names, `tag:<tag>`, folder names or dotted folder paths, as on `dre run`. |
| `--from <WHEN>` |  | Start of the window: a date (00:00 UTC) or an RFC 3339 date-time (default: now, to the minute). May be in the past. |
| `--to <WHEN>` |  | End of the window, exclusive (default: 35 days after --from; at most 366 days after it). |
| `--limit <N>` |  | At most this many firings per schedule (text output defaults to 5). |
| `--split` |  | One occurrence per report and Set instead of one per firing. |
| `--output <OUTPUT>` | `text` | `text` for people, `json` for tools (the versioned schedule document). One of `text`, `json`. |
| `--project-dir <PROJECT_DIR>` | `.` | Project directory (default: the current directory). |
| `--profiles-dir <PROFILES_DIR>` |  | Directory holding profiles.yml (not needed; accepted as on the other project commands). Environment: `DRE_PROFILES_DIR`. |

Example:

```bash
dre schedule ls --from 2026-11-01 --to 2026-12-01
```

## `dre init`

Set up a connection (installing its plugin) and optionally a starter project, interactively.

```text
dre init [OPTIONS]
```

| Option | Default | Description |
|---|---|---|
| `--profiles-dir <PROFILES_DIR>` |  | Directory holding profiles.yml (default: $DRE_PROFILES_DIR, then ~/.dre). Environment: `DRE_PROFILES_DIR`. |

Example:

```bash
dre init
```

## `dre new`

Create a starter project in a new directory.

```text
dre new [OPTIONS] <DIR>
```

| Argument | Description |
|---|---|
| `DIR` | Directory to create (must be missing or empty). |

| Option | Default | Description |
|---|---|---|
| `--profile <PROFILE>` | `warehouse` | The connection profile the project uses by default. |
| `--type <PLUGIN>` | `duckdb` | The plugin of that connection (its `type`), which the project declares. `--source` is the 0.1 name. |

Example:

```bash
dre new my_reports --type postgres
```

## `dre deps`

Install the project's declared plugins (pinned by dre.lock) without running anything.

```text
dre deps [OPTIONS]
```

| Option | Default | Description |
|---|---|---|
| `--project-dir <PROJECT_DIR>` | `.` |  |
| `--profiles-dir <PROFILES_DIR>` |  |  Environment: `DRE_PROFILES_DIR`. |

Example:

```bash
dre deps
```

## `dre plugin`

Manage plugins (sources, formats, destinations).

Subcommands:

- [`dre plugin list`](#dre-plugin-list): List installed plugins, with each one's version and protocol version.
- [`dre plugin install`](#dre-plugin-install): Install a plugin from the registry: `name`, `kind/name`, optionally `@<version req>`.
- [`dre plugin update`](#dre-plugin-update): Install the newest version allowed by the constraint (ignoring dre.lock's pin) and re-pin it.
- [`dre plugin remove`](#dre-plugin-remove): Remove installed versions of a plugin (`name@version` removes just one).

## `dre plugin list`

List installed plugins, with each one's version and protocol version.

```text
dre plugin list [OPTIONS]
```

Example:

```bash
dre plugin list
```

## `dre plugin install`

Install a plugin from the registry: `name`, `kind/name`, optionally `@<version req>`.

```text
dre plugin install [OPTIONS] <PLUGIN>
```

| Argument | Description |
|---|---|
| `PLUGIN` | `duckdb`, `source/duckdb`, `xlsx@^1`, ... |

| Option | Default | Description |
|---|---|---|
| `--project-dir <PROJECT_DIR>` | `.` | Project whose dre.lock to update (default: the current directory, if it's a project). |

Example:

```bash
dre plugin install xlsx@^1
```

## `dre plugin update`

Install the newest version allowed by the constraint (ignoring dre.lock's pin) and re-pin it.

```text
dre plugin update [OPTIONS] <PLUGIN>
```

| Argument | Description |
|---|---|
| `PLUGIN` | `duckdb`, `source/duckdb`, `xlsx@^1`, ... |

| Option | Default | Description |
|---|---|---|
| `--project-dir <PROJECT_DIR>` | `.` | Project whose dre.lock to update (default: the current directory, if it's a project). |

Example:

```bash
dre plugin update postgres
```

## `dre plugin remove`

Remove installed versions of a plugin (`name@version` removes just one).

```text
dre plugin remove [OPTIONS] <PLUGIN>
```

| Argument | Description |
|---|---|
| `PLUGIN` | `duckdb`, `source/duckdb`, `xlsx@^1`, ... |

| Option | Default | Description |
|---|---|---|
| `--project-dir <PROJECT_DIR>` | `.` | Project whose dre.lock to update (default: the current directory, if it's a project). |

Example:

```bash
dre plugin remove duckdb@1.1.0
```

## `dre system`

Commands about DRE itself rather than a project.

Subcommands:

- [`dre system update`](#dre-system-update): Update DRE to the newest release (or VERSION), the way it was installed.

## `dre system update`

Update DRE to the newest release (or VERSION), the way it was installed.

```text
dre system update [OPTIONS] [VERSION]
```

| Option | Default | Description |
|---|---|---|
| `--check` |  | Only report whether an update exists; change nothing. |

Example:

```bash
dre system update --check
```

## `dre explain`

Explain an error code (`dre explain unknown-key`): what it means and how to fix it.

```text
dre explain [OPTIONS] <CODE>
```

| Argument | Description |
|---|---|
| `CODE` | The code, as in `error[unknown-key]`. |

Example:

```bash
dre explain unknown-key
```

## `dre history`

A report's runs in the target path, newest first, and which is current (the latest finished); `--latest --path` prints where the latest files are.

```text
dre history [OPTIONS] <REPORT>
```

| Argument | Description |
|---|---|
| `REPORT` | The report. |

| Option | Default | Description |
|---|---|---|
| `--binding <BINDING>` |  | Only this Binding: a Set's name, or `default` for a report without Sets. |
| `--latest` |  | Only the current run (the latest that finished) of each Binding. |
| `--path` |  | Print only the runs' folders, one per line (with --latest: where the latest files are). |
| `--output <OUTPUT>` | `text` | `text` for people, `json` for scripts. One of `text`, `json`. |
| `--project-dir <PROJECT_DIR>` | `.` | Project directory (default: the current directory). |
| `--target-path <PATH>` |  | The target path (default: $DRE_TARGET_PATH, then `target_path` in dre_project.yml, then target/). Environment: `DRE_TARGET_PATH`. |

Example:

```bash
dre history monthly_revenue --latest --path
```

## `dre unlock`

Remove a Binding's lock left by a run that's no longer going on (it shows the holder and asks first).

```text
dre unlock [OPTIONS] <REPORT>
```

| Argument | Description |
|---|---|
| `REPORT` | The report. |

| Option | Default | Description |
|---|---|---|
| `--binding <BINDING>` | `default` | The Binding: a Set's name, or `default` (the default) for a report without Sets. |
| `--yes` |  | Don't ask for confirmation (for scripts). |
| `--project-dir <PROJECT_DIR>` | `.` | Project directory (default: the current directory). |
| `--target-path <PATH>` |  | The target path (default: $DRE_TARGET_PATH, then `target_path` in dre_project.yml, then target/). Environment: `DRE_TARGET_PATH`. |

Example:

```bash
dre unlock monthly_revenue --binding client_a
```

<!-- docs-nav: generated by .github/scripts/docs_sections.py from docs/sections.json -->

---

**Previous:** [Google Chat](plugin-google_chat.md) · **Next:** [YAML reference](yaml-reference.md)
