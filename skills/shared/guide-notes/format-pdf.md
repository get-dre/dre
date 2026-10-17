- A branded PDF from a Jinja HTML + CSS template, made in-process: no browser, no service. The
  same template makes the `html` format; see the `document` plugin page.
- Page setup goes in CSS: `@page { size: A4; margin: 20mm; @bottom-center { content: "Page "
  counter(page) " of " counter(pages) } }`, `break-before: page`. Long tables repeat `<thead>`.
- Fonts: IBM Plex Sans/Mono are bundled; add brand fonts with `@font-face` from a `.ttf` in the
  project. System fonts are never used; an unknown family falls back with a warning.
- Assets (`<img>`, stylesheets) are files in the project; URLs are refused.
- `max_rows` (default 50,000) fails the run rather than truncate; aggregate in SQL.
