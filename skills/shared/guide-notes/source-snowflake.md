- The `snowflake` package is an alpha (1.0.0 pre-releases). Say so when you suggest it.
- Recommended sign-in (SEC-2), in order:
  1. a person at a laptop with company SSO: `authenticator: externalbrowser`; the driver keeps
     the SSO token in the OS keychain, DRE stores nothing;
  2. a scheduler or CI: key-pair sign-in, `private_key_path` to a key file kept outside the
     project, with `private_key_passphrase` from `env_var()` (SEC-3) if it's encrypted; or
     `workload_identity` where the platform supports it;
  3. a programmatic access token or a password only when nothing else is possible: `token` or
     `password` from `env_var()`.
- `account`, `user`, `role`, `warehouse`, `database` and `schema` aren't secret. A copied dbt
  profile works as it is.
- `load` (big lookups) needs a current `database` and `schema` in the profile.
- To make reports visible in Snowflake, deliver to the external stage's bucket with `s3`, `gcs`
  or `azure_blob`.
