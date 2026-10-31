# Public DRE examples

Small, complete projects using synthetic Acme Corp data. Most need no database server or account;
the overdue-loan example uses your Oracle or SQL Server test database. Install [DRE](../docs/install.md), then copy an example folder and
run commands from that folder. Dependencies install on demand; network access is needed
the first time.

| Project | What it demonstrates |
|---|---|
| [Tutorial](tutorial/) | One workbook, a variable, another tab, formats, Sets, a schedule and local delivery |
| [Monthly finance](monthly-finance/) | Source declarations, a typed account lookup, two tabs, formulas, totals and an Excel template |
| [Overdue loans](overdue-loans/) | Oracle/SQL Server JDBC preparation, independent business checks, ordinary/template workbooks and unexecuted SAS reference |
| [Bank file](bank-file/) | Fixed-width layout, zero padding, implied decimals and CRLF records |
| [Regional reports](regional-reports/) | The same report with two Sets and separate scheduled files |
| [Slack headline](slack-headline/) | A result-driven message and a `when:` threshold |
| [SFTP delivery](sftp-delivery/) | Environment variables, host-key pinning and atomic delivery |
| [Multilingual](multilingual/) | Per-row workbooks and PDF letters named after each city in its own script (Japanese, Arabic, Hindi, ...), with fonts fetched on demand |
| [Password statements](password-statements/) | Per-member password PDFs, a password list, an AES-256 zip pack and `secret_columns` |

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
The overdue-loan project has its own clean-run checker and runs against the pinned database
services in JDBC CI; see its README for credentials, Java and user-supplied driver requirements.

To check a local build with prebuilt plugins:

```bash
python .github/scripts/check_examples.py --dre /absolute/path/to/dre --plugins-dir /absolute/path/to/plugins
```

The CI matrix runs both the public install path and, when engine code changes, the current
source build on Linux, macOS and Windows.
