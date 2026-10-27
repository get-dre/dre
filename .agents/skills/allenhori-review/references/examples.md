# Examples from real reviews

Short excerpts of allenhori's comments on `get-dre/dre` pull requests, grouped by the pattern
they show. Read the linked PR for the full context.

## A ranked review with inline detail (#254)

The review body thanks the author, says what is right and what was verified, then ranks the
findings:

> Thanks @nijanthan-dev, the core fix looks right. Every error path after `start()` now goes
> through the single explicit `close()` in `run_session`, [...] and the `Fake` unit tests pass
> and do prove the close runs on all five paths.
>
> **Must fix before approval**
> 1. `query.rs:118`: the comment says the close also runs on a panic. It doesn't (no guard /
>    `catch_unwind`). Fix the wording or add a guard.
> 2. `query_leak.rs:105`: the e2e test likely passes on `master` too [...], so it doesn't prove
>    the fix. Check it against `master`, then strengthen or remove it.
>
> **Tidy-ups**
> 3. `query_leak.rs:33`: use `TestProject::dre_env` instead of building the command by hand.
> 4. `query_leak.rs:59`: share `alive` / pid polling with `cancel.rs` via `tests/common`. The
>    Windows `tasklist` pid match is a loose substring.
>
> **Optional**
> 5. `query.rs:183`: `run_session` / `session_result` layering and the repeated 5-parameter list.
> 6. `query.rs:264`: `Fake` bools → scenario enum, plus a test helper for the repeated `closed`
>    setup.
> 7. `Cargo.lock`: please mention the `syn` / `getrandom` edge changes in the summary.
>
> Bundling the lockfile fix in this PR is fine.

The inline comment for item 2 shows how to argue that a test does not prove a fix, and what to
do instead:

> **Must fix:** I don't think this test can fail on `master`, so it doesn't prove the fix:
>
> 1. `c.output()` in `run_query` waits for the whole `dre` process to exit before we check the
>    pid. [...]
> 2. Even mid-run, `impl Drop for PluginProcess` [...] closes stdin, waits, then `kill()` +
>    `wait()`s the child. [...]
>
> Could you check it by reverting `query.rs` to `master` and running this test? If it still
> passes, either make it observe something only `close()` does (e.g. have the fixture record
> that it received the protocol `Close` request, and assert on that), or remove it and rely on
> the `Fake` unit tests, which do prove the close runs on every path.

## Findings with a scope statement and a "your call" (#75)

> Review notes on this PR. I found two small things. Neither is a confirmed bug. I read the diff
> and the code around it, but I did not build the PR or run its tests.
>
> **1. A log event can turn into a blank line** (`crates/dre-cli/src/output.rs:306`)
> If turning a JSON event into text fails, `unwrap_or_default()` hides the error and prints an
> empty line. [...] The old code couldn't fail at this point, so this is a new failure mode.
>
> **2. Hiding secrets in keys can merge two entries** (`crates/dre-core/src/secrets.rs:58`)
> [...] Options are to leave keys alone, or to make masked keys unique.
>
> Neither needs to block the PR. Number 1 is a one-line fix. Number 2 is your call.

## Concrete edge cases and side-task failures (#79)

> 2. **`unique_masked_names_with` renames genuinely duplicate non-secret names**
>    (`secrets.rs`). For `select 1 as a, 2 as a, 3 as <secret>` the second `a` is stored as
>    `a#2`. [...] the next run sees `[a, a]` against the stored `[a, a#2, *****]` and reports
>    drift.
> 3. **Snapshot migration failure now aborts the run** [...] Previously a snapshot problem never
>    blocked a run. Consider warning and continuing.

## A re-review: acknowledge, then one change needed (#79)

> Thanks, my three earlier notes are all addressed with tests (conservative matching, duplicate
> literal names kept, snapshot write failure now a warning). The redaction work otherwise looks
> right.
>
> **One change needed:** in `Session::new`, the session's log protection turns on for the whole
> process when any rendered connection field contains a secret. [...] for most real
> Postgres/Databricks profiles a wrong host or bad login would show only that generic message.
>
> Suggested fix:
> - Keep the opaque replacement only for statements whose SQL contains a secret [...].
> - For connection secrets, use normal exact-value masking (`mask`) [...].
> - Don't let one sensitive statement keep plugin logs opaque for the rest of the session [...].
> - A test with a secret in the profile connection whose open fails, asserting the error detail
>   survives with the secret masked, would pin this down.
>
> Minor, non-blocking: test strings still say "PR75".

## Approval with optional notes (#81)

> LGTM. Thanks — the helper boundary is clean, failures carry useful context, and the size drop
> is big.
>
> Two optional follow-ups, not blocking:
> - [...]
> - `tests/duckdb-seed/README.md` is mostly one-off measurements for this change. Those would fit
>   better in the PR description than in the repo, where they'll go stale.

## Red CI with evidence (#40, #128)

> `integration` failed on `explicit_ftps_reuses_the_tls_session_for_data` with
> `connection refused` at 127.0.0.1:2990, the FTPS emulator container. This PR only changes the
> fixed_width plugin, not ftp.
>
> master is red the same way: `integration` failed on the last two completed master runs
> (8194ffe, 7a5bc77). [...] The CI step would need to keep them healthy, for example with
> `--restart` or a check just before the tests, or print `docker logs` when a test fails. I'm
> re-running the failed job once.

> Merged with `integration` red for reasons unrelated to this PR:
> - First run: one flaky sftp test, `unknown_hosts_are_refused_until_trusted`, failed on a
>   known_hosts lookup. The sftp plugin doesn't depend on dre-core or any YAML crate, and the
>   test passed on master.
> - Reruns: Docker Hub's unauthenticated pull rate limit (`toomanyrequests`) on the runner.

## End-to-end evidence (#124)

> dre-demo `./demo run all` with this build: 94 passed. The 14 Databricks scenarios failed
> because the workspace refused new sessions (`400 BAD_REQUEST: [...]`). The released dre 0.3.2
> fails the same way right now, so it's the warehouse, not this change.

## Closing and holding (#111, #162)

> Superseded by #113, which moves `arrow` and `parquet` to 59 together (and bridges to duckdb's
> arrow 58). Closing this one.

> Held: minijinja-contrib 3 needs minijinja 3, a major template-engine upgrade. Planned for 0.5,
> not a 0.4.x patch.
