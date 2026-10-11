---
title: "Project configuration"
description: "Share defaults, override reports and Sets, and bound template queries."
section: build-reports
position: 7
---

# Project configuration

Use `dre_project.yml` for settings shared by reports. Start with a project name and a
connection profile from [profiles.yml](connections.md):

```yaml
name: reporting
default_profile: warehouse
```

Add [plugin dependencies](registry.md) separately in `dependencies.yml`, and keep credentials
in profiles. The [project reference](reference-project.md) lists all keys; the
[glossary](glossary.md) defines profiles, Sets, outputs and targets.

## Share defaults without hiding exceptions

For a finance team that exports workbooks and delimited feeds:

```yaml
name: finance
default_profile: warehouse
default_output: {format: xlsx, header: true}
format_options:
  csv: {header: true, encoding: utf-8, null: "NULL"}
default_set: domestic
vars: {currency: AUD, include_draft: false}
timezone: Australia/Sydney
locale: en-AU
week_start: monday
week_numbering: iso
reports:
  finance:
    +profile: warehouse
    +tags: [finance]
    +vars: {department: finance}
    +output: {format: xlsx}
    +timezone: Australia/Sydney
    +locale: en-AU
    exports:
      +output: {format: csv}
```

Folder names under `reports:` mirror directories under `reports/`. Prefix settings with `+`;
an unprefixed key names a subfolder. A report's own settings override folder defaults.
`+tags` adds selector tags to every report below a folder, including nested folders.
`default_output` starts every report; `format_options` applies only to outputs of its format.
A report's output map changes inherited keys, while an output list replaces the inherited outputs.
See the complete [monthly finance](../examples/monthly-finance/) and
[regional reports](../examples/regional-reports/) projects for report files and SQL.

Variables follow project, folder, report, Set, schedule, then `--var` precedence, highest last.
Values keep their YAML types: `false` is a boolean, not the text `"false"`. Timezone affects
`run.date` and `run.now`; locale affects number filters. Neither converts timestamps returned
by the database. [Templates](templates.md) explains date arithmetic and number formatting.
`week_start` chooses Monday or Sunday for week boundaries; `week_numbering` chooses ISO or
US week numbers. Set both deliberately when exported periods must match a business calendar.

A project's `default_set` chooses a report's variant only when that report declares the name.
A report's own `default_set` takes precedence and must name one of its declared Sets;
otherwise validation reports `unknown-default-set`. A report with one Set needs no explicit
default. With several Sets and no applicable default, select one with `--set` for unattended runs.

## Change one report variant

A report's `name` defaults to its folder name; set it explicitly when that would collide with
another report. Its `tags` support `tag:<tag>` selection and combine with folder tags.

A Set reuses a report while changing the data and presentation. Declare shared connection and
variable values in `sets.yml`; put report-specific query overrides in that report's YAML:

```yaml
# sets.yml
domestic: {profile: warehouse, vars: {region: AU}, locale: en-AU}
international: {profile: warehouse, vars: {region: EU}, locale: de-DE}
```

```yaml
# reports/sales/sales.yml; summary.sql and detail.sql sit beside it
queries: [summary, detail]
sets:
  - domestic
  - name: international
    profile: warehouse
    vars: {region: EU}
    locale: de-DE
    exclude: [detail]
    tab_names: {summary: European Sales}
default_set: domestic
output: {format: xlsx}
```

`exclude` removes queries without changing the others. Use a Set's `queries` instead when order
or settings need replacing; do not combine both (`exclude-and-queries`). `tab_names` changes
sheet labels without renaming SQL files. `output` on a Set changes the inherited output map,
or a named output when several exist; a list replaces all outputs. The complete
[regional example](../examples/regional-reports/) keeps separate files for each layer.
See [Set keys](reference-sets.md) and [inline Set overrides](reference-report.md#sets).

## Bound template work

```yaml
run_query_max_rows: 500
lookup_inline_max_rows: 100
mask_secrets: true
```

`run_query_max_rows` bounds rows returned to Jinja by `run_query()`, not rows streamed into the
report output. Use SQL aggregation or a narrower query when the template exceeds it; increasing
the limit can increase memory use. `lookup_inline_max_rows` chooses when a lookup uses a temporary
table instead of SQL literals. A source without temporary-table support falls back to inline SQL.
See [template queries](templates.md) and [lookup loading](lookups.md).

Keep `mask_secrets` enabled so `DRE_SECRET_*` values are masked in generated diagnostics and
compiled SQL. It does not scrub query results: do not select secrets into report data.
See [environment variables](environment-variables.md).

## Operational settings

```yaml
target_path: target
flags:
  keep_runs: 7
  run_timeout: 30m
  http_timeout: 120
```

`target_path` stores artifacts, not the `dev` or `prod` environment. Persist it on scheduled
runners to preserve drift snapshots. `keep_runs` controls retained run folders, including failed
runs; `run_timeout` bounds the complete run; `http_timeout` bounds download inactivity, not query
time. Their environment variables override YAML, and command flags override environment values
where available. See [target storage](target-path.md), [cancellation](orchestration.md#cancelling-a-run)
and [plugin downloads](managing-plugins.md).

Macro `dispatch` and `plugins` choose dependency resolution rather than report behavior.
[Registry recipes](registry.md#other-places-to-install-from) cover `name`, `version`, `github`,
`registry` and `local`; [macro packages](registry.md#macro-packages) cover namespace and search order.

## Fix configuration errors

- `unknown-folder`: align the folder nesting with directories on disk.
- `unknown-key`: check the file type; report keys belong in report YAML, not the project root.
- `profile-and-sets`: remove report-level `profile` when using Sets and give each Set its connection.
- `unknown-query`: use the SQL file's stem in `queries`, `exclude` and `tab_names`.
- `no-connection`: set `default_profile`, a folder profile, a report/Set profile, or a source's profile.

These are DRE's stable [error codes](reference-error-codes.md). `dre explain <code>` prints the
meaning and repair steps. Run `dre validate --strict` after changing defaults so warnings also fail.

<!-- docs-nav: generated by .github/scripts/docs_sections.py from docs/sections.json -->

---

**Previous:** [DRE practices](practices.md) · **Next:** [Tested examples](examples.md)
