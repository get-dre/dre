- `webhook_url` holds the space's key and token: always `env_var()` (SEC-3). The user creates it in
  the space (Apps & integrations > Webhooks) and sets the variable themselves.
- Messages only: a file output or `attach:` is refused; link files with
  `outputs.<name>.location`.
- The package is a release candidate (1.0.0-rc.1); say so.
- Posting to a space needs a confirmation and a preview first (RUN-2, RUN-6).
