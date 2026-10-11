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

See [SFTP setup](../../docs/plugin-sftp.md) for private-key or SSH-agent authentication.
CI validates production settings with inert values and runs only the no-delivery target.
