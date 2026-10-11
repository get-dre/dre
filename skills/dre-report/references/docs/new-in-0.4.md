---
title: "New in 0.4"
description: "Reliable runs and deliveries: exit codes, cancellation and timeouts, run folders, safe uploads, retries, connection checks, Bindings at once, and xlsx styles."
section: upgrading
position: 2
---

# New in 0.4

DRE 0.4 is about runs you can leave to a scheduler: they stop cleanly, say exactly what happened,
never leave half a file behind and never send anything twice. It's a minor release, so a few
things change; read [What changes](#what-changes) before you update a scheduled job.

## Runs

- **[Exit codes](exit-codes.md)** an orchestrator can act on: `0` success, `1` something failed
  (retry may help), `2` couldn't start (fix it first), `124` the run's timeout, `130` Ctrl-C and
  `143` a termination signal. `dre validate --strict` fails on warnings.
- **Clean cancellation**: Ctrl-C or a termination signal stops the running query on the server
  (Postgres, Databricks), records the Binding as `cancelled`, delivers nothing and starts nothing
  more. A second Ctrl-C stops at once.
- **Timeouts**: `dre run --timeout 2h` (`DRE_RUN_TIMEOUT`, `flags: run_timeout`) stops a run
  that takes too long, as `timed_out`. Every plugin that talks to a server has `connect_timeout`
  and `timeout`, and downloads (the registry, plugins, `dre system update`) give up and try again
  when nothing arrives (`flags: http_timeout`).
- **Run folders**: each run of a report and Set gets its own folder under `target/run/`, with its
  files, `run_results.json` and `dre.log`, and a `current` pointer to the latest.
  `flags: keep_runs` (`--keep-runs`, `DRE_KEEP_RUNS`) keeps a history; `dre history` lists it,
  and `dre unlock` clears a run that died. Two runs of the same Binding at once are refused.
  See [The target path](target-path.md).
- **[Bindings at once](connections.md#running-bindings-at-once)**: a connection entry's
  `threads:` runs that many Bindings on it at once; `dre run --threads N` caps the run.
- **`dre explain <code>`** describes any [error code](reference-error-codes.md), and every
  problem DRE reports names its code.

## Deliveries

- **[No half-written files](plugin-sftp.md)**: `local`, `sftp` and `ftp` upload under a
  temporary name, then rename (`atomic`, `temp_dir`).
- **[`if_exists`](plugins.md#a-file-already-at-the-path)** on every file destination: `overwrite`
  (as before), `error`, or `number` to keep both copies (`report_2.xlsx`).
- **[Retries](plugins.md#tries-again)** on temporary errors, everywhere, never sending anything
  twice: `retries` (default 3) on every plugin that talks to a server. A delivery that took
  several tries has `attempts` in `run_results.json`.
- **RSA SSH keys** ([RUSTSEC-2023-0071](plugin-sftp.md#rsa-keys)): an RSA key warns once per
  run; `allow_rsa_keys` silences or refuses it, and `use_agent: true` signs through your SSH
  agent.

## Checks and configuration

- **Connection checks**: `dre validate` asks each profile entry's plugin to check its settings
  without connecting: a missing field, a value of the wrong form, two settings that can't go
  together. `--all-targets` checks every entry. A misspelt key warns, with the nearest key.
- **`x-*` keys** in every YAML file, for [anchors you reuse](yaml-reference.md#reusing-yaml-x--keys-and-anchors).
- **Jinja**: `{% break %}` and `{% continue %}`, and maps keep the order they're written in.
- **[xlsx](plugin-xlsx.md)** 1.1.0: columns sized from their content (`autofit`, on by default;
  `width:` per column) and `style:` on the output, a tab or a column (fonts, colours, fills,
  borders, header and totals rows, banded rows).

## Docs and security

- One page per plugin, in [Plugins](plugins.md), with its fields and options generated from the
  plugin itself.
- Releases carry a CycloneDX SBOM and signed build provenance for every archive and wheel: see
  [Verifying a download](install.md#verifying-a-download).

## What changes

- **Exit codes**: a run that couldn't start (an invalid project or profile, bad flags, a missing
  plugin) exits `2`, not `1`. A job that treats any non-zero code as a failure is unaffected; one
  that retries on `1` now won't retry what retrying can't fix.
- **Output files move** into run folders: `target/run/<report>/<set>/runs/<run id>/` instead of
  `target/run/<report>/<set>/`, with a `current` file naming the latest. Read the path from `run_results.json` or
  `dre history <report> --latest --path` rather than building it. The shared `logs/dre.log` is
  gone: each run has its own `dre.log`.
- **`--var` values are YAML 1.2**: `--var flag=false` is `false` and `--var n=5` a number. Quote
  a value to keep it text: `--var flag='"false"'`.
- **Maps iterate in written order** in templates, instead of sorted by key.
- **Plugins**: DRE 0.4 needs plugins built on protocol 1. Update them all with
  `dre plugin update` (each `dre.lock` pin moves to the new version): duckdb 1.2.0, postgres
  1.3.0, databricks 1.3.0, csv, parquet and fixed_width 1.0.1, xlsx 1.1.0, object_store 1.1.0,
  sftp 1.2.0, ftp 1.1.0, email 1.2.0, slack 1.2.0, teams and google_chat 1.0.0-rc.2, bigquery and
  snowflake 1.0.0-alpha.2. An older plugin is refused with the command to update it.
- **Postgres** `connect_timeout` defaults to 30 seconds (before, no limit).
- **Skills** 3.0.0 cover DRE 0.4 only.

The schemas for editor checks are at `/schemas/v0.4/`.

<!-- docs-nav: generated by .github/scripts/docs_sections.py from docs/sections.json -->

---

**Previous:** [Updating DRE](updating.md) · **Next:** [New in 0.3](new-in-0.3.md)
