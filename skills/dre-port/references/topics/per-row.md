# Topic: per-row outputs (`for_each`)

Load this when one run should make a file, email or message per member, client, account or
region: a statement per member, an extract per client, an SSRS data-driven subscription, a
Crystal report "burst" per customer. Other tools call it bursting.

`for_each` makes one copy of an output per row of a **driver** query. The report's queries run
once for everyone; DRE splits each query's rows by a **key**, and each copy gets only its slice.

## Per-row output or Sets?

| | Sets (REP-4) | `for_each` |
|---|---|---|
| How many | a handful, named in YAML (clients with their own settings) | hundreds to millions, from the data |
| Queries | run once per Set | run once for everyone |
| Per-copy settings | anything: vars, profile, template, destination | only what the driver row holds (`each.*`) |

Recommend Sets for a few variants that differ in settings, `for_each` when the list comes from
the data.

## Ask (one at a time, only what's still open)

1. **The driver:** which query lists the recipients, one row each? Usually a small query of its
   own (`members`: id, name, email).
2. **The key:** an opaque id present in the driver and every query of the output (REP-12: never
   an email or a name, because keys name files and appear in logs unmasked).
3. **Where each copy goes:** a path per row with `each.*` (required for file destinations), or
   an address (`to: "{{ each.email }}"`).
4. **When a row fails:** `on_row_error: continue` (recommended: the rest carry on, retry the
   failures later) or `fail` (nothing is delivered if any row fails; for all-or-nothing batches).
5. **Review first?** For regulatory or client-facing sends, recommend `hold: true` and
   `dre release` after review (RUN-9). Load `delivery.md` for rates and retries.

## Write

```yaml
queries: [members, holdings]
output:
  - name: statement
    format: pdf
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
      - {profile: member_mail, to: "{{ each.email }}", attach: statement, rate: 120/minute, hold: true}
```

- `each.<column>` is the driver row, in templates, titles, texts, paths and addresses; not in SQL.
- Each row's file is `<output>/<key>.<ext>` in the run folder, or what `file_name:` renders.
- Other `for_each` keys: `concurrency` (default 4), `on_orphan_rows` (`warn` or `fail` for rows
  whose key isn't in the driver), `max_population_rows`, `keep_results`.
- Addresses and other `each.*` values are masked in logs and results; the key isn't.

## Test before anyone gets anything (RUN-8)

Give each destination profile a `dev: {deliver: false}` entry, then:

```bash
dre run statements --preview                    # first 3 rows, masked, sends nothing
dre run statements --preview --only-keys 101,102
dre run statements --limit-rows 10              # a partial run (still delivers, on the run's target)
dre run statements --retry-failed <run id>      # redo only the failed rows, from saved results
```

Docs: [Per-row outputs](../docs/per-row-outputs.md).
