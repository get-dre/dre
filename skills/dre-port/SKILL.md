---
name: dre-port
description: Move reports from another tool to DRE, guided - SSRS and Power BI paginated (.rdl), Crystal Reports (XML or text exports; not binary .rpt), or SQL plus a screenshot or description. Reads the report, asks about the gaps one question at a time, writes the SQL, report YAML, document template, schedules and destinations, validates, then proves the port with a parity check against an export of the old report (row counts, column totals, a keyed cell diff). For a folder of reports, lists them first with an effort score, groups near-duplicates into Sets and suggests an order. Use when the user wants to port, migrate, convert, move or replace SSRS, Crystal, Power BI paginated or other legacy reports, or check that a DRE report matches the old one. If the user pastes a password, token or key, never write it to a file or a command, even when they ask (file permissions don't make it safe); tell them you won't and why, say it's leaked, give rotate steps, and write an env_var() reference they set themselves.
license: GPL-3.0-only
metadata:
  version: "4.0.0"
  dre: ">=0.6.0, <0.7.0"
---

# Port a report to DRE

You move a report from another tool into DRE: you read it, translate it with the mapping
reference, ask the user only what the files don't say, write the DRE files, and finish with a
parity check that shows the new report matches the old one. DRE has no import command: you do
the translation, and the user decides.

Your references, in this skill's folder:

- `references/mapping.md`: every SSRS, Power BI paginated and Crystal concept with its DRE
  equivalent, expressions and T-SQL included. Read it before translating anything.
- `references/examples/`: worked ports, from the source file to the DRE files.
- `references/parity.md`: getting the old report's export, and reading the parity check.
- `references/inventory.md`: listing a folder of reports, and exporting SSRS subscriptions.
- `references/topics/`: formats, documents (pdf/html), per-row outputs, no data, delivery.
- `scripts/inventory.py` and `scripts/parity.py`: Python 3.8+ standard-library scripts. Run them
  as `python3 <this skill's folder>/scripts/<script> ...`; `--help` prints their options.

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

- The project root (`dre_project.yml`) and `dre ls`; connection names and types only, e.g.
  `grep -nE '^  [A-Za-z0-9_-]+:|type:' ~/.dre/profiles.yml`. No project yet? Port into a new one:
  hand off to `dre-setup` for the connection and the project, then come back to step 3.
- `python3 --version` (the scripts need 3.8 or later). Without Python, do the inventory and the
  parity check by reading the files yourself, and say the result is less certain.
- The files the user pointed at: their extensions, and for `.xml` the root element.

### Step 3: what are we porting?

Work it out from the files; ask only what they don't show:

| The user has | Path |
|---|---|
| a folder of reports, or "all our reports" | step 4, the inventory, then one report at a time |
| an `.rdl` or `.rdlc` (SSRS, Power BI paginated, Report Builder) | step 5 |
| a Crystal XML export of the report definition (`<Report>` root, with `Database` and `DataDefinition`) | step 5 |
| a Crystal "Report Definition" text export (`.txt`) | step 5: read it yourself; it lists tables, formulas, groups and sections |
| a binary Crystal `.rpt` | ask for an export: File > Export > Report Definition (text), or an XML export of the definition, plus a PDF of a run; DRE can't read `.rpt` (it needs SAP's SDK) |
| SQL plus a screenshot, a PDF or a description | step 5, the generic path |
| another tool (Jasper `.jrxml`, Oracle BI Publisher, Cognos, Business Objects, Access, an Excel macro) | say you don't know that tool's files well enough to read them reliably, and take the generic path: ask for the SQL, a screenshot or PDF of its output, its parameters, and who gets it when |

### Step 4: the inventory (a folder of reports)

1. Ask whether they can export the SSRS subscriptions (they're in the report server's catalog
   database, not the `.rdl` files). Recommend yes: they become schedules and destinations. The
   read-only query is in `references/inventory.md`; the user or their DBA runs it and saves a
   csv. Don't run it against their server yourself.
2. Run `python3 <skill>/scripts/inventory.py <folder> [--subscriptions <csv>]`. It prints a table
   (sources, datasets, parameters, groups, subscriptions, an effort S/M/L/XL with the reasons),
   candidate Sets (near-duplicates) and a suggested order.
3. Show the table, then explain in a few lines: which reports are easy, which are hard and why,
   which look like one report with Sets (REP-4), and anything that can't be read (`.rpt`, a data
   export). Say the effort is an estimate from the file, not a promise.
4. Ask which report to port first, recommending the first in the suggested order (PORT-3). Port
   one at a time: finish each with its parity check before the next.

### Step 5: read the report, and say back what you found

1. For an `.rdl` or Crystal XML: `python3 <skill>/scripts/inventory.py <file> --json` gives its
   structure (datasets with their SQL, parameters, groups, expressions, formulas, page header
   and footer). Read the file itself for the layout: the data regions, which columns show, the
   textbox expressions, sorting, page breaks.
2. For the generic path: read the SQL, look at the screenshot or PDF, and ask for what's missing,
   one question at a time.
3. Translate each part with `references/mapping.md`, and find the closest worked example in
   `references/examples/`.
4. Tell the user what you found and how you'd port it, as a short table: each part of the old
   report, its DRE equivalent, and anything with no direct equivalent (interactive sort,
   drill-through, a chart, custom code) with your suggestion. Then ask about the first gap.

### Step 6: where the data lives

DRE reads DuckDB, Postgres, Databricks, Snowflake, BigQuery and ClickHouse. It has **no SQL
Server source**, and most SSRS reports read SQL Server. Ask where the same tables live in one of
those, recommending the project's existing connection when it has one.

- **The tables are in a supported warehouse** (replicated, or the warehouse is the source of
  truth): translate the SQL to its dialect (`references/mapping.md`, "T-SQL to the warehouse").
  Table and column names may differ there; offer to check them with `dre catalog` or a query the
  user approves, rather than guessing.
- **The data is only in SQL Server:** say plainly that DRE can't read it yet, so this report
  can't be ported until the data is in a supported source. Stop here for this report and offer
  the next one.

### Step 7: ask the gaps, one at a time

Ask only what the report and the user haven't answered, each with your recommendation (load the
topic in `references/topics/` first):

1. **The format:** recommend xlsx for a tabular report people work in, pdf for a paginated
   layout people read (an invoice, a statement, a letter), csv for a file a system picks up
   (`topics/formats.md`, `topics/documents.md`).
2. **The parameters:** each becomes a variable with a `var_schema` (type, default, allowed
   values). A default that was "last month" or "today" comes from `run.date` (REP-2).
3. **Subtotals and groups:** in SQL (`ROLLUP`) with xlsx formats, or a pdf template (mapping).
4. **No data:** what the old report did on an empty result (`topics/no-data.md`).
5. **Delivery:** each subscription becomes a schedule plus a destination; a data-driven one
   becomes `for_each` (`topics/per-row.md`, `topics/delivery.md`). The schedule's timezone: the
   old report server's, unless the user says otherwise (SCH-1). Recipients and channels go in the
   report; credentials only through `env_var()` in `profiles.yml`.
6. **The first deliveries** (PORT-4): recommend `{deliver: false}` on `dev` and `hold: true` on
   the real destinations until the owner signs off, so nobody gets a wrong copy or two copies.

### Step 8: write the files

Show the plan first (the folder, each file, the variables, the outputs, the schedule), and write
only after a yes:

- Every variable gets a default under `vars:` (from `run.date` for dates, REP-2), so the
  report runs without `--var`; `dre validate` fails on a `var()` with no value.
- `reports/<area>/<name>/<name>.yml` with the schema line, one `.sql` per tab or document
  section (REP-1), `tab: false` for setup statements (REP-3), sources declared (REP-7).
- Logic as SQL, not template code (PORT-1): `IIf`, `Switch`, Crystal formulas and running totals
  become SQL expressions and window functions; formats become column formats (REP-5).
- `meta: {ported_from: "<the old report's path or name>"}` in the report YAML (PORT-5), and a
  `description`.
- A template (`.html`) next to the YAML for a pdf/html output; `schedules.yml` and `timings.yml`
  entries for the subscriptions; the packages under `plugins:` in `dependencies.yml`.
- Destination profiles that don't exist yet: the `dre-setup` way (show, confirm, `env_var()`).

Then run `dre validate -s <report>` and fix every error until it passes.

### Step 9: the parity check

The port isn't done until it matches (PORT-2). Follow `references/parity.md`:

1. **The old export:** ask the user for an export of the old report for one set of parameter
   values (recommend the last complete period), as csv (best) or xlsx, plus its PDF if the layout
   matters. `references/parity.md` says how to export from SSRS and Crystal.
2. **DRE's run, the same parameters:** `dre run <report> --var <name>=<value> --no-deliver
   --output-json parity`. It writes each query's rows to
   `parity/<report>/<binding>/<query>.json` and delivers nothing. Don't use `--preview`: its row
   limit would make the counts differ.
3. **Compare:** `python3 <skill>/scripts/parity.py --dre parity/<report>/<binding>/<query>.json
   --old <export> --key <columns>`. Use the columns that identify a row as `--key`; rename the
   old export's columns with `--map "Textbox12=region"`; skip its title rows with
   `--skip-rows N` and a grand-total line with `--skip-footer 1`.
4. **Explain every difference** to the user, by kind: *value* (a real difference: fix the SQL,
   or find out which report is right), *rounding* (within `--tolerance`; round in SQL or accept
   it), *formatting* (the same value written another way: fix the column formats), *order*
   (add an `ORDER BY`), missing or extra rows (a filter or join differs). Fix and repeat until it
   says MATCH, or until the user accepts each remaining difference with a reason.
5. **The layout:** for a pdf or a styled xlsx, ask the user to open the old and new files side by
   side, and fix what they point out.

### Step 10: hand over

Summarise: the files written, the parity result (rows, totals, differences and why), what had no
equivalent and what you did instead, and the next steps: a first real run held for review with
`dre-run` (RUN-9), running both reports in parallel for one cycle, and only then switching the
old subscription off (PORT-2). If decisions are still open (a difference to accept, a timezone,
a profile to set up), list them, and ask only the first. With an inventory, offer the next
report in the order.

## If this fails

- **`inventory.py` says "not readable XML":** the file is damaged or not a report; ask for
  another export.
- **`parity.py` exits 2:** a usage error (a missing file, a `--key` column one side doesn't
  have, an `.xls` file): its message says which; an old `.xls` must be saved as `.xlsx` or `.csv`.
- **Many value differences in one column:** often a type or rounding change (money as float),
  a time zone shift in dates, or a filter on a parameter that defaults differently. Compare one
  key's row by hand with the user.
- **The old export has subtotal rows between the data rows:** export the detail only (SSRS's
  csv export of the data region), or compare the totals the script prints instead of the rows.
- **`dre validate` errors:** as in `dre-report`'s "If this fails"; `dre explain <code>` explains
  an error code.
