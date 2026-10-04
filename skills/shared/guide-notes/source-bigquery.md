- The `bigquery` package is an alpha (1.0.0 pre-releases). Say so when you suggest it.
- Recommended sign-in (SEC-2), in order:
  1. a person at a laptop: leave `method` out (`oauth`) after `gcloud auth
     application-default login`; DRE stores nothing;
  2. a scheduler or CI: `method: service-account` with `keyfile` pointing at a key file the
     user keeps outside the project, or `method: external-oauth-wif` where the platform offers
     workload identity federation;
  3. `service-account-json` or `oauth-secrets` only when the key or token can only arrive as a
     value: then `keyfile_json` or `token` from `env_var()` (SEC-3).
- `project` and `dataset` aren't secret. A copied dbt profile's `database` and `schema` work too.
- Suggest `maximum_bytes_billed`, so a mistaken query fails instead of running up a bill, and
  `location` when the datasets aren't in the US.
- `dre validate --live` dry-runs every statement; `--debug` shows the bytes each would process.
