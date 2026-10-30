# Reports in any language

Nine city offices, each named in its own language and script: Japanese, Chinese, Korean,
Russian, Arabic, Greek, Hindi, Portuguese and German, with staff names and roles in the same
language. One run writes a workbook and a PDF letter per office, named after the city and
holding only that office's staff, and one workbook with every office.

```bash
dre validate
dre run cities
```

Expected files: `out/offices/東京.xlsx`, `out/offices/서울.xlsx`, `out/offices/القاهرة.xlsx`,
`out/offices/मुंबई.xlsx`, `out/offices/São Paulo.xlsx` and the rest, one per office with its
two staff, `out/letters/東京.pdf`, `out/letters/القاهرة.pdf`, `out/letters/मुंबई.pdf` and the rest
(one letter per office), plus `out/all-offices.xlsx` with all nine offices.

Text from the database keeps its script end to end: cell values, sheet contents and file names.
The file name here comes from `path:` (`{{ each.city }}`). Without a `path`, a per-row file is
named after the row's key, with letters and digits of any script kept; see
[per-row outputs](../../docs/per-row-outputs.md#files).

## The PDF letters

`letter.html` is one template for every office. Its `<html lang="{{ each.lang }}" dir="{{ each.dir }}">`
comes from the office row, so Arabic runs right to left and the Chinese, Japanese and Korean
letters each get their own font: those three share Han characters, and `lang` says which
language's shapes to draw.

No font is in this project. A PDF only uses fonts it is given, and the bundled ones cover Latin,
Greek and Cyrillic. For every other script DRE fetches the matching Noto font on first use, checks
it against a pinned checksum, and keeps it in `~/.dre/fonts`, so later runs need no network. The
first run therefore needs to reach the DRE registry. On a machine that can't, run
`dre fonts install ja zh ko ar hi` on a connected one and copy `~/.dre/fonts`, or see
[fonts for other languages](../../docs/plugin-document.md#fonts-for-other-languages).
