# Topic: PDF and HTML documents

Load this when a report should be a branded document: a statement, an invoice, a board pack, a
compliance summary, or a port of a paginated SSRS or Crystal layout. The `html` and `pdf` formats
come from the `document` package (add `document` under `plugins:` in `dependencies.yml`).

## Ask (one at a time, only what's still open)

1. **PDF or HTML?** Recommend `pdf` for something printed, filed or emailed as a statement;
   `html` for a page opened in a browser or pasted into a portal. One template makes both.
2. **Do they have a layout to match?** A sample PDF, a screenshot, a Word mock-up or the old
   report. Ask for it (free text or a file path); without one, recommend starting from the plain
   document (no `template:`: a table per query) and styling from there.
3. **The page:** size and orientation (recommend A4 portrait, or Letter in North America),
   running header and footer, page numbers ("Page 2 of 5").
4. **Branding:** a logo file, colours, a font file (`.ttf`/`.otf`) they're licensed to use.
   Without a font, the bundled IBM Plex Sans is used.
   **Other languages:** Japanese, Chinese, Korean, Arabic, Hebrew, Hindi and Thai need nothing in
   the project. For a PDF, DRE fetches the matching Noto font on first use (pinned, checksummed,
   cached in `~/.dre/fonts`); an offline machine needs `dre fonts install ja ar ...` once, an
   offline registry copy or the project's own `@font-face`; `missing_fonts: error` fails instead
   of drawing boxes. Han characters follow the HTML `lang` (`<html lang="{{ each.lang }}">`, or
   `<td lang="ja">`); without it, only Chinese is guessed. Never commit fonts for this.
   `examples/multilingual` has a PDF letter per office in nine languages.
5. **Legal text:** terms or disclaimers go in their own template files, included per Set or
   client (REP-11).

## Write

```yaml
# reports/clients/statement/statement.yml
queries: [summary, holdings]          # summary: one row of headline numbers (REP-10)
output:
  format: pdf
  template: {file: statement.html}    # next to the YAML, then parent folders, then unique in the project
  author: Acme Funds
```

```html
<!-- reports/clients/statement/statement.html -->
<!doctype html>
<html lang="en-AU">
<head>
  <title>Statement for {{ var('client') }}</title>
  <link rel="stylesheet" href="brand.css">
  <style>
    @page { size: A4; margin: 20mm 15mm;
            @bottom-center { content: "Page " counter(page) " of " counter(pages); } }
  </style>
</head>
<body>
  <img src="logo.svg" alt="Acme">
  <p>Balance on {{ run.date | date('d mmmm yyyy') }}:
     <strong>{{ results.summary.first.balance | currency('AUD', 2) }}</strong></p>
  {% if results.holdings.row_count == 0 %}<p>No holdings this period.</p>
  {% else %}{{ table(results.holdings) }}{% endif %}
  {% include "terms.html" %}
</body>
</html>
```

- A template reads `results.<query>` (`first.<column>`, `rows`, `row_count`, `value`), `var()`,
  `run.*`, the number filters (`number`, `percent`, `signed`, `currency`, `compact`), `date(...)`,
  `table(results.q)` and `row.fmt('column')` for a column's YAML `format`.
- **Aggregate in SQL** (REP-10): totals and subtotals come from a query (`ROLLUP`), never summed
  in Jinja. Group headings: loop over rows and print a heading when the group column changes.
- Images, stylesheets and fonts are files in the project; URLs are refused (rendering is offline).
- Shared layout: `{% extends "base.html" %}` and `{% include %}`; project macros aren't available.

## Design loop

```bash
dre run statement --no-deliver --output-json design    # runs the queries once
dre run statement --from-results design                # each tweak: re-render only, no query
```

Open the file under `target/run/` after each re-render and ask the user what to change.

## Check

`dre validate -s <report>` parses the template and its includes, checks images and stylesheets
exist and each `results.<name>` is a query of the output. A missing value fails the run, naming
the template line, so a document never goes out with a silent blank. `max_rows` (default 50,000)
caps a document's rows: aggregate or filter instead of raising it.

Docs: [PDF and HTML documents](../docs/plugin-document.md).
