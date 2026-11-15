# Public DRE examples

Small, complete projects using synthetic Acme Corp data. Most need no database server or account.
The overdue-loan and warehouse examples need your databases, and the data-test example has an
optional PostgreSQL source scenario. Install [DRE](../docs/install.md), then copy an example folder and
run commands from that folder. Dependencies install on demand; network access is needed
the first time.

| Project | What it demonstrates |
|---|---|
| [Tutorial](tutorial/) | One workbook, a variable, another tab, formats, Sets, a schedule and local delivery |
| [Data tests](data-tests/) | Saved-result and source checks, reconciliation, run/test/build, warning/error Bindings and capped CSV evidence |
| [Monthly finance](monthly-finance/) | dbt-style source declarations, YAML, CSV and Excel lookups, typed variables, styled tabs, formulas, totals and an Excel template |
| [Overdue loans](overdue-loans/) | Oracle/SQL Server JDBC preparation, independent business checks, ordinary/template workbooks and unexecuted SAS reference |
| [Bank file](bank-file/) | Fixed-width layout, zero padding, implied decimals and CRLF records, plus a readable copy with a header |
| [Regional reports](regional-reports/) | The same report with five Sets (renamed tabs, empty-result rules, excluded queries), schedules and shared timings |
| [Slack headline](slack-headline/) | A result-driven message and a `when:` threshold |
| [SFTP delivery](sftp-delivery/) | Environment variables, host-key pinning, key files, key text, the SSH agent and atomic delivery |
| [Multilingual](multilingual/) | Per-row workbooks and PDF letters named after each city in its own script (Japanese, Arabic, Hindi, ...), with fonts fetched on demand; a notice in every paper size |
| [Password statements](password-statements/) | Per-member password PDFs, a password list, an AES-256 zip pack and `secret_columns` |
| [Cloud delivery](cloud-delivery/) | Parquet and partner csv/delimited files to S3, GCS, Azure Blob, FTPS and FTP, with every sign-in and `if_exists` choice |
| [Messages](messages/) | One headline by email (STARTTLS, implicit TLS, plain), Teams, Google Chat and Slack Block Kit; a per-row email with a rate limit |
| [Warehouses](warehouses/) | Postgres, ClickHouse, Databricks, BigQuery and Snowflake with every connection and sign-in mode; macro packages and `dispatch` |
| [MCP](mcp/) | A report described for AI clients through the optional MCP companion |

Every example with a remote destination has `dev: {deliver: false}`. A default run formats real output
locally and cannot contact the destination. Production settings require your environment
variables and an explicit `--target prod`; inspect output first.

## Verify examples

From the repository root:

```bash
python .github/scripts/check_examples.py
```

The script validates every project in every target with inert test values, runs every
example that needs no server or account in the development target, and checks the saved workbook values,
formats, formulas, template expansion, text record bytes and skipped message branch.
It also creates a new project with `dre new` and tests each tutorial step.
Scratch projects use the system temporary directory and are removed, including on failure.
The overdue-loan and warehouse projects are validated only. The overdue-loan project has its own clean-run checker and runs against the pinned database
services in JDBC CI; see its README for credentials, Java and user-supplied driver requirements.

To check a local build with prebuilt plugins:

```bash
python .github/scripts/check_examples.py --dre /absolute/path/to/dre --plugins-dir /absolute/path/to/plugins
```

The CI matrix runs both the public install path and, when engine code changes, the current
source build on Linux, macOS and Windows.
