---
title: "Tested examples"
description: "Complete, copyable projects with checked output: Excel, bank files, Sets, Slack and SFTP."
section: build-reports
position: 8
---

# Tested examples

These [public example projects](../examples/README.md) include the YAML, SQL and supporting
files needed to run them. Every project uses synthetic data and DuckDB, so local runs need
no database account. Start with [Your first report in 10 minutes](first-report.md).

| Example | Output | Files |
|---|---|---|
| First report | Two regional workbooks, after seven small steps | [Tutorial project](../examples/tutorial/README.md) |
| Monthly finance | Multi-tab workbook and a filled Excel template | [Finance project](../examples/monthly-finance/README.md) |
| Bank file | Exact-width payment records with CRLF endings | [Bank project](../examples/bank-file/README.md) |
| Regional reports | One workbook per Set, with named schedules | [Regional project](../examples/regional-reports/README.md) |
| Slack headline | Message controlled by a result threshold | [Slack project](../examples/slack-headline/README.md) |
| SFTP delivery | CSV and an environment-based remote profile | [SFTP project](../examples/sftp-delivery/README.md) |

[Install DRE](install.md), copy the chosen project directory and run `dre validate` from it.
Each README gives the run commands and expected values. Dependencies install on demand.

Slack and SFTP default to `dev: {deliver: false}`: outputs stay local. To use a real destination,
set your own environment variables, inspect the local output and explicitly select `--target prod`.

CI validates all six projects, runs their local paths and checks saved output. It also exercises
every tutorial stage on Linux, macOS and Windows. Production remote profiles are checked with
inert environment values; CI never delivers to Slack or SFTP.
