---
title: "New in 0.9"
description: "Selected runs isolate report errors, per-row populations stream to disk, and the core is prepared for large projects."
section: upgrading
position: 2
---

# New in 0.9

**Prepared, unreleased.** Core 0.9.0, compatible plugins and skills 5.0.0 are being prepared
as one coordinated release. This page does not announce published packages.

## Selected runs can proceed when another report is broken

`dre run monthly` checks that selection and its dependencies. Discovered errors in other reports
produce `N other reports have errors; run dre validate` on stderr (singular for one report).
Each Binding's `run_results.json` adds `project_errors`: one `path`, `code` and `message` entry
per affected report. Selected-report errors and errors in project-wide dependencies still stop
the run. See [Build and run reports](building-reports.md#run-a-selection-when-another-report-is-broken)
for the checks discovery covers.

`dre validate` remains the strict whole-project check, including when `-s` selects which
Bindings to show. No new strict flag is needed for runs.

Runs no longer rebuild the whole manifest. `dre compile` and `dre validate` still write the
full manifest; a run leaves it unchanged and sets `manifest_checksum` to null. Integrations
that need the current project description should call `dre validate --json` or `dre compile`
first. See [When it's written](manifest.md#when-its-written).

## Remove the per-row population cap

Remove `for_each.max_population_rows` from project YAML. It is rejected with a migration
message, without a deprecation period. Populations stay in the Arrow spool and a temporary disk
index; workers render bounded chunks, so no population row cap is needed. Allow disk space for
those files. Filter or aggregate in SQL when you need fewer copies; a document's own
`max_rows` still applies to each document.

## Other behavior changes

- Per-row delivery records and `dre release` use the same delivery error text and codes as
  Binding runs, including `local/file-exists` and the reminder that output is still in `target/`.
  A failed delivery no longer records retries from earlier batches.
- Validation rejects two unnamed outputs with the same extension, such as `delimited` and
  `fixed_width`. Per-row splitting rejects a query name containing `/`.
- `dre plugin list` gives a different error when a package manifest names no plugin. Git macro
  packages clone into temporary `dre_deps/.clone-*` folders.
- With JSON logs, a template query that fails during `dre compile` or `dre run --dry-run` has
  `error_code: compile-needs-run`, rather than `run-failed`.
- An output key set to `null` in a layer inherits the outer value. For example,
  `destination: null` in a Set inherits the report's destination.
- Template scans ignore Jinja-looking text inside strings, comments and `{% raw %}` blocks,
  and ignore `each.x` outside `{{ }}`. A source-role scan recognizes only the keyword argument
  named `role`.
- The parse pass runs once during planning: `run.now` used to choose a profile or source is the
  plan's instant. With multiple threads, source `describe` log lines appear during planning,
  before the target line. Connection limits bound the number of workers started.

## PDF fonts, including offline servers

The document 1.3.0 plugin uses checksum-verified Noto fonts for Chinese, Japanese, Korean,
Arabic, Hebrew, Devanagari and Thai PDFs without project fonts. Downloads are cached under
`~/.dre/fonts`. Han text follows the HTML `lang` attribute per element; without a suitable
language, kana selects Japanese, Hangul selects Korean, and other Han text uses Simplified
Chinese with a note suggesting `lang`. HTML output keeps the reader's fonts.

When a fallback font is unavailable, the PDF is made with boxes and warns once per script per
run. The warning explains three fixes: an offline `DRE_REGISTRY_URL` copy, `dre fonts install`
on a connected machine followed by copying the cache, or project `@font-face` fonts. Core
tries each font package once per run. The PDF option `missing_fonts: error` fails the output
or per-row copy instead; `warn` is the default. The option is rejected on HTML outputs.

`dre fonts install <script>...` (or `--all`) prepares the cache, accepting registry script
codes and language aliases; `dre fonts ls` lists cached packages, versions, scripts, sizes
and verification status. See [offline PDF fonts](plugin-document.md#offline-servers).

## Core APIs and scale

Core has a per-run context and a plugin-host interface, one delivery module and format catalog,
query-based project access, one Jinja parser, and explicit run stages with typed errors. Format
and destination processes are reused across outputs and Bindings, bounded by workers.

For Rust callers, `RunOptions` takes `mode: Mode` and `rows: RowOptions`; `RunSummary::error`
is an `Error`. `engine::EventSink` replaces `run::Ui` and `engine::dispatch`.
`find_plugin` is in `plugin_host`, `render_connection` in `profiles`, and `Level` in `engine`.
`Error::hint` was removed. Template scanning uses MiniJinja's `unstable_machinery` feature;
check scanning behavior when upgrading MiniJinja (the lockfile pins the current version).

## Compatibility and release order

DuckDB remains an optional plugin. For projects using it, core 0.9.0 **needs duckdb 1.3.0 or
later**: connection serialization for a DuckDB file now comes from the source's `single_writer`
description. DuckDB 1.2.1 does not provide it. Install a compatible DuckDB plugin before core;
projects that do not use DuckDB do not need that package.

The protocol and plugins keep their own version schemes. This source uses `dre-protocol`
0.10.0; the core release publishes that crate before the core crates that depend on it. The
font reply adds optional `repeated` (absent means false), with wire version still 1. Rust
`ResultSets::request_fonts` returns `Result<Result<Vec<String>, FontError>>`.

Publish required font registry assets before the document plugin that fetches them, and publish the
compatible DuckDB and document plugins before core 0.9.0. The PDF font work and this core change
must pass combined validation before that release.

Document 1.3.0 needs core 0.9.0 for the new font behavior. With an older core it can still make
PDFs using available project fonts, but cannot fetch fallback fonts; `missing_fonts: error`
then fails a PDF needing a fallback with a message requiring DRE 0.9.0. Core's `dre fonts`
commands work with any document plugin.

After core artifacts exist, publish skills 5.0.0 for DRE `>=0.9.0, <0.10.0`, then finalize demo
pins against those published artifacts and checksums. Skills 4.x remain available for earlier
DRE minors. Until release, validate demos with local builds and a local registry; do not pin an
unpublished package in a project that expects the public registry.

<!-- docs-nav: generated by .github/scripts/docs_sections.py from docs/sections.json -->

---

**Previous:** [Updating DRE](updating.md) · **Next:** [New in 0.8](new-in-0.8.md)
