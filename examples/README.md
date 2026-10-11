# Public DRE examples

Small, complete projects using synthetic Acme Corp data. No database server or account is
needed for local runs. Install [DRE](../docs/install.md), then copy an example folder and
run commands from that folder. Dependencies install on demand; network access is needed
the first time.

| Project | What it demonstrates |
|---|---|
| [Tutorial](tutorial/) | One workbook, a variable, another tab, formats, Sets, a schedule and local delivery |
| [Monthly finance](monthly-finance/) | Source declarations, a typed account lookup, two tabs, formulas, totals and an Excel template |
| [Bank file](bank-file/) | Fixed-width layout, zero padding, implied decimals and CRLF records |
| [Regional reports](regional-reports/) | The same report with two Sets and separate scheduled files |
| [Slack headline](slack-headline/) | A result-driven message and a `when:` threshold |
| [SFTP delivery](sftp-delivery/) | Environment variables, host-key pinning and atomic delivery |

The Slack and SFTP examples have `dev: {deliver: false}`. A default run formats real output
locally and cannot contact the destination. Production settings require your environment
variables and an explicit `--target prod`; inspect output first.

## Verify examples

From the repository root:

```bash
python .github/scripts/check_examples.py
```

The script validates every project, including production configuration with inert test
values, runs every example in the development target, and checks the saved workbook values,
formats, formulas, template expansion, text record bytes and skipped message branch.
It also creates a new project with `dre new` and tests each tutorial step.
Scratch projects use the system temporary directory and are removed, including on failure.

To check a local build with prebuilt plugins:

```bash
python .github/scripts/check_examples.py --dre /absolute/path/to/dre --plugins-dir /absolute/path/to/plugins
```

The CI matrix runs both the public install path and, when engine code changes, the current
source build on Linux, macOS and Windows.
