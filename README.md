<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/logo-dark.svg">
    <img src="docs/assets/logo.svg" alt="DRE logo" width="360">
  </picture>
</p>

<p align="center"><strong>Reports as code.</strong> The dbt-style workflow, for reporting.</p>

<p align="center">
  <a href="https://github.com/get-dre/dre/releases/latest"><img src="https://img.shields.io/github/v/release/get-dre/dre?filter=v*&sort=semver&label=release" alt="Latest release"></a>
  <a href="https://pypi.org/project/dre-cli/"><img src="https://img.shields.io/pypi/v/dre-cli?label=pypi" alt="PyPI version"></a>
  <a href="https://crates.io/crates/dre-cli"><img src="https://img.shields.io/crates/v/dre-cli?label=crates.io" alt="crates.io version"></a>
  <a href="https://github.com/get-dre/dre/actions/workflows/ci.yml"><img src="https://github.com/get-dre/dre/actions/workflows/ci.yml/badge.svg?branch=master" alt="CI"></a>
  <a href="https://pypi.org/project/dre-cli/"><img src="https://img.shields.io/pypi/dm/dre-cli?label=pypi%20downloads" alt="PyPI downloads"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/get-dre/dre" alt="License: GPL-3.0"></a>
  <a href="https://getdre.com/docs/"><img src="https://img.shields.io/badge/docs-getdre.com-1d6fd8" alt="Documentation"></a>
</p>

# DRE

<!-- package-description:start -->
**DRE**, the **Declarative Reporting Engine**, is open-source reports as code. You keep each report
as `.sql` and YAML files in git; DRE runs the SQL against your databases (each tab of a workbook can
come from a different one, and tables can be declared as dbt-style sources), writes the result as
csv, delimited, fixed-width, parquet or xlsx (including multi-sheet workbooks, number formats,
formulas and totals rows, and branded Excel templates), and delivers the file wherever it needs
to go: email, SFTP/FTP, S3, GCS, Azure Blob, Databricks Volumes or Slack. A report can also send a
headline [message](docs/messages.md) built from its results, to Slack or email (Microsoft Teams and
Google Chat are in preview). It runs on whatever scheduler you already have: cron, Airflow, Dagster, Databricks Jobs.

If you know dbt, you already know DRE: a project of SQL and YAML, Jinja and macros, `ref()`,
`source()`, folder config, tags, selectors, profiles and targets. dbt builds your tables; DRE
delivers the last mile, the reports people receive. DRE is inspired by dbt and is an independent
project, not affiliated with or endorsed by dbt Labs, Inc. (dbt is their trademark).
[DRE and dbt, side by side](https://getdre.com/dbt/).
<!-- package-description:end -->

Website: [getdre.com](https://getdre.com).

Status: under active development, before 1.0. A patch release never breaks a project; a minor
release may, with release notes and a [migration guide](docs/migrating-to-0.2.md).

## Install

```bash
curl -fsSL https://getdre.com/install.sh | sh   # macOS and Linux
brew install get-dre/tap/dre                                                          # Homebrew
pip install dre-cli                                                                   # pip, uv, pipx
```

More ways, and Windows, in [Install](docs/install.md). Or let a coding agent do it: DRE's
[agent skills](skills/README.md) guide you from installing DRE to a delivered report.

```bash
claude plugin marketplace add get-dre/dre && claude plugin install dre@dre   # Claude Code
npx skills add get-dre/dre#skills-latest                                     # other agents
```

Then ask: "help me with dre".

## Quick start

```bash
dre init               # pick a source, enter its connection, start a project
cd my_reports
dre validate           # check the project and compile its SQL
dre run                # run every report; output lands in target/run/
```

## Documentation

- [Getting started](docs/getting-started.md) and [Install](docs/install.md)
- [Concepts](docs/concepts.md), [Build and run reports](docs/building-reports.md), [Connections and targets](docs/connections.md), [Sources](docs/sources.md), [Schedules](docs/schedules.md), [the orchestration recipe](docs/orchestration.md)
- [Templates](docs/templates.md) and [Lookups](docs/lookups.md)
- [Plugins](docs/plugins.md), [Managing plugins](docs/managing-plugins.md), [the registry and `dre.lock`](docs/registry.md), [the plugin protocol](docs/protocol.md)
- [The target path](docs/target-path.md), [the manifest and `run_results.json`](docs/manifest.md), [schedule occurrences (`dre schedule ls`)](docs/schedule-ls.md)
- [Updating DRE](docs/updating.md), [Upgrading to 0.2](docs/migrating-to-0.2.md), [Environment variables](docs/environment-variables.md), [Building from source](docs/building-from-source.md)
- [Practices: how to set up, write and run reports well](docs/practices.md)
- [Agent skills: DRE in your coding agent](skills/README.md)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Pull requests need a signed [CLA](CLA.md) and a
maintainer's approval.

## License

This project is licensed under the [GNU General Public License v3.0](LICENSE).

A commercial license — for embedding DRE into a product or service you distribute to third parties, without GPL's copyleft obligations — is also available. Contact the maintainer for details.
