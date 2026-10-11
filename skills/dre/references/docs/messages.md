---
title: "Messages"
description: "Headline numbers to Slack, Teams, Google Chat and email, built from a report's own queries."
section: build-reports
position: 4
---

# Messages

Many people who depend on a report never open the file: they want yesterday's revenue, how
many payments failed, whether the month closed. A `message` output turns a report's query
results into a short headline, a title plus a few lines of text, and posts it to Slack, Microsoft
Teams, Google Chat or an inbox. It needs no plugin of its own: the `message` format is built
into DRE. *New in 0.3.*

A message is written to the target path as a `.md` file too, so any file destination (a
Databricks Volume, S3, SFTP, a local folder) delivers it unchanged and keeps a history of what
was sent.

## A daily headline

Aggregate in SQL, and let the message read the result:

```sql
-- reports/finance/daily/headline.sql
select
  sum(amount) filter (where day = date '{{ run.date.prev_day.iso }}') as revenue,
  sum(amount) filter (where day = date '{{ run.date.prev_day.iso }}') /
    nullif(sum(amount) filter (where day = date '{{ run.date.add(days=-8).iso }}'), 0) - 1 as change
from {{ source('sales', 'orders') }}
```

```yaml
# reports/finance/daily/daily.yml
queries: [headline]
output:
  format: message
  title: "Revenue {{ run.date.prev_day.iso }}"
  text: |
    Revenue yesterday: **{{ results.headline.value | currency('EUR') }}**
    ({{ results.headline.first.change | percent | signed }} on the same day last week)
  destination:
    - {profile: team_slack, channel: "#finance"}
```

In Slack this arrives as:

> **Revenue 2026-10-03**
> Revenue yesterday: **€12,340**
> (+4.1% on the same day last week)

`format: message` alone, with no `text:`, already works: the default template writes one block
per query (a single value as `revenue: 12,340`, one row as `column: value` lines, several rows as
a list of up to ten with `+ N more`).

## What a message template reads

For each query of the output, `results.<query>` has:

| | |
|---|---|
| `value` | the first column of the first row |
| `first.<column>` | a column of the first row |
| `rows` | the rows, each `row.<column>` or `row[0]` (at most `max_rows`, default 1,000) |
| `row_count` | the true number of rows, even past `max_rows` |
| `columns` | the column names |
| `sets[n]` | a result set by index (one `.sql` file makes one, so `sets[-1]` is the same as the query) |

Values keep their types: numbers stay numbers, dates are DRE dates, nulls are `none`. The number
filters make them readable: `number`, `percent`, `signed`, `currency` and `compact`, in the
project's [`locale:`](templates.md#locale). `var()`, `run.*` and your macros work as everywhere.

The text is a small Markdown subset that every service can show: `**bold**`, `*italic*`,
`[links](https://...)`, `` `code` `` and `- ` bullets. No headings or tables. Each destination
translates it, and every value the template prints is escaped, so a customer called
`acme_corp*` can't break the formatting. Keep a longer template in a file with `file:
messages/daily.md` instead of `text:`.

## A headline plus the file

`output:` can be a list. The queries run once, every output is made from the same results, and
file outputs are delivered before messages, so a message can link to what was delivered:

```yaml
queries: [headline, detail]
output:
  - name: workbook
    format: xlsx
    queries: [detail]                 # the headline query isn't a tab in the workbook
    destination:
      - {profile: reports_s3, path: "s3://reports/daily-{{ run.date.yyyymmdd }}.xlsx"}
  - name: headline
    format: message
    queries: [headline]               # the message doesn't read the 10k-row detail
    text: |
      Revenue yesterday: **{{ results.headline.value | currency('EUR') }}**
      Full report: {{ outputs.workbook.location }}
    destination:
      - {profile: team_slack, channel: "#finance"}
      - {profile: finance_mail, to: cfo@example.com, attach: [workbook]}
```

`outputs.<name>` has `location` (where it was delivered), `files` and `status`. `attach:` sends
another output's files along with the message, to destinations that take both: the email
arrives with the headline in its body and the workbook attached; in Slack the file goes in the
same post. Teams and Google Chat take messages only, so link the file instead.

## Sending only when it matters

Any output can carry `when:`, a condition over the results. When it's false the output is
skipped: nothing is written or sent, the run output says so, and `run_results.json` records it as
`skipped`. It's not a failure.

```yaml
queries: [failed_payments]
output:
  format: message
  when: "results.failed_payments.row_count > 0"
  title: "Failed payments {{ run.date.iso }}"
  text: |
    **{{ results.failed_payments.row_count }}** payments failed:
    {% for p in results.failed_payments.rows[:10] %}- {{ p.customer }}: {{ p.amount | currency('EUR', 2) }}
    {% endfor %}
  destination: {profile: ops_teams}
```

A message whose text renders empty is skipped the same way, so `{% if %}` inside `text:` works as
a condition. `when:` works on file outputs too ("only send the CSV if there are rows").

Each run decides from its own results. DRE keeps nothing between runs: there's no "alert once",
no deduplication and no escalation. A report that should say "revenue dropped" says it every day
the data shows it. Alerting on whether a run succeeded belongs to your orchestrator or the
Platform.

## Before sending

- `dre validate` checks every template compiles, that `results.<x>` names one of the output's
  queries and `outputs.<x>` another output, and that a file output or `attach:` doesn't go to a
  destination that only takes messages.
- `dre run <report> --preview` prints each message (title, text, its length against each
  destination's limit, and whether `when:` passed) and sends nothing. Numbers then come from the
  row sample.
- `dre validate -s <report>` lists every output with its destinations; production targets stand
  out.

A real run logs one line per message sent, and `run_results.json` keeps its full title and text.

## Destinations

| Destination | Takes | Limit | |
|---|---|---|---|
| [`slack`](plugin-slack.md) | messages and files | 4,000 characters | over the limit: cut short, full `.md` attached |
| [`email`](plugin-email.md) | messages and files | | HTML body with a plain-text alternative; subject from the title |
| [`teams`](plugin-teams.md) | messages only | 15,000 characters | a Workflows webhook |
| [`google_chat`](plugin-google_chat.md) | messages only | 4,000 characters | a space webhook |
| any other | the `.md` file | | `s3`, `sftp`, `databricks` Volumes, `local`, ... |

See [the `message` format](plugins.md#the-message-format) for every option, and
[practices](practices.md#rep-9-aggregate-in-sql-keep-messages-short) for advice.

<!-- docs-nav: generated by .github/scripts/docs_sections.py from docs/sections.json -->

---

**Previous:** [Lookups](lookups.md) · **Next:** [DRE practices](practices.md)
