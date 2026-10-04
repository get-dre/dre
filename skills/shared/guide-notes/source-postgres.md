- Sign-in: Postgres has no sign-in that stores nothing, so `password` is always
  `"{{ env_var('<NAME>') }}"` (SEC-2, SEC-3), set by the user.
- Recommend `sslmode: require` (or `verify-full` with `sslrootcert`) for any server that isn't on
  the user's own machine; `prefer`, the default, falls back to no TLS without saying so.
- Recommend a read-only database user for reports.
- A database reachable only through a jump host: add an `ssh:` block (see the docs above) rather
  than telling the user to run `ssh -L`. `host` stays the database's address as the bastion sees
  it. Sign in to the bastion as for SFTP (SEC-2): a key pair, `private_key_path`, or on a CI
  runner the key's text in `private_key` from `env_var()`. Pin `host_key_fingerprint` (the error
  for an unknown bastion prints it) when there's no `known_hosts`. Keep `sslmode: verify-full`
  working through the tunnel: it checks the certificate against `host`.
- Cast unconstrained `numeric` to `numeric(p,s)` in SQL, or it arrives as text (see the docs
  above). Types DRE can't map need a cast, e.g. `::text`.
- On macOS, a `verify-ca` or `verify-full` server certificate valid for more than 825 days is
  rejected; see the docs above.
