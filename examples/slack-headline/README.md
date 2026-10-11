# Slack revenue headline

A single query supplies the message's revenue. `when:` sends it only when revenue reaches
the threshold. Development keeps the generated Markdown in `target/` and delivers nowhere.

```bash
dre validate
dre run headline
dre run headline --var threshold=4000
```

The first run keeps a Markdown message containing `Acme Corp revenue: $3,500`.
The second run skips the output because 3,500 is below 4,000: no file or delivery.
The run's `run_results.json` records this as `skipped`.

To send a real message, give your Slack bot permission to post and invite it to the channel.
Set `DRE_SECRET_SLACK_TOKEN` and `SLACK_CHANNEL` in your shell or secret manager.
Never put credentials in YAML or commit them. Inspect the development output, then run:

```bash
dre validate --target prod
dre run headline --target prod
```

See [Slack setup](../../docs/plugin-slack.md). CI validates the production shape with inert
environment values but runs only `dev`; it does not use a Slack account or send messages.
