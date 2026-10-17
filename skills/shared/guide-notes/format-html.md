- A document from a Jinja HTML + CSS template, written as one self-contained `.html` (images,
  stylesheets and fonts inlined; `embed_assets: false` keeps links). Same template as `pdf`.
- The template reads what a message reads: `results.<query>` (`value`, `first`, `rows`,
  `row_count`, `columns`), `var()`, `run.*`, the number filters, plus `date('d mmm yyyy')` and
  `table(results.<query>)`. Aggregate in SQL, not in the template.
- Name the template by file name (`template: {file: statement.html}`): found next to the YAML,
  then in parent folders, then anywhere unique. Share layout with `{% extends "base.html" %}`.
- Keep legal text (terms, disclaimers) in included files, not in vars.
