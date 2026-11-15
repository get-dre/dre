# Cloud delivery

One day of synthetic Acme Corp orders, delivered as Parquet to Amazon S3, Google Cloud Storage
and Azure Blob Storage, and as partner files (csv and delimited, each to its own
specification) over FTPS, plain FTP and object storage.

```bash
dre validate
dre run
```

The default `dev` target formats every file and delivers nothing (`dev: {deliver: false}`): the
files stay in `target/run/`. Two other targets need your environment variables:

- `local`: S3, GCS and Azure emulators and an FTP server on this machine (`S3_ENDPOINT`,
  `GCS_ENDPOINT`, `AZURE_BLOB_ENDPOINT`, `FTP_HOST`, ... with the defaults in `profiles.yml`).
- `prod`: the real stores, signing in with an AWS profile (SSO or `credential_process`), a GCS
  key file, the machine's Azure managed identity, a partner's SAS token and `az login`.

Inspect the local files first, then `dre run --target prod`.

What each output shows:

| Output | Shows |
|---|---|
| `orders`: `latest`, `archive`, `audit` | Parquet; `if_exists: overwrite`, `error` and `number` |
| `bank` | Every field quoted, CRLF, a byte order mark; FTPS with `atomic` upload, `temp_dir` and retries; held for `dre release` |
| `insurer` | Text columns quoted, a `NULL` marker, formula-leading text defused |
| `loader` | No quoting, no header, `;` |
| `legacy` | Pipe-delimited Latin-1, in a ZipCrypto zip over plain FTP |
| `partner_tab`, `minimal` | Tab- and `;`-delimited, quoted strings or only where needed |

`dre_project.yml` sets the csv and delimited defaults (`format_options`) and US-style weeks.
