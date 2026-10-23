---
title: "Report YAML reference"
description: "Every key of a report YAML file: queries, output, destinations, Sets and templates."
section: reference
position: 4
---

# Report YAML reference

<!-- Generated from docs/schemas by .github/scripts/schema_docs.py. Edit the schema, not this page. -->

A report: SQL queries plus an output, in a YAML file under `reports/`.

Where: a `.yml` file under `reports/`.

For editor autocomplete and validation, add this as the first line of the file ([editor setup](editor-setup.md)):

```yaml
# yaml-language-server: $schema=https://getdre.com/schemas/v0.7/report.schema.json
```

For copyable examples and common errors, see [Build and run reports](building-reports.md). The [glossary](glossary.md) defines DRE's terms.

## Keys

| Key | Type | Default | Description |
|---|---|---|---|
| `name` | string |  | The report's name. Default: the name of the folder the YAML file is in. Report names are unique across the project. |
| `description` | string |  | What this report answers, including its business definitions and units. |
| `meta` | map |  | Metadata for external tools. DRE checks its shape, not the meaning of its keys. |
| `tags` | list of string |  | Tags to select the report with, `-s tag:<tag>`. |
| `queries` | list of string or map (see below) |  | The `.sql` files whose results make the report's tabs, in the order they run, on one database session. |
| `output` | map (see below) or list of map (see below) |  | One output, or a list of outputs formatted from the same run of the queries (file outputs are delivered before messages). A list replaces the inherited output; a map changes it. |
| `profile` | string |  | The connection (in `profiles.yml`) the queries run on, unless a query's own `profile:` or a source's says otherwise. Default: the folder's `+profile`, then `default_profile`. Can't be combined with `sets`. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`. |
| `sets` | list of string or map (see below) |  | The Sets the report can run as, by name (declared in `sets.yml`) or declared here. |
| `default_set` | string |  | The Set a plain `dre run` uses. Must be one of `sets`. |
| `vars` | map |  | Variables, read in SQL and YAML with `var('name')`. Values can be strings, numbers, booleans, lists or maps. |
| `var_schema` | map |  | Optional declarations for variables: type, default, allowed values, description, required, and for strings a pattern and max_length. Kept separate from `vars` so a map-valued variable remains a plain value. |
| `timezone` | string |  | The timezone `run.date` and `run.now` use, an IANA name such as `Australia/Sydney`. Default: UTC. |
| `locale` | string |  | The locale this report's number filters format for (`de-DE`). Default: the folder's `+locale`, then the project's `locale`, then `en`. |
| `plugins` | list of plugin packages: a name, `name: "<version>"`, or a map (see below) |  | The plugin packages this project uses. DRE installs them on demand into `dre_deps/` and pins them in `dre.lock`. May be written in any project YAML file; `dependencies.yml` is the usual place. |
| `sources` | map, as in [the sources reference](reference-sources.md) |  | dbt-style source declarations (see the sources schema). May be written in any project YAML file. |

## `queries[]`

A query with settings, instead of just its name.

| Key | Type | Default | Description |
|---|---|---|---|
| `query` (required) | string |  | The name of a `.sql` file under `reports/`, without folder or extension. |
| `profile` | string |  | The connection this query runs on, over the report's, Set's, folder's and project's. Must agree with the `profile` of any source it uses. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`. |
| `tab` | boolean | `true` | `false` runs the file only for what it does (temp tables, `SET`s) and discards any result, so it gets no tab. |
| `tab_name` | string |  | The tab (sheet) name. Default: the file's name. Not allowed with `tab: false`. |
| `anchor` | string |  | Where the data starts on the sheet (xlsx only). Default: `A1`. |
| `header` | boolean |  | Whether to write the column names as the first row (xlsx only). Default: the output's `header`. |
| `columns` | map |  | Per-column settings for this tab, by column name (xlsx only). |
| `on_empty` | string |  | When this query returns no rows: `continue` keeps its empty tab or section, `skip` leaves its tab (or per-query file) out, `fail` fails the run. Wins over the output's `on_empty`. Default: the output decides once every query is empty. |
| `secret_columns` | list of string |  | Columns whose values are secrets (a PDF or zip password): values are masked as `*****` (one under 4 characters where it stands alone) in the console, logs, `run_results.json`, `rows.jsonl`, `--output-json` and previews. Never masked in delivered files or messages. |
| `autofit` | boolean |  | Size this tab's columns from their content (xlsx only), over the output's `autofit`. Default: the output's, which is on. |
| `style` | map (see below) |  | How this tab looks (xlsx only), over the output's `style`: `font`, `header`, `totals`, `banded_rows`, `borders`, and cell keys (`bold`, `fill`, ...) for every data cell. See the xlsx plugin page. |

## `queries[].columns.<name>`

Settings for one column of an xlsx tab.

| Key | Type | Default | Description |
|---|---|---|---|
| `format` | string |  | The Excel number format of the column, e.g. `#,##0.00` or `dd/mm/yyyy`. See the xlsx column formats in the plugins reference. |
| `formula` | string |  | An Excel formula for each row of this column; `{name}` stands for that column's cell on the same row, e.g. `=ROUND({qty}*{unit_price},2)`. The SQL selects a placeholder column where the formula goes. |
| `total` | string |  | Puts a total under the column: one of `sum`, `count`, `average`, `min`, `max`, or a formula such as `=SUM({net:*})`. |
| `style` | map |  | How the column's data cells look: `bold`, `italic`, `underline`, `font` (`{name, size}`), `font_color`, `fill` (`"#RRGGBB"`), `align` (`left`, `center`, `right`), `border` (`none`, `thin`, `medium`). Over the tab's and output's `style`. |
| `width` | any or number |  | The column's width: `auto` (sized from its content, at most 60 characters) or a number of characters. Over the tab's and output's `autofit`. |

## `queries[].style`

How this tab looks (xlsx only), over the output's `style`: `font`, `header`, `totals`, `banded_rows`, `borders`, and cell keys (`bold`, `fill`, ...) for every data cell. See the xlsx plugin page.

| Key | Type | Default | Description |
|---|---|---|---|
| `header` | map |  | Cell keys for the header row (default: bold). |
| `totals` | map |  | Cell keys for the totals row (default: bold, a thin top border). |
| `banded_rows` | string or any |  | The fill of every other data row, `"#RRGGBB"`, or `false`. |
| `borders` | `none` or `thin` or `medium` |  | A border around every cell of the table. |
| `bold` | boolean |  | Bold text in every data cell. |
| `italic` | boolean |  | Italic text in every data cell. |
| `underline` | boolean |  | Underlined text in every data cell. |
| `font` | map (see below) |  | The font: `{name: Calibri, size: 11}`. |
| `font_color` | string |  | The text colour, `"#RRGGBB"`. |
| `fill` | string |  | The background colour, `"#RRGGBB"`. |
| `align` | `left` or `center` or `right` |  | Horizontal alignment. |
| `border` | `none` or `thin` or `medium` |  | A border around each data cell. |

## `queries[].style.font`

The font: `{name: Calibri, size: 11}`.

| Key | Type | Default | Description |
|---|---|---|---|
| `name` | string |  | The font's name. |
| `size` | number |  | The size in points. |

## `output[]`

How a report's result is written and where it goes. Besides the keys below, each format takes its own options (for example `delimiter` for `delimited`, `columns` for `fixed_width`, `text` for `message`); they are documented with the formats.

| Key | Type | Default | Description |
|---|---|---|---|
| `name` | string |  | Names the output so other outputs and Set overrides can refer to it (`outputs.<name>` in a message, `attach:`). Also the default file name (`<name>.<ext>`). Unique within the report. |
| `format` | string |  | The output format: `message` (built in: a short headline rendered from the results), or `csv`, `delimited`, `fixed_width`, `parquet` or `xlsx` (each a plugin). Default: `csv`, or the project's `default_output`. |
| `queries` | list of string |  | Which of the report's queries this output formats. Default: all of them. Each query runs once, whatever the number of outputs. |
| `when` | string or boolean |  | A Jinja expression over the results (`results.<query>.value < 0`); when it's false, the output is skipped and recorded as `skipped`. |
| `destination` | map (see below) or list of map (see below) |  | Where to deliver the file: one destination, or a list to deliver to several in one run. Default: the file stays in the target path. |
| `template` | map (see below) |  | The template an xlsx output fills (a branded workbook, instead of a new one), or an html or pdf output renders (a Jinja HTML + CSS file; see the document plugin). |
| `for_each` | map (see below) or any |  | One copy of the output per row of a driver query (`{query: members, key: member_id}`), each with its own slice of the results and the driver row as `each.*`; `false` turns an inherited one off. See docs/per-row-outputs.md. |
| `file_name` | string |  | A per-row output's file name, with Jinja (`"{{ each.member_id }}_statement.pdf"`). Default: the row's key and the format's extension, under a folder named after the output. |
| `on_empty` | string |  | When every query of the output returns no rows: `continue` (default) writes and delivers it empty, with a warning; `skip` writes and delivers nothing; `fail` fails the run. A query's own `on_empty` wins. See "When there's no data" in docs/building-reports.md. |
| `extension` | string or boolean or null |  | File extension for text formats (e.g. `aba`), instead of the format's own. `""` or `false` means none. Not for xlsx. |
| _other keys_ | | | Options of the plugin that handles this block; see [Plugins](plugins.md). |

## `output[].destination[]`

Where a file is delivered: the name of a destination profile in `profiles.yml`, an optional `path`, and the options of that destination's plugin (e.g. `to`, `subject` and `body` for email). The built-in profile `local` copies the file to a local path.

| Key | Type | Default | Description |
|---|---|---|---|
| `profile` (required) | string |  | The destination profile in `profiles.yml` (under `destinations:`) to deliver with. `local` is built in. It uses its entry for the run (`--target`, `DRE_TARGET`, else the profile's own `target:`, else `dev`); a missing entry is an error, and an entry `{deliver: false}` delivers nowhere. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`. |
| `path` | string |  | Where to put the file: a path, or a URL such as `s3://bucket/key`, depending on the destination. Rendered with Jinja, so it can use `var()`, `run.*`, macros and `destination.*` (this destination's settings). |
| `attach` | string or list of string |  | On a message output's entry, for a destination that takes messages and files (`slack`, `email`): other outputs of the report whose files go with the message. |
| `hold` | boolean |  | Hold this delivery: the output is rendered and every other destination delivered, and this one waits for `dre release`. `dre run --hold` holds every destination. |
| `rate` | string |  | At most this many deliveries to this destination per second, minute or hour (`100/minute`, `5/second`, `20000/hour`), spaced evenly: `60/minute` sends one a second. For `dre run` and `dre release`; `--rate` overrides it. |
| `burst` | integer |  | With `rate`: how many deliveries may go back to back while keeping the same average. Default: 1 (evenly spaced). |
| `zip` | boolean or map (see below) |  | Deliver one zip instead of loose files: the output's files plus every `attach:`ed output's, flat. `true` for no password, or `{password, encryption}`. The zip is named by `path` (which must end in `.zip`), else `<output>.zip`. |
| _other keys_ | | | Options of the plugin that handles this block; see [Plugins](plugins.md). |

## `output[].destination[].zip`

Zip the files with a password.

| Key | Type | Default | Description |
|---|---|---|---|
| `password` | string |  | The zip's password. Jinja: `env_var()`, `var()`, `each.*`. An empty password is an error. |
| `encryption` | `aes256` or `zipcrypto` |  | `aes256` (default): strong; opens in 7-Zip, WinRAR, Keka. `zipcrypto`: weak, but opens in Windows Explorer and macOS Archive Utility. |

## `output[].template`

The template an xlsx output fills (a branded workbook, instead of a new one), or an html or pdf output renders (a Jinja HTML + CSS file; see the document plugin).

| Key | Type | Default | Description |
|---|---|---|---|
| `file` (required) | string |  | The template file (`.xlsx` for xlsx, `.html` for html and pdf): a bare file name, found next to the report's YAML, then in each parent folder, then anywhere in the project (it must be unique); or a path with `/`, relative to the project root. |
| `bindings` | list of map (see below) |  | xlsx only: where each query's data goes in the template. Default: none, so the template is copied as it is. |

## `output[].template.bindings[]`

One block of data in an xlsx template: a table of a query's result, or a single cell.

| Key | Type | Default | Description |
|---|---|---|---|
| `sheet` (required) | string |  | The template sheet to write into. |
| `query` | string |  | The query whose result goes here. Must be one of the Binding's queries. |
| `result_index` | integer |  | Which result set of the query to use, counting from 1. Default: the last. |
| `anchor` | string |  | The top-left cell of a table block. A table block needs a `query`. |
| `header` | boolean |  | Whether to write the column names above the data in a table block. |
| `columns` | list of string |  | The columns of the query to write in a table block, in order. Default: all. |
| `cell` | string |  | Makes this a single-cell binding: the cell to write. Needs exactly one of `value`, or `query` with `column`. |
| `value` | string |  | A fixed value (rendered with Jinja) for a single-cell binding. |
| `column` | string |  | The column of the query's first row to write into a single-cell binding. |

## `output[].for_each`

One copy of the output per row of a driver query (a per-row output): each copy gets only its row's slice of the results, and the driver row as `each.<column>` in every template of the output. `false` turns an inherited `for_each` off.

| Key | Type | Default | Description |
|---|---|---|---|
| `query` (required) | string |  | The driver: a query of the report, one row per copy of the output. It isn't a tab or section of the output itself. |
| `key` | string or list of string |  | The column (or list of columns) that identifies a row, in the driver and in every query of the output. Use an opaque id, never an email or a name: keys appear in file names and logs. |
| `concurrency` | integer |  | How many rows render and deliver at once. Default: 4. `--row-concurrency` and `DRE_ROW_CONCURRENCY` override it. |
| `on_row_error` | string |  | `continue` (default): a failed row is recorded and the others carry on; the run ends failed. `fail`: render every row first, and deliver only if all rendered. |
| `on_orphan_rows` | `warn` or `fail` |  | Rows of a query whose key isn't in the driver: `warn` (default) or `fail`. |
| `max_population_rows` | integer |  | The most rows across the output's queries before anything renders. Default: 5,000,000. |
| `keep_results` | boolean |  | Keep the run's query results in its run folder, for `--retry-failed`. Default: true. |
| `mask_row_values` | boolean |  | Mask destination values rendered from `each.*` (addresses, paths) in the console, logs and results. Default: the project's `mask_row_values`, true. |

## `sets[]`

A Set declared in the report: a named variant of it.

| Key | Type | Default | Description |
|---|---|---|---|
| `name` (required) | string |  | The Set's name. A report can also name Sets declared in `sets.yml`. |
| `profile` | string |  | The connection this Set runs on. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`. |
| `vars` | map |  | Variables, read in SQL and YAML with `var('name')`. Values can be strings, numbers, booleans, lists or maps. |
| `exclude` | list of string |  | Queries to leave out of this Set, by name. |
| `queries` | list of string or map (see below) |  | Replaces the report's queries for this Set. |
| `tab_names` | map |  | Renames tabs for this Set: query name to tab name. |
| `output` | map (see below) or list of map (see below) |  | Output settings for this Set: a map changes the inherited output (with several, the one its `name:` names); a list replaces them all. |
| `locale` | string |  | The locale for this Set's number filters (`fr-FR`), above the report's. |

## `sets[].queries[]`

A query with settings.

| Key | Type | Default | Description |
|---|---|---|---|
| `query` (required) | string |  | The query's name. |
| `tab` | boolean |  | Whether the query makes a tab. |
| `tab_name` | string |  | The tab name. |
| `on_empty` | `continue` or `skip` or `fail` |  | When this query returns no rows: keep its empty tab, leave it out, or fail the run. |
| `secret_columns` | list of string |  | Columns whose values are masked in DRE's logs, results and previews, added to the query's own. |
| _other keys_ | | | Options of the plugin that handles this block; see [Plugins](plugins.md). |

## `var_schema.<name>`



| Key | Type | Default | Description |
|---|---|---|---|
| `type` (required) | `string` or `integer` or `number` or `boolean` or `date` or `array` or `object` or `enum` |  | The value's type. Dates are real calendar dates in YYYY-MM-DD form. |
| `description` | string |  | What this input means, including its units or reporting period. |
| `required` | boolean | `false` | Whether execution requires a value. A default satisfies this requirement. |
| `default` | any |  | Used when no project, folder, report, Set, schedule or CLI value supplies this variable. |
| `allowed` | list of any |  | Optional exact values accepted for this variable; required for the enum type. |
| `pattern` | string |  | For `string` variables: a regular expression the whole value must match, e.g. `^[A-Z]{3}[0-9]{4}$` for a customer code. Anchors are implied. |
| `max_length` | integer |  | For `string` variables: the most characters a value may have. |

## `plugins[]`

A package with its source.

| Key | Type | Default | Description |
|---|---|---|---|
| `name` (required) | string |  | The package name: lowercase letters, digits and `_`. |
| `version` | string |  | A version constraint such as `1.2.0` or `>=1.0`. Not allowed with `local`. |
| `github` | string |  | Install from the releases of this GitHub repository, `owner/repo`. |
| `local` | string |  | Use the package folder at this path as it is. |
| `registry` | string |  | Install from this registry index (a URL or a path) instead of the default one. |

<!-- docs-nav: generated by .github/scripts/docs_sections.py from docs/sections.json -->

---

**Previous:** [dre_project.yml reference](reference-project.md) · **Next:** [sets.yml reference](reference-sets.md)
