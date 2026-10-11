---
title: "First-party plugins"
description: "The first-party plugins, their profile fields and output options."
section: plugins
position: 3
---

# First-party plugins

The complete [examples](examples.md) cover local output, workbooks, fixed-width feeds, report
variants, headline messages and SFTP delivery. Each plugin page below adds its profile recipe
and common errors. [Dependency keys](reference-dependencies.md) describe declarations;
the [glossary](glossary.md) defines source plugin, format, destination and plugin package.

Plugins come in packages, declared once each under `plugins:` in `dependencies.yml` (see
[the registry docs](registry.md)). A source or destination is configured through a profile in
`profiles.yml` (under `connections:` or `destinations:`) whose target has its `type`. Fields holding
secrets can use `env_var()`.

| Package | Provides |
|---|---|
| `duckdb` | the [`duckdb`](plugin-duckdb.md) source |
| `postgres` | the [`postgres`](plugin-postgres.md) source |
| `databricks` | the [`databricks`](plugin-databricks.md) source, and the [`databricks`](plugin-databricks.md) destination (Volumes and workspace files) |
| `bigquery` | the [`bigquery`](plugin-bigquery.md) source (alpha: 1.0.0 pre-releases) |
| `snowflake` | the [`snowflake`](plugin-snowflake.md) source (alpha: 1.0.0 pre-releases) |
| `csv` | the [`csv`](plugin-csv.md) and [`delimited`](plugin-csv.md) formats |
| `fixed_width` | the [`fixed_width`](plugin-fixed_width.md) format |
| `parquet` | the [`parquet`](plugin-parquet.md) format |
| `xlsx` | the [`xlsx`](plugin-xlsx.md) format |
| `object_store` | the `s3`, [`gcs`](plugin-gcs.md) and [`azure_blob`](plugin-azure_blob.md) destinations |
| `sftp` | the [`sftp`](plugin-sftp.md) destination |
| `ftp` | the [`ftp`](plugin-ftp.md) destination |
| `email` | the [`email`](plugin-email.md) destination |
| `slack` | the [`slack`](plugin-slack.md) destination |
| `teams` | the [`teams`](plugin-teams.md) destination, messages only (release candidate: 1.0.0-rc.1) |
| `google_chat` | the [`google_chat`](plugin-google_chat.md) destination, messages only (release candidate: 1.0.0-rc.1) |

```yaml
# dependencies.yml
plugins:
  - databricks
  - xlsx
  - object_store
```

Each plugin has its own page, with its profile fields, options and notes:

- Sources: [duckdb](plugin-duckdb.md), [postgres](plugin-postgres.md),
  [databricks](plugin-databricks.md), [bigquery](plugin-bigquery.md),
  [snowflake](plugin-snowflake.md).
- Formats: [csv and delimited](plugin-csv.md), [fixed_width](plugin-fixed_width.md),
  [parquet](plugin-parquet.md), [xlsx](plugin-xlsx.md), and the built-in
  [`message`](#the-message-format).
- Destinations: [s3](plugin-s3.md), [gcs](plugin-gcs.md), [azure_blob](plugin-azure_blob.md),
  [sftp](plugin-sftp.md), [ftp](plugin-ftp.md), [databricks](plugin-databricks.md#as-a-destination),
  [email](plugin-email.md), [slack](plugin-slack.md), [teams](plugin-teams.md),
  [google_chat](plugin-google_chat.md), and the built-in `local`.

This page has what they share.

## Sources

### Types from warehouses

Every warehouse source (`databricks`, `bigquery`, `snowflake`) sends the same kinds of value the
same way, so a report looks the same whichever warehouse it reads:

- Scalars keep their type. Exact numbers (`DECIMAL`, `NUMERIC`, `NUMBER(p,s)`) stay decimals at
  their declared precision; Snowflake `NUMBER(p,0)` up to 18 digits is an integer. Zone-aware
  timestamps (`TIMESTAMP`, `TIMESTAMP_TZ`/`_LTZ`) are UTC; naive ones (`TIMESTAMP_NTZ`,
  BigQuery `DATETIME`) stay naive.
- Semi-structured and nested values (`VARIANT`, `OBJECT`, `ARRAY`, `STRUCT`, `MAP`, `JSON`,
  BigQuery `RANGE`) become compact JSON text on one line, whatever the database returned. csv
  and `delimited` quote it like any other text, so a file never gets a line break in the middle
  of a record.
- Types no format holds exactly become text: intervals (Databricks' own text; BigQuery as ISO
  8601, e.g. `P1Y2M3DT4H`), geography and geometry, vectors, and `BIGNUMERIC` values wider than a
  38-digit decimal.
- A column of `NULL`s (`SELECT NULL`) is text.

xlsx refuses text longer than an Excel cell holds (32,767 characters): the run fails naming the
sheet, the cell and the column, rather than cutting the value. Shorten or cast it in the SQL, or
write that query to a text format.

## Formats

Each format plugin declares and checks its own options: `dre validate` and `dre run` send every
report's `output:` keys to the plugin before anything runs, and report each problem with the
report it came from. A format no declared package provides, or whose package isn't installed, is
an error. For one no declared package provides, `dre validate` and `dre run` look it up in DRE's
plugin registry and name the package to add under `plugins:` in `dependencies.yml`.

Project-wide defaults for a format go in `dre_project.yml` under `format_options`, keyed by
format. They apply under every output of that format, whatever folder or report chose it, and a
report's own keys win:

```yaml
format_options:
  delimited: {delimiter: "|", quoting: strings}
  csv: {quoting: all}
```

| Format | Options |
|---|---|
| `csv`, `delimited` | `delimiter`, `quote`, `quoting`, `header`, `line_ending`, `encoding`, `null`, `byte_order_mark` |
| `fixed_width` | `columns` (see [Fixed-width columns](plugin-fixed_width.md#columns)), `header`, `line_ending`, `encoding`, `line_breaks` |
| `parquet` | none; Arrow types are preserved |
| `xlsx` | `header`, `max_rows_per_sheet`, `autofit` (see [Column widths](plugin-xlsx.md#column-widths)), `style` (see [Styles](plugin-xlsx.md#styles)), `columns`, `date_format`, `datetime_format`, `time_format` (see [xlsx column formats](plugin-xlsx.md#column-formats)), `totals_label` (see [xlsx formulas and totals rows](plugin-xlsx.md#formulas-and-totals-rows)); per query `anchor`/`header`/`autofit`/`style`/`columns`; `template` |
| `message` (built in, no plugin) | `text` or `file`, `title`, `max_rows` (see [The `message` format](#the-message-format)) |

Every format but xlsx also takes `extension`: the output file's extension (`aba`, `dat`, ...), or
`""` for none. The file is written the same way; only its name changes.

- Timestamps with a timezone are written in their zone with the offset,
  `2026-01-01 11:00:00+11:00`; timestamps without one as `2026-01-01 00:00:00`.

### The `message` format

`message` is built into DRE (see [Messages](messages.md) for a guide with examples). It renders
the output's query results through Jinja into a short headline: a title, plus text in a small Markdown subset (`**bold**`, `*italic*` or `_italic_`,
`` `code` ``, `[text](url)` and `- ` bullets; no headings or tables). Destinations that take
messages post it natively; every other destination delivers it as a `.md` file, which is also
written to the target path (`<report>.md`, or `<name>.md` for a named output; `extension:`
changes the extension).

| Option | Default | |
|---|---|---|
| `text` | the default template | The message, a Jinja template. |
| `file` | | The message template in a file, relative to the project root or `templates/`. Not with `text`. |
| `title` | `<report>: <run date>` | A Jinja template; the subject line of an email, the heading in chat. |
| `max_rows` | `1000` | How many rows of each query `results.<query>.rows` holds. A warning says when it's reached; file outputs of the same query still get every row. |

Templates read `results.<query>` for each of the output's queries:

- `value`: the first column of the first row (`none` with no rows);
- `first.<column>`: a column of the first row;
- `rows`: the rows (at most `max_rows`), each readable as `row.<column>` or `row[0]`;
- `row_count`: the true number of rows, even past `max_rows`;
- `columns`: the column names;
- `sets[n]`: a result set by index, each with the same fields. One `.sql` file makes one result set
  (its last statement's), so `sets[0]` and `sets[-1]` are the query's result.

Values keep their types: numbers stay numbers, dates and timestamps are DRE dates (`.iso`,
`.yyyymmdd`, `.format()`, ...), nulls are `none`. Every value a template prints is escaped for Markdown, so a
`*` or `_` in the data stays literal; `| safe` prints a value as written. The number filters
(`number`, `percent`, `signed`, `currency`, `compact`; see [Templates](templates.md)) make values
readable, and `var()`, `run.*` and macros work as in every template.

With neither `text` nor `file`, the default template writes one block per query: a single value
as `column: value`, one row as `column: value` lines, several rows as a list of at most ten,
then `+ N more`. Numbers are written with `number` in the Binding's locale (two decimals unless
whole).

A message whose text renders empty (after trimming) is skipped, like an output whose `when:` is
false: nothing is written or delivered, and `run_results.json` records it as `skipped`.
`dre run --preview` prints each message (title, text, length, whether `when:` passed) and delivers
nothing; numbers then come from the row sample. A real run logs one line per message, and
`run_results.json` keeps the full title and text.

## Destinations

The built-in `local` destination copies the file to a path, relative to the project. It needs no
plugin and no declaration. It takes the options `if_exists`, `atomic` and `temp_dir`, described
below.

A destination entry's keys other than `profile` and `path` are the plugin's options, and the
plugin checks them the same way formats do, against the destination profile's entry for the
run's target. A value holding Jinja is checked once it's rendered, at delivery.

Every destination that talks to a server takes two timeouts in its profile entry, as durations
(`30s`, `5m`) or seconds:

| Field | Default | Meaning |
|---|---|---|
| `connect_timeout` | `30s` | How long to wait for a connection. |
| `timeout` | `60s` | How long a request may make no progress (the server sends or accepts nothing) before it fails. For object stores, how long one request may take; large files go up in parts. |

There's no limit on how long a whole upload takes as long as it keeps moving. To bound a whole
run, use the run's timeout (`dre run --timeout`, `DRE_RUN_TIMEOUT`, `flags: run_timeout`).

### Tries again

A temporary failure is tried again: a connection refused, reset or timed out, an HTTP 429 or a
5xx. The waits are about 1s, 4s and 16s (a little random), or the server's `Retry-After`. Each
retry is logged at info level, and a delivery that took several tries has `attempts` in
`run_results.json`. Refused credentials or permissions, and other 4xx answers, fail at once.

| Field | Default | Meaning |
|---|---|---|
| `retries` | `3` | How many times to try again, in the profile entry of every destination and source that talks to a server. `0` never tries again. |

Nothing is ever sent twice:

- **Files** (`sftp`, `ftp`, object stores, `databricks`): an upload is tried again whole; with
  `atomic` (the default) the half-written temporary file is replaced, never shown.
- **Email**: only when the server certainly didn't accept the message: the connection failed
  before the session began, or the server answered 4xx (try later). A connection dropped after
  the message was sent isn't tried again.
- **Chat posts** (`slack`, `teams`, `google_chat`): only on a 429 or 503, or when no connection
  was made.
- **Sources** (`postgres`, `databricks`, `bigquery`, `snowflake`): only while connecting or
  signing in, never once a query is sent.

### A file already at the path

By default a delivery **replaces** a file already at its path: the usual reason a name is taken
is a rerun of a corrected report, and replacing the bad file is what's wanted. Every file
destination (`local`, `s3`, `gcs`, `azure_blob`, `sftp`, `ftp`, `databricks`) takes
`if_exists` per destination entry to change that:

```yaml
destination:
  profile: client_sftp
  path: "outbound/monthly-{{ run.date.yyyymm }}.xlsx"
  if_exists: error      # overwrite (default) | error | number
```

| Value | What happens |
|---|---|
| `overwrite` | The new file replaces the old one. |
| `error` | That delivery fails (code `<plugin>/file-exists`, `local/file-exists` for `local`); the run's other deliveries still go, and the run exits 1. |
| `number` | Both are kept: the new file is saved as `<name>_2.<ext>`, else `_3`, and so on. `run_results.json` (`location`) and the log show the name used. |

The check and the write are one step where the server allows it: SFTP's exclusive create and
no-replace rename, conditional uploads on object stores (`If-None-Match: *`, GCS's
`ifGenerationMatch=0`), `overwrite=false` on Databricks, and an exclusive create for `local`. FTP
has no such step, so DRE looks first, then uploads: two runs at the same moment could both see the
name free. Put a date or a period in delivery paths so different runs don't collide by accident.

### Uploads under a temporary name

The `local`, `sftp` and `ftp` destinations write a file as `.<name>.dre-part` in the same folder,
then rename it to its final name, so a dropped connection or a stopped run never leaves a
half-written file where a receiving system may pick it up. Two options, per destination entry:

| Option | Default | Meaning |
|---|---|---|
| `atomic` | `true` | `false` writes straight to the final name (for a server that forbids renames). |
| `temp_dir` | | Where the temporary file goes, on the same server: absolute, or relative to the file's folder (`../staging`). For a receiver that picks up any new file, even a hidden one. |

Replacing a file already there: SFTP's rename can't replace one, so DRE removes the old file just
before the rename (a moment with no file at that name); an FTP server's rename usually replaces it
in one step. Object stores (`s3`, `gcs`, `azure_blob`) and Databricks Volumes and Workspace files
only show a file once its upload completes, so they need no temporary name.

### Several destinations

`output.destination` takes one destination or a list. Each entry names a profile, an optional
`path`, and any options its plugin takes (recipients, a channel, a message). Options are rendered
with the same Jinja context as paths:

```yaml
output:
  format: xlsx
  destination:
    - profile: reports_s3
      path: "s3://reports/{{ var('client') }}/monthly-{{ run.date.yyyymmdd }}.xlsx"
    - profile: finance_mail
      to: "{{ var('client') }}-finance@example.com"
      subject: "Monthly report {{ run.date.iso }}"
    - profile: team_slack
      channel: "#finance-reports"
      message: "Monthly report for {{ var('client') }}"
```

- Entries are delivered in order. If one fails, the rest are still attempted; the Binding then
  fails and the run exits 1 (see [exit codes](exit-codes.md)).
- Each entry uses its profile's entry for the run (`--target`, `DRE_TARGET`, else the profile's
  own `target:`, else `dev`). A profile with no such entry is an error before anything runs; an
  entry `{deliver: false}` delivers nowhere, logged, while the others are delivered.
- `run_results.json` lists every entry under `deliveries`, with `profile`, `type`, `target`,
  `status` (`delivered`, `not_delivered` for `deliver: false`, or `failed`), `location` and
  `error`.
- A Set can replace the whole list. Overriding only `path:` works when exactly one destination
  is inherited; with several, override the full list.
- The local file is named after the first entry's `path`. `--output-path` and `--output-name`
  apply to every entry that has a path, of the first output only when a report has several.
- Credentials stay in `profiles.yml`. Options belong to the report, so a Set can address its own
  recipients.
- The `email` destination always attaches the output file, so an output over its size limit
  fails that entry; DRE can't email a link instead (see [`email`](plugin-email.md)).
- A destination fails the delivery if its entry has a key it doesn't take, so a misspelt `path`
  is caught instead of ignored.

<!-- docs-nav: generated by .github/scripts/docs_sections.py from docs/sections.json -->

---

**Previous:** [Plugin packages, the registry and `dre.lock`](registry.md) · **Next:** [DuckDB](plugin-duckdb.md)
