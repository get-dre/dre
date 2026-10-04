- `webhook_url` is a credential: always `env_var()` (SEC-3). The user creates it in Teams (channel
  menu > Workflows > "Post to a channel when a webhook request is received") and sets the
  variable themselves; never ask them to paste it.
- Messages only: a file output or `attach:` is refused. Deliver the file to object storage and
  link it from the message with `outputs.<name>.location`.
- The package is a release candidate (1.0.0-rc.1); say so.
- Posting to a channel needs a confirmation and a preview first (RUN-2, RUN-6).
