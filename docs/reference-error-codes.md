---
title: "Error codes reference"
description: "Every code DRE reports, what it means and how to fix it."
sidebar:
  order: 30
---

# Error codes reference

<!-- Generated from crates/dre-core/src/codes.rs. Edit the registry, not this page. -->

Every problem DRE reports has a code: `error[unknown-key]: ...` in the console, `code` in `--log-format json` events, `dre validate --json` and `run_results.json`. `dre explain <code>` prints the explanation below. A plugin's own codes are namespaced by the plugin (`sftp/host-key-mismatch`) and documented with it. Codes are stable: one is never reused for something else.

Each code has a kind, which decides `dre`'s exit code and whether trying again can help.

## Usage: how the command was called

### ambiguous-selector

A selector matches a report and a folder.

A bare name can mean a report or a folder. Use `folder:<path>` or the report's full dotted path.

### invalid-selector

A selector can't be resolved.

The selector (`-s`, `--set`, `--schedule`) doesn't resolve to Bindings to run. The message says why; `dre ls` lists what exists.

### project-file-missing

There's no dre_project.yml.

DRE looks for `dre_project.yml` in `--project-dir` (default: the current folder). Run the command from the project root, pass `--project-dir`, or create a project with `dre new`.

### selector-matches-nothing

A selector or schedule matches no report.

The selector (`-s`, a schedule's `select:` or `report:`) names no report, folder, tag or Set. `dre ls` lists what exists.

### unknown-source

A selector names a source that doesn't exist.

`source:<name>` selects reports using that source; the name must be declared under `sources:`.

## Config: the project's files

### compile-failed

A report doesn't compile.

`dre compile` or `validate` couldn't render a Binding's SQL. The message has the cause.

### compile-needs-run

A report can only be checked by running it.

The report queries the database while rendering (`run_query()`, `columns()`), so `validate` can't compile it offline. `dre run` (or `validate --live`) checks it.

### conflicting-declaration

Two fragments of a report set the same key.

A report may be split over several YAML files, but each key is set in one of them only. Remove the key from one file.

### conflicting-packages

A macro package is declared twice with different revisions.

Declare each package once, at one revision.

### conflicting-plugin-constraints

Two declarations of a plugin allow no common version.

A package declared in several files must have constraints some version satisfies. Align them.

### conflicting-plugin-sources

A plugin is declared with two sources.

Every declaration of a package must agree on where it comes from (`github:`, `local:`, `registry:`).

### connection-conflict

A query would need two connections.

A query uses sources (or a `profile:`) on different connections, but one query runs on one connection. Split it, or move the tables to one system.

### duplicate-lookup

Two lookups have the same name.

Lookups are named after their file, without the extension; two files with one name are ambiguous.

### duplicate-output

Two outputs would have the same name or file.

Outputs of one report need unique `name:`s, and two unnamed outputs of one format would write the same file. Give each output a `name:`.

### duplicate-package

Two macro packages have the same name.

Macros are called through the package's name, so names are unique.

### duplicate-ref-name

A lookup and a .sql file have the same name.

`ref()` names are shared by `.sql` files and lookups, so they must be unique. Rename one.

### duplicate-report-name

Two reports have the same name.

Report names are unique across the project: a report's name is its folder's (or its `name:`), and an unmanaged report is named after its `.sql` file. Rename one of them.

### duplicate-schedule-name

Two schedules have the same name.

Schedule names are unique across the project.

### duplicate-set

A Set is declared or listed twice.

Set names are unique in `sets.yml` files, and a report lists each Set once.

### duplicate-source

Two sources have the same name.

Source names are unique across the project's YAML files.

### duplicate-source-column

A table declares a column twice.

Column names are unique within a table, compared case-insensitively.

### duplicate-source-table

A source declares a table twice.

Table names are unique within a source.

### duplicate-sql-name

Two .sql files have the same name.

Queries are found by file name across `reports/`, so two `.sql` files with one name are ambiguous. Rename one.

### duplicate-timing-name

Two timings have the same name.

Timing names are unique across the project.

### exclude-and-queries

A Set uses both `exclude:` and `queries:`.

A Set either leaves queries out (`exclude:`) or replaces them (`queries:`), not both.

### invalid-cell

A cell reference isn't valid.

Cells are written like `A1` or `AB12`.

### invalid-destination-option

A destination option is wrong.

An `output.destination` entry has a bad value for one of its options (`attach:` naming no output, a plugin's option of the wrong type). The message names the option.

### invalid-field

A key has a value of the wrong type or shape.

The value doesn't fit the key: a string where a list is expected, a number out of range, an unknown choice. The message says what the key takes.

### invalid-folder-config

Folder config under `reports:` has the wrong shape.

Each folder under `reports:` in dre_project.yml is a map: `+` keys are settings, other keys are subfolders.

### invalid-locale

A locale isn't one the number filters know.

`locale:` takes a tag such as `en`, `de-DE` or `fr`; the message lists what's known.

### invalid-lookup

A lookup or its config is wrong.

A lookup is a csv, xlsx, xls, json or jsonl file under `lookups/` (or a `.yml` of rows), with an optional `<name>.yml` config of `columns`, `sheet` and `load`. The message names the problem.

### invalid-output-option

An output option is wrong.

A shared output key (`template`, `extension`, ...) or a format's option has a bad value, or applies to another format. The message names the option.

### invalid-package

A macro package is broken.

The package's folder or manifest couldn't be read. The message has the reason.

### invalid-packages

`packages:` is wrong.

`packages:` (in dependencies.yml or packages.yml) is a list of macro packages, each with `git:` and `revision:`, or `local:`.

### invalid-plugin-declaration

A `plugins:` entry is wrong.

Each entry is a package name, `name: "<version>"`, or a map with `name:` and one of `github:`, `local:`, `registry:`.

### invalid-profile

A profile in profiles.yml is wrong.

Each profile has `targets:`, a map of entries with a `type:` (or `{deliver: false}` for a destination), and optionally `target:`, the entry it uses by default.

### invalid-profile-value

A value that chooses a connection uses something only a connection has.

`profile:` and `default_profile` are rendered before any connection is open, so they can't use `connection.*`, `run_query()` or `columns()`.

### invalid-profiles

profiles.yml has the wrong shape.

profiles.yml is a map with `connections:` and/or `destinations:`, each a map of profiles. See the profiles.yml reference.

### invalid-project

dre_project.yml has the wrong shape.

`dre_project.yml` must be a map of keys (`name:`, `default_profile:`, ...). See the dre_project.yml reference.

### invalid-query-name

A query is named with a path or extension.

`queries:` entries are bare `.sql` file names without folders or the extension: `orders`, not `reports/sales/orders.sql`.

### invalid-report

A report file has the wrong shape.

A report YAML file must be a map of report keys (`queries:`, `output:`, ...). See the report reference.

### invalid-schedule

A schedule is wrong.

Each schedules.yml entry has a `name`, `report:` (optionally `set:`) or `select:`, and a timing: `timing:` naming one in timings.yml, or its own `cron`, `every` or `rrule`.

### invalid-source

A source declaration is wrong.

`sources:` is dbt's format: a list of sources, each with a `name:` and `tables:`, optionally `database`, `schema`, `profile`, `quoting`, `tags`, `meta`. See the sources reference.

### invalid-target-path

The target path isn't usable.

The target path (`--target-path`, `DRE_TARGET_PATH` or `target_path:`) must be a local or mounted folder outside the project's sources, not a URL. To copy outputs to object storage, deliver them with a destination.

### invalid-template

An xlsx template binding is wrong.

`output.template` needs a `file:`, and each binding a `sheet:` with either a table block (`query`, optional `anchor`, `columns`) or a single cell (`cell` with `value`, or `query` + `column`).

### invalid-timezone

A timezone isn't a known IANA name.

`timezone:` takes an IANA name such as `Australia/Sydney` or `UTC`, not an offset or abbreviation.

### invalid-timing

A timing in timings.yml is wrong.

Each timing is a map with exactly one of `cron`, `every` or `rrule`, and optionally `starting`, `at`, `except`, `also` and `timezone`.

### invalid-version-constraint

A plugin's version constraint doesn't parse.

Constraints are semver requirements such as `1.2.0`, `>=1.0` or `^1`.

### jinja-syntax

A template doesn't compile.

A Jinja template in SQL or YAML has a syntax error: an unclosed `{{` or `{%`, an unknown tag. The message has the line.

### misplaced-packages

`packages:` is in the wrong file.

Macro packages are declared in dependencies.yml or packages.yml at the project root.

### missing-field

A required key is missing.

The file leaves out a key it needs, e.g. `name:` in dre_project.yml, or the name of a report outside a report folder.

### missing-queries

A report has no queries.

A managed report needs a non-empty `queries:` list naming `.sql` files under `reports/`. A YAML file that only configures a report must name one that declares `queries:`.

### missing-target-entry

A profile has no entry for the run's target.

Each profile the run uses picks an entry by `--target`, else `DRE_TARGET`, else its own `target:`, else `dev`. Add that entry to the profile's `targets:`, or choose another target. A destination can deliver nowhere on a target with `{deliver: false}`.

### missing-template

A template file doesn't exist.

`output.template.file` names a file that isn't in the project. Paths are relative to the project root.

### moved-plugin-declaration

Plugins are declared under an old key.

Plugin packages are declared under `plugins:` (usually in dependencies.yml), not `destinations:`, `formats:` or a list of names under `sources:`.

### no-connection

A query has no connection.

Nothing gives the query a connection: give it `profile:`, use a source with a `profile:`, or give the report one (`profile:` on the report or Set, a folder's `+profile`, or `default_profile` in dre_project.yml).

### no-result-set

A query that makes a tab returned no result set.

The query's last statement returned no rows to write (it created a view, set a variable). If it only prepares data for later queries, give it `tab: false` in the YAML.

### package-missing

A macro package isn't there.

A local package's folder doesn't exist, or a git package isn't installed; run `dre deps`.

### package-name-clash

A macro package's name is already taken.

Package names are Jinja variables, so one can't share a name with a DRE function, a project macro or the project itself.

### parse-failed

A query doesn't render.

Rendering a query without a database failed: a macro error, a bad `source()` or `ref()`, a function used where it can't run. The message has the cause.

### profile-and-sets

A report sets both `profile:` and `sets:`.

With Sets, each Set chooses its connection; give the report one or the other.

### profiles-missing

The project uses profiles but there's no profiles.yml.

DRE looks for profiles.yml in `--profiles-dir`, `DRE_PROFILES_DIR`, the project directory, then `~/.dre`. Create one with `dre init`, or point DRE at yours.

### profiles-sources-renamed

profiles.yml uses the old `sources:` section.

DRE 0.2 renamed profiles.yml's `sources:` to `connections:`. Rename it.

### ref-cycle

`ref()` calls go round in a circle.

Shared SQL files `ref()` each other in a loop. The message shows the cycle; break it.

### removed-key

A key that was removed.

The key belonged to an older DRE. The message says what replaces it.

### removed-template-name

A template uses a name that was removed.

The function or variable was renamed in an earlier release; the message says what to use.

### render-failed

A template failed to render during the run.

A destination, message or `when:` template failed to render with the run's results. The message has the cause.

### schedule-moved

A schedule is set where schedules are no longer written.

Schedules moved from report and folder YAML (`schedule:`, `+schedule`) to schedules.yml, as named entries.

### schedule-needs-anchor

A timing needs a start date.

`every`, and an rrule with `INTERVAL` above 1, a `COUNT` or a day taken from its start, need `starting:` to know which days to fire on.

### schedule-no-time

A timing has no time of day.

An `every` or `rrule` timing without `at:` (or `BYHOUR`) fires at midnight. Add `at:` to fire at another time.

### schedule-path-clash

Two schedules of one Binding deliver to the same path.

With the same vars and date, two schedules would write the same file and one would overwrite the other. Give them different vars, or a path that tells them apart.

### schedule-seconds

A cron expression has seconds.

DRE's cron expressions have five fields (minute to weekday); a sixth field of seconds isn't supported.

### schedule-timezone-mismatch

A schedule fires in another timezone than its report renders in.

The run date is the report's timezone's date at the firing time, which may not be the day you expect. Set `timezone:` on the schedule to fire and render in one.

### schedule-too-frequent

A timing fires more often than DRE allows.

Schedules fire at most every minute; the message has the interval.

### setup-on-other-connection

A setup statement runs on another connection than the queries after it.

A `tab: false` query prepares state (temp tables, `SET`s) that only exists on its own connection, but a later query reads it on another. Run them on one connection.

### source-key-not-supported

A dbt source key DRE doesn't use yet.

The key is valid dbt but DRE ignores it for now (freshness, loader, tests, ...). The declaration still works.

### target-mismatch

The profiles are on another target than the run.

Every profile the run uses chooses a different entry than the run's target (`--target`, `DRE_TARGET`, else `dev`). Pass `--target` or set `DRE_TARGET` so `target.name` matches.

### target-path-unwritable

The target path can't be written.

DRE couldn't create or write the target folder (permissions, a read-only mount, a full disk). The message has the reason.

### unknown-default-set

`default_set` isn't one of the report's Sets.

`default_set` (in the report or dre_project.yml) must name one of the report's `sets:`.

### unknown-folder

Folder config names a folder that doesn't exist.

Folder config in dre_project.yml (`reports:`) sets something for a folder that isn't under `reports/`. Check the spelling and nesting, or remove it.

### unknown-key

A key DRE doesn't know.

The file has a key that isn't part of its format, usually a typo or a key in the wrong place (a report key in dre_project.yml). The message lists the keys that are allowed there; the YAML references list them all.

### unknown-profile

A profile isn't defined in profiles.yml.

The connection or destination profile isn't under `connections:`/`destinations:` in the profiles.yml DRE found. Check the name, or `dre validate -v` for where DRE looked.

### unknown-query

A query name doesn't match a .sql file or one of the report's queries.

The name in `queries:` (or a Set's `exclude:`/`queries:`, an output's `queries:`, `tab_names:`) doesn't match a `.sql` file under `reports/`, or isn't one of the report's queries. Check the spelling.

### unknown-ref

`ref()` names nothing.

`ref('name')` must name a `.sql` file under `reports/` or a lookup under `lookups/`.

### unknown-run-attribute

`run.<x>` isn't part of the run context.

Templates can use `run.report`, `run.set`, `run.target`, `run.schedule`, `run.date` (with its navigation, e.g. `.prev_month`) and `run.now`.

### unknown-set

A report names a Set that doesn't exist.

A name in a report's `sets:` must be declared in `sets.yml` (or be a map that declares it inline).

### unknown-timing

A schedule names a timing that doesn't exist.

`timing:` must name an entry of timings.yml; the message lists them.

### unmanaged-report

A .sql file runs as an unmanaged report.

A `.sql` file no YAML lists in `queries:` (and no `ref()` uses) is an unmanaged report: it runs with the folder's settings into a csv. That's for quick tests; add a YAML to make it a managed report.

### unrecognized-yaml

A YAML file isn't a report, Set, plugin or schedule file.

DRE recognises project YAML files by their keys. A file outside `reports/` that's none of a report (`queries:`), Sets, timings, schedules, `plugins:` or `sources:` is ignored. Move it, or fix its keys.

### unresolved-var

`var()` has no value.

No level sets the variable: `--var`, the schedule, the Set, the report, the folders, or the project. Set it at one of them, or give `var()` a default.

### unset-env-var

`env_var()` names an unset variable.

The environment variable isn't set and `env_var()` has no default. Set it, or give a default: `env_var('NAME', 'fallback')`.

### unused-query

A query feeds no output.

Every output names its queries, and this one is in none of them. Add it to an output's `queries:`, or give it `tab: false` if it only prepares later queries (temp tables, `SET`s).

### unused-timing

A timing isn't used by any schedule.

Remove it, or use it with `timing:` in a schedule.

### yaml-syntax

A YAML file doesn't parse.

The file isn't valid YAML: an unclosed bracket or quote, a tab used for indentation, a key written twice. The message names the line. Fix the file; `dre validate` reports every file's problems in one pass.

## Plugins

### format-failed

An output couldn't be written in its format.

The format plugin failed to write the output (bad options, a template it can't fill, a result it can't represent). The message has the plugin's reason.

### options-unchecked

A plugin's options weren't checked.

The plugin is too old to describe its options, or couldn't be asked. Run `dre plugin update` to get a version that checks them.

### plugin-install-failed

A plugin couldn't be installed.

Downloading or verifying a plugin package failed. The message has the reason (network, checksum, no build for this platform).

### plugin-not-found

There's no plugin to check options against.

The plugin isn't installed, so its options couldn't be checked. Run `dre deps`.

### plugin-not-installed

A declared plugin isn't installed.

Run `dre deps` (or any command without `--no-auto-install`) to install the project's plugins.

### undeclared-plugin

The project uses a plugin no declared package provides.

A connection type, format or destination type needs a plugin package listed under `plugins:`. Add the package the message names, then run `dre deps`.

## Refused: unsafe statements

### unmanaged-side-effect

An unmanaged report runs a statement it may not.

Unmanaged reports may only run `SELECT`/`WITH` and create temp tables or views, so a stray `.sql` file can't change data. Rewrite the statement, or give the report a YAML.

## Connections

### connection-failed

A connection couldn't be opened.

The source plugin couldn't connect: the host is unreachable, the credentials are refused, the warehouse is unavailable. The message has the plugin's reason. Trying again can work when the cause is temporary.

## Queries

### query-failed

A query failed on the database.

The database rejected or failed a statement: a syntax error, a missing table, a permission. The message names the file and line.

## Delivery

### delivery-failed

An output couldn't be delivered.

The destination plugin failed to deliver the file or message. The message has the plugin's reason; the output stays in the target path.

## Internal

### io-error

A file couldn't be read.

DRE couldn't read a file it found (permissions, a broken link, a file that vanished). The message has the operating system's reason.

### run-failed

A Binding failed for a reason without its own code.

Something went wrong while running the Binding that isn't one of the other run errors. The message has the cause; please report it if it looks like a bug.
