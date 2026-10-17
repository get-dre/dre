# Topic: when there's no data (`on_empty`)

Load this when a query can return no rows (no exceptions today, no new members, a closed market)
or the user asks "what happens if it's empty?". SSRS's `NoRowsMessage` and Crystal's "suppress if
no records" map here.

## Ask (one question)

"Some days `<query>` may return nothing. What should happen then?"

| `on_empty` | What happens | Exit | When to recommend |
|---|---|---|---|
| `continue` (default) | written and delivered empty, with a warning | 0 | readers expect a file every time (a system picking it up) |
| `skip` | nothing written or delivered; a "skipped" line | 0 | people: don't send an empty email or post |
| `fail` | the run fails before anything is written | 1 | the data should never be empty (a regulator's daily file) |

## Write

```yaml
queries:
  - holdings
  - {query: exceptions, on_empty: skip}    # no exceptions: no Exceptions tab
output:
  format: xlsx
  on_empty: skip                           # nothing at all: send nothing
```

- Set on an output, or on a query entry for that query alone; a query's own setting wins.
- An empty file keeps its shape: a csv has its header, an xlsx tab its column names.
- In a pdf or html template, show an empty state with
  `{% if results.q.row_count == 0 %}...{% endif %}`; a query's `skip` has no effect there.
- **`count(*)` is never empty**: it returns one row holding 0. Use `when:` on the count for a
  message (`when: "results.exceptions.value > 0"`), or return the rows themselves.
- Per-row outputs: the output's `on_empty` decides an empty driver; each row's slice follows the
  query and output settings (a skipped row is recorded `skipped`, reason `no_data`).

Docs: [When there's no data](../docs/building-reports.md#when-theres-no-data).
