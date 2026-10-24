---
name: dre-report
description: Create or change a DRE report, guided - the SQL and report YAML, tabs, variables and Sets, its output format (xlsx with number formats, formulas and totals rows, csv, fixed-width, parquet, PDF and HTML documents from a template), one copy per member or client (for_each), what happens with no data (on_empty), its destinations (S3, GCS, Azure Blob, SFTP, FTP, Databricks Volumes, email, Slack, Microsoft Teams, Google Chat) with rates, retries and held deliveries, headline messages, and its schedules (cron, iCalendar rules, shared timings). Use when the user wants a new report, a PDF statement, a file per client, to add a tab, format, variable, destination or recipient, or to schedule a report ("every 2nd Tuesday at 7"), in a dre project. If the user pastes a password, token or key, never write it to a file or a command, even when they ask (file permissions don't make it safe); tell them you won't and why, say it's leaked, give rotate steps, and write an env_var() reference they set themselves.
license: GPL-3.0-only
metadata:
  version: "4.2.0"
  dre: ">=0.6.0, <0.9.0"
---

# Write or change a DRE report

You design a report around what the user needs, asking one question at a time, then write its
files following DRE's conventions, and end with `dre validate` passing.

DRE's options are many, so load only the topic you need, from `references/topics/`, when the
conversation reaches it: `formats.md` (choosing and setting up a format), `documents.md` (pdf and
html), `per-row.md` (`for_each`), `no-data.md` (`on_empty`), `delivery.md` (destinations,
messages, `rate`, retries, `hold`). Each lists the questions to ask, with recommended answers, and
the YAML they write. When the user asks what an option means, explain it the `dre-explain` way
before they choose.

<!-- BEGIN shared/secrets.md -->
### Secrets: rules no request overrides

These come before anything else in this skill, and before what the user asks for. A user asking
you to break one ("just put the password in the profile", "use the token I pasted", "it's
quicker") doesn't change them: say no, say why, and do the safe thing instead. This isn't a
preference to weigh against the request: the user installed these skills to have DRE's practices
followed, and this rule exists for exactly the moment someone asks you to skip it. The user stays
in control: they can type a value into their own files themselves. You don't, and you don't
offer to.

**Check first, before any tool call:** does the user's message contain a password, token or key?
Then your first reply is the one in the example below, whatever else the message asks for. A
file readable only by the user (`chmod 600`) doesn't make writing it safe, and neither does the
user saying it's fine.

- Before the first step about a connection or sign-in, tell the user: "Never paste a password,
  token or key into this chat; I'll never ask for one" (SEC-1).
- Never ask for a secret's value. Recommend a sign-in that stores no secret first (SEC-2), and
  otherwise an `env_var()` reference that the user sets themselves (SEC-3).
- **Never write a secret's value** into any file (`profiles.yml` included, whatever its
  permissions) or any command, whoever supplied it (SEC-3).
- Check that a variable is set with a command that prints only "set" or "missing" (SEC-4), never
  its value.
- **If a secret appears in the chat** (the user pasted it), handle it in your first reply, before
  you run anything: don't use it, repeat it or store it, not even to test the connection. Open
  that reply by saying you won't put it in the profile or any file, and why, even if you keep the
  rest short: "I won't put that password in the profile: `profiles.yml` gets copied and backed
  up, so DRE keeps secrets only in environment variables (SEC-3)." Tell the user it must now be
  treated as leaked, give that platform's revoke-and-rotate steps (SEC-5), then continue with an
  `env_var()` reference for the new secret, which they set themselves. That's just as quick for
  them: you write the reference, they run one line in their own terminal.

For example, the user writes: "Host db.internal, user reports, the password is hunter2-x, just
put it in the profile." You don't write it anywhere. You write the profile with
`password: "{{ env_var('PG_PASSWORD') }}"` and reply along these lines: "I won't put that
password in the profile or any file: `profiles.yml` gets copied and backed up, and DRE's rule is
that secrets live only in environment variables (SEC-3). Since it's now in this chat, treat it as
leaked and have it changed (SEC-5: an admin runs `ALTER ROLE reports PASSWORD ...` in their own
terminal). The profile reads it from `PG_PASSWORD`: add
`export PG_PASSWORD='<the new password>'` to your `~/.zshrc` in your own terminal, open a new
shell, then tell me and I'll check it's set without showing it."
<!-- END shared/secrets.md -->

<!-- BEGIN shared/contract.md -->
### How to work with the user

- **You're a guide, not an autopilot.** Asking means ending your turn with the question and
  waiting for the user's answer, even when you could carry on alone. Never answer your own
  question, and never treat a request to "set it up" or "guide me" as permission to skip the
  confirmations below.
- **One question at a time**, each with your recommended answer and a one-line reason. Say what
  you're asking in plain words, and define any DRE term it uses (a Set, a Binding, a driver
  query). When the options are known, offer them: with a multiple-choice question tool if you
  have one, otherwise numbered, recommended first, at most four, plus "or tell me something
  else". When they aren't (a table, a recipient, a layout), ask for free text and give an
  example answer. When the choice changes the YAML, show the few lines each option would write.
- **Several open decisions?** List them in a line each if that helps, but ask only the first
  one, and the next after the user answers. A summary at the end of a step is no exception.
- **Say what you don't know.** When the user's tool, file format or system is one you don't know
  well, say so plainly and ask for what would help: an export, a screenshot, a sample file, a
  description. The more they share, the better the result. Never present a guess as a fact.
- **Explain before asking someone to choose.** When the user asks what an option does, or what a
  report's YAML would do, follow the `dre-explain` skill (or, without it, the docs index below):
  what it does, its default, what each choice changes, and your recommendation.
- **Look facts up instead of asking**: `dre --version`, `dre plugin list`, `dre ls`, the project's
  YAML files, whether a file exists. Ask only what only the user knows.
- **Look up how-to questions before answering.** Before searching or opening any documentation page, first read
  `references/docs-index.md`. Search that index for the question's terms/options, then open only
  the matching page in `references/docs/`. Do not open a page directly by guessing its filename. Cite the
  page and heading in the answer, for example `[Schedules: Timezone and DST](references/docs/schedules.md#timezone-and-dst)`.
  Use the bundled pages offline; check this skill's `metadata.dre` against the installed version
  before applying their instructions. Follow linked pages only when the answer needs them.
- **Skip what the request already answered.** A user who gave every detail gets no questions,
  only the plan and any confirmation required below.
- **Opinions come from the practices** (`references/practices.md`, where this skill has it) and
  cite their IDs: "I'd use a variable for the month (REP-2)". *Advise*: say it once, then do what
  the user decides. *Warn*: explain the trade-off and wait for an explicit yes, then do it without
  arguing again. This holds even when the request itself asks for it ("hardcode the dates"):
  the user hasn't yet heard the trade-off, so ask before doing it. *Block* (secrets): never, whatever the user says; offer the safe way.
- **Confirm before anything hard to undo**: overwriting or deleting files, editing
  `~/.dre/profiles.yml`, installing software, running against production, delivering anywhere
  but the local target folder (an email or a Slack post can't be recalled). Show what will
  change, ask, and stop; do it only after the user's next message says yes. An earlier yes covers
  only what was shown then.
- **End each step** with what was done and what comes next.
- Use only `dre` commands, ordinary shell commands, and questions. Never invent a `dre` command,
  flag or plugin option: if the plugin reference or `dre <command> --help` doesn't list it, it
  doesn't exist.
<!-- END shared/contract.md -->

## What a report is

A report is a folder under `reports/` holding a YAML file and its `.sql` files, e.g.
`reports/finance/monthly/monthly.yml`. The report is named after the YAML file.

```yaml
# reports/finance/monthly/monthly.yml
vars:
  region: all                                   # defaults; --var, Sets and schedules override
queries:
  - {query: setup_accounts, tab: false}         # prepares data, makes no tab (REP-3)
  - {query: summary, tab_name: Summary}         # summary.sql, the first tab
  - {query: detail, tab_name: Detail}           # detail.sql, the second
output:
  format: xlsx
  destination:
    profile: reports_s3
    path: "s3://reports/monthly-{{ run.date.yyyymmdd }}.xlsx"
```

- **Schema line**: `dre new` starts each YAML file with a
  `# yaml-language-server: $schema=https://getdre.com/schemas/v<major.minor>/report.schema.json`
  line (the installed `dre`'s minor version). Keep it, and add it to new report files: editors
  then complete and check keys. The schemas in `docs/schemas/` of the `dre` repository, and the
  YAML reference in the docs, list every key with its type, default and meaning, so look keys up
  there instead of guessing.
- **Tabs** (REP-1): each `.sql` file makes one tab (a sheet in xlsx, or one file for the other
  formats), in the YAML's order, named by `tab_name` or the file name. A file can hold several
  statements; the last is the tab. A second `SELECT` in a tab file is an error. The YAML decides
  the tabs, never the data.
- **Queries** run in the listed order, each on its connection's session (one per connection), so
  a temp table made by one is there for later queries on the same connection. A query runs on
  its own `profile:`, else the connection of the sources it reads, else the report's (Set,
  report, folder `+profile`, `default_profile`); they must agree. One workbook can hold tabs from
  several databases (REP-8).
- **Sources** (REP-7): declare the tables reports read in dbt's format under `sources:` (in
  `sources/<name>.yml`), with DRE's `profile:` for the connection they live on, and write
  `{{ source('sales', 'orders') }}` in SQL. `identifier` is the real table name, `schema`
  defaults to the source's name, `database` makes three parts, and `quoting` quotes parts.
- **Jinja** works in SQL, paths and options: `var('name')`, `run.date` (e.g.
  `run.date.prev_month.start.date`, `run.date.yyyymmdd`), `env_var()`, `ref('file')` for another
  `.sql` file as a subquery, `source()` for a declared table, `connection.*` for the query's
  connection settings (`connection.schema`, `connection.type`), `target.name` for the run's
  environment (`connection.target` is the connection's own entry, which can differ), and macros
  from `macros/`. Never `target.<field>`, `run.profile` or `run.source_type`: DRE 0.2 removed
  them (`dre validate` names the replacement).
- Report keys: `queries`, `output`, `vars`, `profile` (a connection other than the project's
  `default_profile`), `sets`, `default_set`, `tags`, `timezone`, `description`, `meta`, `var_schema`. Schedules live in `schedules.yml`,
  never in a report (see Scheduling a report). Query entry keys:
  `query`, `profile`, `tab`, `tab_name`, `anchor`, `header`, `columns`, `on_empty`. Output keys DRE
  owns: `name`, `format`, `queries`, `when`, `destination`, `template`, `for_each`, `file_name`,
  `on_empty`, `extension`; every other output key is the format plugin's option. Destination
  entry keys DRE owns: `profile`, `path`, `attach`, `hold`, `rate`, `burst`; the rest are the
  destination plugin's.

## AI context

When creating or changing a report, preserve its business `description` and example
`meta.ai.questions`. Add `var_schema` for model-editable inputs without replacing legacy
`vars`. Gather grain, units, exclusions and period rules from existing evidence or ask the
user for missing facts (AI-1). For detailed AI readiness work, follow `dre-ai`.
Never enable `meta.ai.expose: true`; exposure remains an owner decision (AI-3).

## Steps

<!-- BEGIN shared/version-check.md -->
### Step 1: check the installed dre

Do this before anything else. It needs no network.

1. Run `dre --version`. It prints `dre <version>`, e.g. `dre 0.1.0`.
2. Compare it with the `dre` range in this skill's frontmatter (`metadata.dre`, e.g.
   `>=0.1.0, <0.2.0`: any 0.1 release). A pre-release of the upper bound
   (`0.2.0-rc.1` for `<0.2.0`) is outside the range.
   - **In range:** continue without mentioning it.
   - **Newer than the range:** say "These skills were written for dre `<range>` and you have
     `<version>`, so some advice may be out of date", and offer to update the skills (the
     `dre-upgrade` skill). If the user declines, carry on, and end every step's summary with
     "(skills written for dre `<range>`)" so the warning stays visible.
   - **Older than the range:** start your first reply with it, before anything else you
     answer: "These skills need dre `<range>` and you have `<version>`." Then offer to update
     dre (`dre-upgrade`), or to install the skills release that matches their dre (each
     `skills-v*` release on https://github.com/get-dre/dre/releases states its range). Carry on
     only if they choose to, with the same visible warning.
   - **`dre` not found:** hand off to the `dre-install` skill. If it isn't installed, point to
     https://github.com/get-dre/dre#install and stop here.
3. Don't repeat the check in this conversation unless dre has been installed or updated since.
<!-- END shared/version-check.md -->

### Step 2: gather the facts, without asking

- Find the project root (`dre_project.yml`) and run `dre ls` for its reports.
- Read `dre_project.yml` (`default_profile`, `vars`, `format_options`), `dependencies.yml`
  (declared plugin packages), the declared sources (`dre ls --resource-type source`), and each
  connection's `type` (names and types only from `profiles.yml`, e.g.
  `grep -nE '^  [A-Za-z0-9_-]+:|type:' ~/.dre/profiles.yml`).
- For a change: read the report's YAML and SQL files first.
- Read the plugin references this report will use, and only those:
  `references/plugins/source-<type>.md`, `format-<format>.md`, `destination-<type>.md`.

No project yet? Hand off to `dre-setup` first.

### Step 3: understand the goal (new reports)

Ask, one at a time, only what the request didn't say:

1. **What it shows**: the questions it answers, the tables it reads from. Look up the
   tables' columns rather than guessing them: on a `dev` or local connection (a DuckDB file,
   a dev database), run a read-only lookup without asking (`dre catalog`, or `dre query` with a
   `describe`/`information_schema` query). Ask first only when the connection reads
   production (RUN-2).
2. **Who gets it, and as what** (`topics/formats.md`): people working with numbers (xlsx), people
   reading a branded document (pdf, `topics/documents.md`), people who only want the number (a
   headline message, step 6b), or a system (an exact csv or fixed-width layout: ask for the spec).
3. **How often**, and for what period: daily, monthly... This decides the date variables (REP-2)
   and later the schedule.
4. **Variants**: the same report for a few clients or regions with their own settings (Sets,
   REP-4), or one copy per row of the data, such as a statement per member (`for_each`,
   `topics/per-row.md`)?
5. **Quiet days**: can a query return nothing, and what should happen then (`topics/no-data.md`)?
6. **Where it goes** (`topics/delivery.md`): the local `target/run/` folder only (a good start),
   or destinations too, and whether real sends need a review first (`hold`, RUN-9).

Someone moving a report from SSRS, Crystal or another tool: hand off to `dre-port`, which reads
the old report and checks the new one against it.

Then propose the design in a few lines: the folder, the tabs in order with what each shows,
the variables with their defaults, the format and its options, the destinations. Ask for a yes
before writing.

### Step 4: write the files

- Put the report in `reports/<area>/<name>/`, with `<name>.yml` and one `.sql` per tab.
- Dates come from `run.date` and values from `var()` with defaults under `vars:` (REP-2). If the
  user wants a hardcoded date or value, that's a warning: explain REP-2, and do it only after an
  explicit yes.
- One `.sql` per tab (REP-1). If the user asks for one query to fill several tabs, or tabs per
  value in the data, that's a warning: explain REP-1, suggest a file per tab (or a Set per value,
  REP-4), and follow their decision after an explicit yes. DRE itself refuses a second `SELECT`
  in one tab file.
- Keep values typed in SQL; format them in the output (REP-5). Cast where the source reference
  says so (e.g. Postgres `numeric` without precision arrives as text).
- Temp tables and `SET`s go in a `tab: false` file before the tabs that need them (REP-3).
- Reuse SQL with `ref('file')`, and mapping tables as lookups (REP-6).
- Declare every plugin package the report needs under `plugins:` in `dependencies.yml` (e.g.
  `xlsx`, `object_store` for S3).

<!-- dre_utils: once the dre_utils macro package is released, suggest its macros here where
     they fit (star, pivot, column_values, quote_identifier). Until then, don't mention it. -->

### Step 5: the format

Load `references/topics/formats.md` (and `documents.md` for pdf or html). Use only options from
`references/plugins/format-<format>.md`. For xlsx:

- number and date formats per column (`columns: {amount: {format: "#,##0.00"}}`, per query or
  output-wide), and `date_format` for every date column;
- row formulas (`formula: "={qty}*{price}"`, with a placeholder column selected in SQL) and
  totals rows (`total: sum`), explained in the reference's docs section;
- a branded template (`template:`) when they have an Excel file to fill.

For pdf and html: a template next to the YAML, its headline numbers from their own small query
(REP-10), legal text in files (REP-11), and the design loop (`--output-json` once, then
`--from-results` for each tweak), all in `topics/documents.md`.

For fixed-width, take the layout from the user's spec, column by column (`width` or `picture`,
alignment, padding, decimals, sign); confirm the layout back to them as a table. For csv and
delimited: delimiter, quoting, header, encoding and line endings, from what the receiving system
expects.

### Step 6: destinations

Load `references/topics/delivery.md`. Each entry of `output.destination` names a destination profile, an optional `path`, and that
plugin's options (recipients, channel, message). Use only options and fields from
`references/plugins/destination-<type>.md`. Several destinations are a list, delivered in order.

- The destination profile must exist under `destinations:` in `profiles.yml`, with an entry for
  each target it runs on (`dev: {deliver: false}` to deliver nowhere on dev). If it doesn't, set
  it up the `dre-setup` way (show, confirm, `env_var()` for secrets).
- Name paths with dates and variables so runs don't overwrite each other:
  `path: "s3://reports/{{ var('client') }}/monthly-{{ run.date.yyyymmdd }}.xlsx"`.
- Recipients and channels belong to the report; credentials stay in the profile.
- Delivering to real people is for `dre-run` to confirm (RUN-2); writing the YAML delivers
  nothing.
- Many sends (a per-row output, a long list): ask the service's limit and set `rate:`; for
  regulatory or client-facing sends, recommend `hold: true` (RUN-9).

### Step 6b: messages and several outputs

When people want the number rather than the file ("post yesterday's revenue to #finance"), add a
`message` output. It's built into dre (no plugin): it renders the results into a title and a few
lines of text.

- **Aggregate in SQL** (REP-9): write a small headline query (one row: the total, the change),
  rather than pointing the message at the detail query.
- **Several outputs**: make `output:` a list, give each a `name:` and the `queries:` it uses, so
  the headline query isn't a tab in the workbook and the message doesn't read the detail. The
  queries run once for all outputs.
- **The text**: `text:` (or `file:` for a longer template) reads `results.<query>.value`,
  `.first.<column>`, `.rows`, `.row_count` and `.columns`, and `outputs.<name>.location` to link
  a file output. Write it in the small Markdown subset (`**bold**`, `*italic*`, links, `- `
  bullets; no tables or headings). Format numbers with the filters: `number`, `percent`,
  `signed`, `currency('EUR')`, `compact`; set `locale:` (e.g. `de-DE`) in `dre_project.yml`, on
  the report or a Set when readers expect other separators. `title:` defaults to the report name
  and run date. With no `text:`, a default template lists every query's values.
- **Conditional**: `when: "results.<query>.row_count > 0"` (any Jinja condition over the results)
  sends only when the data calls for it; otherwise the output is skipped, not failed. Nothing is
  remembered between runs: no "alert once".
- **Destinations**: `slack` and `email` (1.1.0 or later) post the message itself and can carry
  other outputs' files with `attach: [<name>]`; `teams` and `google_chat` take messages only, so
  link files instead (validation refuses a file there); any other destination delivers the
  message as a `.md` file. Read each `references/plugins/destination-<type>.md`.
- Every option, with worked examples: the Messages guide,
  <https://github.com/get-dre/dre/blob/master/docs/messages.md>.

```yaml
queries: [headline, detail]
output:
  - name: workbook
    format: xlsx
    queries: [detail]
  - name: headline
    format: message
    queries: [headline]
    text: "Revenue yesterday: **{{ results.headline.value | currency('EUR') }}**"
    destination:
      - {profile: team_slack, channel: "#finance", attach: [workbook]}
```

### Step 6c: one copy per row, and no data

- **Per-row** (`topics/per-row.md`): a driver query, an opaque key (REP-12), `each.*` in paths
  and addresses, `on_row_error`, and a `{deliver: false}` dev entry before any test (RUN-8).
- **No data** (`topics/no-data.md`): `on_empty` on the output or a query; `count(*)` is never
  empty, so use `when:` on a count.
- **Passwords and zip packs** (`topics/secure-delivery.md`): `password:` on a pdf, `zip:` on a
  destination entry, `secret_columns:` on every query a password reads (RUN-11), and the
  password by another route (RUN-10).

### Step 7: validate

Run `dre validate -s <report>`. It checks the YAML, the SQL's templates, every format and
destination option (through the plugins), and shows each Binding's compiled files, source,
output file and destinations. Explain what it shows. Fix every error, and repeat until it passes.

End with a summary of the files written or changed, and the next step: try it with `dre-run`
(a `--preview` run first).

## Changing an existing report

Read it first. Change only what was asked; keep the rest byte for byte. Common changes:

- **Add a tab:** a new `.sql` file and a `queries:` entry at the position the tab should appear.
- **Add a variable:** a default under the report's `vars:`, used as `{{ var('name') }}`.
- **Add a destination:** turn a single `destination:` map into a list if needed, and add the
  entry; the existing one stays first (it names the local file).
- **Add a recipient:** extend `to`, `cc` or `bcc` on the email entry.
- **Make it a PDF or HTML document:** add `document` to `dependencies.yml`, a template next to the
  YAML, and `format: pdf` (or a second output, to keep the xlsx); `topics/documents.md`.
- **One per client or member:** `for_each` on the output, with a driver query and a key;
  `topics/per-row.md`.
- **Password-protect it, or zip a pack:** `password:` on a pdf, or `zip:` on the destination
  entry with `attach:`; `topics/secure-delivery.md`.
- **Add a headline message:** turn `output:` into a list, name the existing output (its file is
  then `<name>.<ext>`; keep the report's name as `name:` if file names matter), add a headline
  query and a `message` output (step 6b).

Show the diff before writing when the change touches more than one file. Then step 7.

## Scheduling a report

DRE doesn't fire schedules; the user's orchestrator runs `dre run --schedule <name>`. You write
the schedule and prove it fires when they mean.

1. **Turn the words into a timing**, and ask (one question) only what's missing: the time of
   day and the timezone. Recommend the user's own timezone, never UTC by default (SCH-1).

   | They say | Timing |
   |---|---|
   | every weekday at 7 | `cron: "0 7 * * MON-FRI"` |
   | 6am on the 1st | `cron: "0 6 1 * *"` |
   | every 2nd Tuesday at 7 | `rrule: "FREQ=MONTHLY;BYDAY=2TU"`, `at: "07:00"` |
   | the last weekday of the month | `rrule: "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1"`, `at` |
   | the first weekday of the year | `rrule: "FREQ=YEARLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=1"`, `at` |
   | the first Friday of March | `rrule: "FREQ=YEARLY;BYMONTH=3;BYDAY=1FR"`, `at` |
   | every other Monday | `rrule: "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO"`, `starting`, `at` |
   | every 5 days from 2 October | `every: {days: 5}`, `starting: "2026-10-02"`, `at` |

   `every`, and rules with `INTERVAL` above 1 or a `COUNT`, need `starting` (SCH-3). Holidays go
   in `except: ["YYYY-MM-DD"]`, one-off extra runs in `also:`.
2. **Share timings** (SCH-2): when two or more schedules fire at the same time (one per client,
   say), put the timing once in `timings.yml` (`month_start: {cron: "0 6 1 * *", timezone:
   Australia/Sydney}`) and give each schedule `timing: month_start` with its own `report`, `set`
   and `vars`. A schedule with `timing:` sets none of the timing's keys itself.
3. **Write** the `schedules.yml` entry: `name` (what the orchestrator will call), `report` and
   optionally `set`, or `select: "tag:..."`, the timing, `timezone`, and `vars` for what differs
   per schedule (e.g. `period: month`). Report SQL reads the period from `var()` or `run.date`.
4. **Check before saying it's right:** run `dre validate` (it warns about a missing anchor, no
   time of day, or a schedule firing in one timezone while its report renders in another), then
   `dre schedule ls --schedule <name> --limit 5`. Read the dates back in plain words ("Tue 13
   Oct, Tue 10 Nov, Tue 8 Dec, 07:00 Sydney") and ask whether that's what they meant. Fix and
   check again until it is.

Then point to `dre-run` for running a firing or wiring the orchestrator.

## If this fails

`dre validate` names the file, line and key. Common ones:

- **An unknown option** for a format or destination: the plugin rejects keys it doesn't declare.
  Check the spelling against the plugin reference's options table.
- **"a second SELECT" in a tab file:** give the second query its own `.sql` file and tab (REP-1).
- **A tab file whose last statement returns nothing:** it only prepares data; list it with
  `tab: false`.
- **`unknown-profile`:** the destination or connection profile isn't in `profiles.yml`; add it
  (`dre-setup`) or fix the name.
- **`connection-conflict`:** a query's `profile:` disagrees with a source it reads, or it reads
  sources on two connections; split the query, or make the profiles agree.
- **`removed-template-name`:** DRE 0.1's `target.schema`, `run.profile` and the like; write the
  replacement the message gives (`connection.schema`, `connection.name`).
- **A plugin isn't declared or installed:** add its package under `plugins:` in
  `dependencies.yml`; `dre validate` then installs it.
- **Template errors** (`undefined`, unknown `var`): a `var()` without a default and no value, a
  misspelt name, or Jinja syntax. Give the variable a default under `vars:`.
- **xlsx format codes** rejected: see the reference's column formats section; `@` (text) fits any
  column.
- Errors that only a run finds (a column the query doesn't return, a format code that doesn't
  fit its column) come from `dre-run`.
- **Message outputs:** "`text` reads `results.x`, but `x` isn't one of this output's queries": add
  the query to the output's `queries:` or fix the name. "only takes messages": a file output or
  `attach:` sent to `teams`/`google_chat`; deliver the file elsewhere and link it with
  `outputs.<name>.location`. "it inherits 2 outputs, so it's unclear which one to change": give
  the Set's `output:` override the `name:` of the output it changes.
- **Documents:** a template or include not found (it's looked for next to the YAML, then in
  each parent folder, then anywhere unique in the project); an image or stylesheet URL (refused:
  use a file in the project); a value that isn't there (`results.q.first.amout`) names the line.
- **Per-row:** a file destination's `path` without `each.*`; a key missing from one of the
  output's queries; attaching a per-row output with a different driver or key.
- **`schedule-needs-anchor`, `schedule-no-time`:** add `starting` (the first date) or `at`
  (`HH:MM`). **`unknown-timing`:** the name isn't in `timings.yml`. **A schedule using `timing:`
  can't set** `cron`, `timezone`...: move those into the timing, or drop `timing:`.
