# Messages

Yesterday's synthetic Acme Corp sales as one headline in every channel: email (STARTTLS,
implicit TLS and a plain internal relay), Microsoft Teams, Google Chat and Slack (Block Kit,
with the file in a thread or a separate message, and a direct message). The csv file goes by
email with its own subject and body, and to Slack. A second report sends one email per
regional manager, at most 30 a minute.

```bash
dre validate
dre run
```

The default `dev` target renders every message and file and sends nothing. Amounts are
formatted for Australia (`locale: en-AU`): `Revenue: A$3,500 from 4 orders.` The `local`
target sends to a mail catcher and webhook mocks on this machine; `prod` sends for real. Both
need your environment variables (`DRE_SECRET_SMTP_PASSWORD`, `TEAMS_WEBHOOK_URL`,
`GOOGLE_CHAT_WEBHOOK_URL`, `DRE_SECRET_SLACK_TOKEN`, `SLACK_CHANNEL`). A webhook URL is a
credential: keep it in the environment, never in the file.
