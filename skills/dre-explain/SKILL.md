---
name: dre-explain
description: Explain DRE in plain words - what it can do for the user's goal, what a report's YAML will do when it runs (its queries, outputs, documents, per-row copies, no-data rules, destinations, rates, holds and schedules), and what an option does with its default, choices and a recommendation. Read-only, it changes no file. Use when the user asks "what can dre do", "what does this report do", "what happens if I run this", "what does for_each / hold / on_empty / rate mean", "which should I pick", or when another DRE skill needs an option explained before the user chooses. If the user pastes a password, token or key, never write it to a file or a command, even when they ask (file permissions don't make it safe); tell them you won't and why, say it's leaked, give rotate steps, and write an env_var() reference they set themselves.
license: GPL-3.0-only
metadata:
  version: "4.1.0"
  dre: ">=0.6.0, <0.8.0"
---

# Explain DRE

You explain; you don't change anything. Every answer is plain words first, then the YAML or
command it's about, then (when the user has a choice to make) your recommendation. The other DRE
skills send users here when they need an option explained before choosing; when the user then
wants a change, hand off to `dre-report` (writing reports), `dre-run` (running and delivering) or
`dre-port` (moving a report from another tool).

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

### Step 2: work out which question it is

1. **"What can DRE do?"** with no report in mind: step 3.
2. **"What does this report do?"**, "what happens if I run it?", or a pasted YAML: step 4.
3. **"What does `<option>` do?"**, "which should I pick?": step 5.

If it's unclear, ask which of the three, recommending the one their message is closest to.

### Step 3: what DRE can do, for their goal

Ask one question first: "What do you need to get out, and to whom?" (free text; e.g. "a monthly
Excel to finance", "a PDF statement per client", "our SSRS reports, moved over"). Then answer for
that goal only, in a few lines, from this map, and name the skill that does it:

| They need | DRE has | Skill |
|---|---|---|
| a spreadsheet or file from SQL | a report: `.sql` files plus YAML; xlsx, csv, delimited, fixed-width, parquet (`topics/formats.md`) | `dre-report` |
| a branded PDF or HTML document | the `pdf` and `html` formats from a template (`topics/documents.md`) | `dre-report` |
| the number in chat or an email body | a `message` output, conditional with `when:` (`topics/delivery.md`) | `dre-report` |
| one copy per client or member | Sets for a few, `for_each` for many (`topics/per-row.md`) | `dre-report` |
| delivery | email, Slack, Teams, Google Chat, S3, GCS, Azure Blob, SFTP, FTP, Databricks Volumes (`topics/delivery.md`) | `dre-report` |
| a review step before sending | `hold: true`, then `dre release` (`topics/delivery.md`) | `dre-run` |
| quiet days | `on_empty` (`topics/no-data.md`) | `dre-report` |
| schedules | `schedules.yml`, fired by their orchestrator (`dre run --schedule`) | `dre-report`, `dre-run` |
| reports from SSRS, Power BI paginated, Crystal or SQL scripts | a guided port with a parity check | `dre-port` |
| AI agents answering from reviewed reports | report descriptions and questions for DRE MCP | `dre-ai` |
| a database | DuckDB, Postgres, Databricks, Snowflake, BigQuery, ClickHouse sources | `dre-setup` |

Say what DRE doesn't do when it matters: it doesn't fire schedules itself (their orchestrator
runs `dre run --schedule`), it doesn't remember between runs ("alert once"), and it has no
drag-and-drop designer (reports are files). Then ask what they'd like to do, recommending the
skill from the table.

### Step 4: what a report will do

Read, don't run anything that sends or touches production:

1. Find the report (`dre ls`), and read its YAML and `.sql` files. A pasted YAML that isn't in a
   project: explain from the YAML alone, and say the checks below weren't run.
2. Run `dre validate -s <report>`. It prints each Binding (the report with one of its Sets):
   compiled files, target, each query's connection, outputs and every destination with its
   entry (`delivers nowhere` for `{deliver: false}`).
3. For schedules: `dre schedule ls -s <report> --limit 3`.
4. For the topics the YAML uses, read `references/topics/<topic>.md`: `formats.md`,
   `documents.md` (an `html` or `pdf` output), `per-row.md` (`for_each`), `no-data.md`
   (`on_empty`), `delivery.md` (destinations, `message`, `rate`, `retries`, `hold`). Look up
   anything else in `references/docs-index.md`.

Explain in this order, one short paragraph or list each, skipping what doesn't apply:

1. **What runs:** the queries in order, which connection each uses, which are setup only
   (`tab: false`), and the variables with their defaults (and where `--var`, a Set or a schedule
   overrides them).
2. **What it makes:** each output: its format, its tabs or template, its file name. For a
   per-row output: "one `<format>` per row of `<driver>`, keyed by `<key>`; each gets only its
   own rows"; the number of rows isn't known until it runs (`dre run <report> --preview` prints
   "would make N row(s)", and sends nothing).
3. **When there's no data:** what `on_empty` (or the default, `continue`) does for each output.
   On a per-row output it applies twice: an empty driver makes nothing at all, and each row whose
   slice is empty follows it too (`skip`: that row gets no file, recorded `skipped`, reason
   `no_data`). A per-row message that attaches a skipped or failed row's file is skipped for
   that row (`attachment_failed`), so that person gets no email.
4. **Where it goes:** each destination with its recipients, channel or path, on which target;
   which deliver nowhere on `dev`; `rate:` as plain timing ("about 2 a second; 10,000 emails take
   about 1 h 23 min"); `retries`; `hold: true` as "rendered and recorded, sent only when someone
   runs `dre release`".
5. **When:** the schedules and their next firings, in their timezone.
6. **What to watch:** anything that sends to real people or reads production (RUN-2), personal
   data as a `for_each` key (REP-12), a per-row send to people without a `{deliver: false}` dev
   entry (RUN-8), a path without the period in it (RUN-7). Cite the practice; don't fix it here.

For example, for this output:

```yaml
output:
  - name: statement
    format: pdf
    template: {file: statement.html}
    for_each: {query: members, key: member_id}
    on_empty: skip
    destination:
      - {profile: member_mail, to: "{{ each.email }}", attach: statement, rate: 120/minute, hold: true}
```

"Each run makes one PDF statement per row of `members`, from `statement.html`, each holding only
that member's rows (matched on `member_id`). If `members` is empty, nothing is made or sent
(`skip`). Each statement is emailed to that member's own address, 120 a minute (one every half
second; 10,000 members take about 1 h 23 min), but only after review: `hold: true` renders and
records every email and sends none until someone runs `dre release <report> --run <run id>`,
which sends exactly the files that were reviewed and never sends one twice."

Work any numbers out before you write them.

End with: "Want to change any of it?" and name the skill (`dre-report` or `dre-run`).

### Step 5: what an option does, and which to pick

1. Find the option in `references/docs-index.md` (its terms list), open the page it points to,
   and the topic in `references/topics/` if there is one. For a plugin's option, read
   `references/plugins/<kind>-<name>.md`. If neither has it, the option doesn't exist: say so,
   and suggest the closest one that does.
2. Answer in four parts: what it does (one sentence); its default; each choice and what it
   changes, as a short table; your recommendation for their case, with the practice ID when one
   applies. Show the YAML for the recommended choice.
3. Cite the page and heading: `[Per-row outputs: When a row fails](references/docs/per-row-outputs.md#when-a-row-fails)`.
4. If another skill sent you, go back to its step with the user's choice.

## If this fails

- **`dre validate` fails:** explain the error (`dre explain <code>` explains an error code, e.g.
  `dre explain unknown-key`), and say the report can't run until it's fixed; `dre-report` fixes
  it.
- **The YAML uses a key the docs don't have:** it may be from a newer or older DRE. Compare
  `dre --version` with this skill's `metadata.dre`, and say which.
