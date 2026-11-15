# SFTP delivery

A synthetic CSV statement. Development delivers nowhere; the file remains under
`target/run/statement/default/runs/<run-id>/`.

```bash
dre validate
dre run statement
```

Expected CSV:

```csv
client,revenue
client_a,3500.00
```

For a real server, set these variables in your shell or secret manager:

| Variable | Value |
|---|---|
| `SFTP_HOST` | Server hostname |
| `SFTP_PORT` | Server port (default 22) |
| `SFTP_USERNAME` | Login name |
| `DRE_SECRET_SFTP_PASSWORD` | Login password |
| `SFTP_HOST_KEY_FINGERPRINT` | SHA256 fingerprint verified with the server administrator |

The host key is pinned; unknown hosts are not automatically trusted. Do not obtain the
fingerprint from an unverified connection. Edit `/incoming/client_a.csv` to the agreed
remote path. The upload is atomic and `if_exists: error` prevents replacing an existing file.

Inspect the local CSV, then explicitly send it:

```bash
dre validate --target prod
dre run statement --target prod
```

The same report also demonstrates these credential modes; replace their sample server
settings with your own environment variables before running the production target:

| Destination | Connection variables and defaults | Authentication and server verification |
|---|---|---|
| `bank_sftp` | `BANK_SFTP_HOST` (`sftp.bank.example`), `BANK_SFTP_PORT` (2222), `BANK_SFTP_USERNAME` (`acme_corp`) | `BANK_SFTP_KEY_FILE`, `DRE_SECRET_BANK_SFTP_KEY_PASSPHRASE`, `BANK_SFTP_KNOWN_HOSTS` |
| `archive_sftp` | `ARCHIVE_SFTP_HOST` (`sftp.archive.example`), `ARCHIVE_SFTP_PORT` (22), `ARCHIVE_SFTP_USERNAME` (`acme_corp`) | `DRE_SECRET_ARCHIVE_SFTP_KEY` (key text), `ARCHIVE_SFTP_HOST_KEY_FINGERPRINT` |
| `ops_sftp` | `OPS_SFTP_HOST` (`sftp.ops.internal`), `OPS_SFTP_PORT` (22), `OPS_SFTP_USERNAME` (`USER`, otherwise `reporter`) | Your SSH agent, `OPS_SFTP_KNOWN_HOSTS` (default `~/.ssh/known_hosts`) |

Use an explicit `OPS_SFTP_KNOWN_HOSTS` file to isolate automated runs from your personal SSH
configuration. Unknown hosts remain refused.

See [SFTP setup](../../docs/plugin-sftp.md) for private-key or SSH-agent authentication.
CI validates production settings with inert values and runs only the no-delivery target.
