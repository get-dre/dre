---
title: "Per-row outputs"
description: "One statement, file or email per member, client or account from one run: for_each, each.*, failures, retries, masking and held deliveries."
section: build-reports
position: 5
---

# Per-row outputs

A quarterly statement for each of 10,000 members, a monthly report for each client account, an
extract to each client's SFTP folder: `for_each` makes one copy of an output per row of a
**driver** query. The report's queries run **once** for the whole population, so 10,000
statements cost one warehouse query and all come from the same data; DRE then splits each
query's result by a key, and each copy gets only its own slice. (Other tools call this
"bursting".) *New in 0.6.*

```yaml
# reports/members/statements/statements.yml
queries: [members, holdings, transactions]
output:
  - name: statement
    format: pdf                       # any format: pdf, xlsx, csv...
    template: {file: statement.html}
    for_each: {query: members, key: member_id}
    destination:
      - profile: statements_s3
        path: "s3://statements/{{ run.date.yyyymm }}/{{ each.member_id }}.pdf"
  - name: email
    format: message
    for_each: {query: members, key: member_id}
    title: "Your statement, {{ each.first_name }}"
    text: "Your statement for {{ run.date.month_name }} is attached."
    destination:
      - {profile: member_mail, to: "{{ each.email }}", attach: statement, rate: 120/minute}
  - name: run_summary                 # one file for the whole run
    format: xlsx
    queries: [holdings]
```

- **The driver** (`for_each.query`) is a query of the report: one row per copy. It isn't a tab
  or section of the per-row output itself; other outputs may use it.
- **The key** (`for_each.key`) is a column, or a list of columns, present in the driver and in
  every query of the output. Each copy gets the rows with its key. **Use an opaque key**, such as
  a member number, never an email or a name: keys name files and appear in logs, unmasked.
  `dre validate` warns about a key that looks like personal data.
- **`each.<column>`** is the driver row, in every template of the output: a document template,
  a message's `title` and `text`, and a destination's `path`, `to`, `subject` and other options.
  It isn't available in SQL or through `var()`.
- **`results.<query>`** inside a per-row output holds only that row's slice, so templates written
  for one output work unchanged, and a member never sees another's data.

`for_each` works with every format and destination, and inherits like other output keys: a Set
can turn it on, or off with `for_each: false`. Each Set's Binding fans out on its own.

## Files

Each row's file is `<output>/<key>.<ext>` in the run folder (`statement/12345.pdf`; a
composite key is joined with `_`), or what `file_name:` renders (`"{{ each.member_id }}_q3.pdf"`).
Key values are made safe for file names. Before anything renders, the run fails when two rows
have the same key, or would write the same file name, so rows never overwrite each other. A
destination with a `path` must use `each.*` in it, or `dre validate` fails; pathless
destinations (email, Slack) are exempt.

## When a row fails

| `for_each.on_row_error` | A row fails to render or deliver |
|---|---|
| `continue` (default) | It's recorded with its key, code and message; the other rows carry on; the run ends `failed` (exit 1) |
| `fail` | Every row renders first; if any failed, **nothing** is delivered |

Every row's result is a line of `<output>.rows.jsonl` in the run folder, written as it finishes:
`key`, `status` (`succeeded`, `skipped`, `failed` or `held`), `files` with their SHA-256,
`deliveries` (the destination, its rendered target, status and location) and `error`.
`run_results.json` keeps counts and the failed keys (`output_results[].for_each`), so it stays
readable at 10,000 rows; per-row files aren't listed in its `outputs`. Ctrl-C or a timeout stops
new rows from starting; finished rows stay recorded. See [the manifest page](manifest.md).

## Speed and limits

Rows render and deliver `for_each.concurrency` at a time (default 4; `--row-concurrency` or
`DRE_ROW_CONCURRENCY` override it), each worker reusing its plugin processes from row to row.
This is separate from `--threads`, and opens no extra warehouse connections.
`rate:` on a destination entry paces its deliveries across every worker, to stay under an SMTP
relay's or Slack's limits: `rate: 100/minute`, `rate: 5/second` or `rate: 20000/hour`, spaced
evenly (`60/minute` sends one a second). `burst: 20` lets up to 20 go back to back while keeping
the same average. `dre run --rate` and `dre release --rate` override every destination's rate
for one command. If a service still says to slow down, the delivery is tried again after
`retry_wait` (doubling, up to `max_retry_wait`) or the service's `Retry-After`, up to `retries`
times (see [Tries again](plugins.md#tries-again)).

### Watching a long send

At a terminal, each per-row output (and `dre release`) shows a bar as rows finish, with the
measured speed (over the last 30 seconds), the `rate:` it's capped at, and the time left:

```text
     email [██████████████░░░░░░░░] 250/400 rows · 35/s (rate 100/s) · ~5s left
```

The speed is what the destination actually took: below the rate here, because the mail server
accepted about 35 a second. A row counts once all its deliveries are done; held rows count in
`dre run`, and `dre release` counts each row once all its held deliveries have gone. Without a
terminal (cron, CI, a pipe), a `Progress` line comes at most every 30 seconds instead, so a
three-hour send shows it's alive:

```text
  Progress  output `email`: 1,200/20,000 rows, 0 failed, 98/min (rate 100/min), ~3h11m left
```

A send under 30 seconds prints no extra line. `--output json` writes a `rows_progress` event
(`output`, `done`, `total`, `failed`, `rate`, `rows_per_second`, `seconds_left`) on the same
schedule, and `-q` shows neither.

The population is held in memory to split it. `for_each.max_population_rows` (default 5,000,000
rows across the output's queries) fails the run before anything renders when a query returns
more: filter or aggregate in SQL. A document's own [`max_rows`](plugin-document.md#how-big-a-document-can-be)
applies to each row's document.

## No data and orphan rows

- An empty driver: the output's [`on_empty`](building-reports.md#when-theres-no-data) decides
  (`continue` and `skip` make nothing; `fail` fails the run).
- Each row's slice: query- and output-level `on_empty` apply to it. A member with no
  transactions gets an empty section (`continue`), or no statement (`skip`: the row is
  `skipped`, reason `no_data`), or a failed row (`fail`, then `on_row_error` applies).
- Orphan rows, whose key isn't in the driver, belong to no copy: the run warns with their count
  and up to 10 keys (`orphan_rows` in the results). `for_each.on_orphan_rows: fail` makes it an
  error.

## Messages and attachments

A per-row message output that attaches a per-row output with the same driver and key gets that
row's own file: each member's email carries only their statement. If that row's file failed or
was skipped, its message is `skipped`, reason `attachment_failed`. Attaching an ordinary output
attaches the same file to every row. `dre validate` fails on attaching a per-row output with a
different driver or key.

## Masking

Destination values rendered from `each.*` (addresses, names, paths) are **masked** in the
console, `dre.log`, `run_results.json` and `rows.jsonl`: an email keeps its first character and
its domain (`j***@example.com`), anything else is `*****`. The key is never masked. The output
files themselves are the data and are never masked. `mask_row_values: false` in
`dre_project.yml`, or under `for_each`, shows them as they are.

Passwords and other secrets in the data go further: `secret_columns:` on the driver query's entry
masks those columns' values as `*****` everywhere DRE reports, whatever `mask_row_values` says,
and `--output-json` saves them masked. See [Password-protect PDFs and zip report
packs](password-protected-outputs.md), which also covers `password:` on per-row PDFs and a zip
per row.

## Retrying failed rows

A per-row run keeps its query results in its run folder (`results/`, Arrow IPC, as
[`--from-results`](plugin-document.md#designing-a-template) reads). After fixing the cause of
failed rows:

```bash
dre run statements --retry-failed 20261017T060000Z-k3f9
```

renders and delivers **only** that run's failed and never-started rows, from those saved results,
with no query, so retried members get the same figures as the rest of the batch. The retry is
its own run folder (`retry_of` in its `run_results.json`, shown by `dre history`), doesn't become
the current run, leaves the original's `rows.jsonl` untouched, and skips the report's other
outputs. Retrying again (of either run) never re-sends a row that succeeded. A run that was
pruned (`keep_runs`) can't be retried; with `for_each.keep_results: false` nothing is saved, and
a retry runs the queries again, with a warning.

`--only-keys 101,102` and `--limit-rows 10` make only some rows (recorded as `partial`).

## Previewing

`dre run statements --preview` renders the first 3 rows (or the `--only-keys` ones), prints
each row's files and rendered destinations, masked, and what a real run would make ("would make
10,212 row(s) and 10,212 deliveries"), and sends nothing. `--unmask` shows the values as they
are, on your terminal only.

## Held deliveries and `dre release`

A maker/checker step: `hold: true` on a destination entry (or `dre run --hold` for every
destination) renders and delivers everything else, and records the held deliveries: per row in
`rows.jsonl` (`held`), and in `held.json` in the run folder, with each file's SHA-256. The run
succeeds, and says how many are held.

```yaml
destination:
  - {profile: review_s3, path: "s3://review/{{ run.date.yyyymm }}/{{ each.member_id }}.pdf"}
  - {profile: member_mail, to: "{{ each.email }}", attach: statement, hold: true}
```

After review:

```bash
dre release statements --run 20261017T060000Z-k3f9 --by jane     # every held delivery
dre release statements --run 20261017T060000Z-k3f9 --only-keys 101,102
dre release statements --run 20261017T060000Z-k3f9 --exclude-keys 103
```

`dre release` paces deliveries at each destination's recorded `rate:` (or `--rate 30/minute`),
and delivers from the run folder's files, byte for byte what was reviewed: it never
re-renders or re-queries, and refuses a file whose SHA-256 changed since the run. It takes the
Binding's lock, is its own run folder (`release_of`, `released_by`, and each delivery's result in
`released`), and never sends a delivery twice: releasing again sends only what's still held. It
works for any output, per-row or not (`--binding <set>` for a report with Sets). Keep the run
in the target path for the review window: a shared target path, and `keep_runs`.

## Test before you send

**Use a `{deliver: false}` entry for your dev target**, so a run during development renders every
row and sends nothing:

```yaml
# profiles.yml
destinations:
  member_mail:
    targets:
      dev: {deliver: false}
      prod: {type: email, host: smtp.example.com, from: statements@example.com}
```

<!-- docs-nav: generated by .github/scripts/docs_sections.py from docs/sections.json -->

---

**Previous:** [Messages](messages.md) · **Next:** [Password-protect PDFs and zip report packs](password-protected-outputs.md)
