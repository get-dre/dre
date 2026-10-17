# Topic: delivery and traffic control

Load this when choosing where a report goes, adding a recipient or channel, posting a headline
message, or controlling how deliveries are sent: rates, retries, holding for review. Use only
fields and options listed in `../plugins/destination-<type>.md`.

## Ask (one at a time, only what's still open)

1. **Where does it go?** Recommend the local `target/run/` folder only while building (nothing is
   sent), then the real destination:

   | They say | Destination (package) |
   |---|---|
   | email it | `email` (`email`) |
   | post to a channel | `slack` (`slack`), `teams` (`teams`), `google_chat` (`google_chat`) |
   | drop it in a bucket | `s3`, `gcs`, `azure_blob` (`object_store`) |
   | a file server or a partner's folder | `sftp` (`sftp`), `ftp` (`ftp`) |
   | a Databricks Volume | `databricks` (`databricks`) |

   The destination profile (server, credentials) lives in `profiles.yml` under `destinations:`;
   recipients, channels and paths belong to the report. Set up a missing profile the `dre-setup`
   way, with `env_var()` for any secret.
2. **The file or the number?** People who want the number get a `message` output (built in), on
   its own or with the file attached (`attach: [<output name>]`). Chat posts to `teams` and
   `google_chat` take messages only: link the file instead.
3. **Only sometimes?** `when: "results.<query>.row_count > 0"` sends a message only when the data
   calls for it (or `on_empty`, see `no-data.md`).
4. **How many sends?** For per-row outputs or big lists, ask the service's limit and set `rate:`.
5. **Review first?** For regulatory or client-facing sends recommend `hold: true` (RUN-9).

## Write

```yaml
output:
  - name: workbook
    format: xlsx
    queries: [detail]
    destination:
      - profile: reports_sftp
        path: "/outbound/sales-{{ run.date.yyyymm }}.xlsx"   # the period in the path (RUN-7)
        if_exists: error                                     # never replace a delivered file
  - name: headline
    format: message
    queries: [headline]                                      # a one-row query (REP-9)
    when: "results.headline.first.net != 0"
    text: "Net sales last month: **{{ results.headline.first.net | currency('EUR') }}**"
    destination:
      - {profile: team_slack, channel: "#finance", attach: [workbook]}
      - {profile: finance_mail, to: [finance@example.com], attach: [workbook], hold: true}
```

## Traffic control

| Key (destination entry) | What it does |
|---|---|
| `rate: 100/minute` (`/second`, `/hour`) | spaces deliveries evenly across every worker; `burst: 20` lets 20 go back to back |
| `retries` (default 3), `retry_wait` (default about 1s, doubling; e.g. `30s`), `max_retry_wait` (default `10m`) | temporary failures (429, 5xx, SMTP 4xx) are tried again; also in the profile entry |
| `hold: true` | renders and records the delivery, sends nothing until `dre release` |
| `{deliver: false}` (a profile's target entry) | delivers nowhere on that target, e.g. `dev` (RUN-8) |

`dre run --rate 30/minute` and `dre run --hold` apply to every destination for one run. After a
held run is reviewed:

```bash
dre release <report> --run <run id> --by <name>            # every held delivery
dre release <report> --run <run id> --only-keys 101,102    # some rows of a per-row output
```

`dre release` sends the files byte for byte as rendered, refuses any that changed, and never
sends one twice. Keep the run in the target path for the review window.

Nothing is sent twice by a retry: an email is retried only when the server certainly didn't
accept it.

Docs: [Messages](../docs/messages.md), [Plugins: Destinations](../docs/plugins.md#destinations),
[Tries again](../docs/plugins.md#tries-again),
[Held deliveries](../docs/per-row-outputs.md#held-deliveries-and-dre-release).
