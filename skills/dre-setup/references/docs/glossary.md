---
title: "Glossary"
description: "DRE terms, from Bindings and Sets to targets, sources and run artifacts."
section: reference
position: 17
---

# Glossary

## Binding

A [report paired with a Set](building-reports.md), resolved into the queries, connection,
variables and outputs for one run. A report without Sets has a default Binding.

## Connection

A named [database profile](connections.md) under `connections:` in `profiles.yml`.
Each query uses one connection; different queries in one report can use different connections.

## Destination

The [place a file or message is delivered](plugins.md#destinations), such as a local path,
object-storage bucket, email address or chat channel. Named destination profiles live under
`destinations:` in `profiles.yml`; `local` is built in.

## dre.lock

The [generated dependency lock file](registry.md#drelock), recording exact plugin versions,
checksums and macro-package commits. Commit it so another machine can resolve the same dependencies.

## Firing

One [occurrence of a schedule](schedule-ls.md#when-a-schedule-fires) at a UTC timestamp.
Its run date is the calendar date in the schedule's or report's timezone.

## Format

The [writer for an output](plugins.md#formats), such as `csv`, `parquet`, `fixed_width` or
`xlsx`. Formats are plugins except for the built-in `message` format.

## Lookup

A [small mapping table kept in a project file](lookups.md), rather than in a database.
`ref('name')` exposes it to SQL; `lookup('name')` exposes its rows to Jinja.

## Macro package

A [dependency containing reusable Jinja macros](registry.md#macro-packages), installed from
Git or a local folder. Call its macros through the package's namespace.

## Manifest

The [machine-readable description of the loaded project](manifest.md), written to
`target/manifest.json`: reports, Bindings, sources, schedules and their dependencies.
Unlike dbt's manifest, it describes reports and deliveries rather than transformation models.

## Message

A [text output rendered from query results](messages.md), usually a headline or an alert.
It can be delivered alone or with another output's files attached.

## Output

One [formatted result of a report](building-reports.md), with its own format, query subset
and destinations. Several outputs reuse the same query execution.

## Plugin package

An [installable release containing one or more plugins](registry.md), with its own version.
For example, the `object_store` package supplies the `s3`, `gcs` and `azure_blob` destinations.

## Profile

A named [connection or destination configuration](connections.md) in `profiles.yml`.
Each profile has target entries and may choose its own default target. DRE profiles are named
connections or destinations; they are not dbt's top-level project profile containing `outputs`.

## Registry

The [index of plugin packages and releases](registry.md#the-registry) DRE consults when
installing dependencies. A project can use the default registry or specify another index.

## Report

A [YAML declaration of SQL queries and outputs](building-reports.md).
An SQL file without a report declaration is an unmanaged report, intended for quick tests.

## run_results.json

The [record of what a run did](manifest.md#run_resultsjson): statuses, timings, query results,
output files and deliveries. It complements the manifest, which describes what can run.

## Schedule

A named [selection of reports or Sets with a timing and optional variables](schedules.md).
DRE describes schedules and lists occurrences; an external scheduler starts the processes.

## Schema drift

A [change to a query's result columns or types](building-reports.md), compared with its
previous successful run. Drift checks help catch output changes before delivery.

## Set

A named [variant of a report](building-reports.md), such as a client or region, with its own
connection and variables. A report may override a Set's queries, tab names and outputs.

## Source

A [declaration of existing database tables](sources.md), accessed with `source('name', 'table')`.
Like dbt's source declarations it identifies tables; DRE adds `profile:` to choose their connection
and does not execute dbt source freshness checks or tests.

## Source plugin

A [plugin that connects to a database and runs SQL](plugins.md#sources), such as DuckDB or
PostgreSQL. It is distinct from a source declaration, which names tables.

## Target

An [environment entry](connections.md#which-target-each-profile-uses), such as `dev` or `prod`,
selected from each profile's `targets:`. `--target` or `DRE_TARGET` selects it for every used profile.
Unlike dbt's `target` context, DRE's target is not the database connection; use `connection.*` for that.

## Target path

The [directory for generated files](target-path.md): compiled SQL, manifests, schema snapshots
and run outputs. It defaults to `target/` and is unrelated to selecting the `dev` or `prod` target.

## Timing

A [recurrence rule](schedules.md#share-a-timing), defined with `cron`, `rrule` or `every`.
Named entries in `timings.yml` let several schedules share one rule.

<!-- docs-nav: generated by .github/scripts/docs_sections.py from docs/sections.json -->

---

**Previous:** [The manifest and `run_results.json`](manifest.md) · **Next:** [Updating DRE](updating.md)
