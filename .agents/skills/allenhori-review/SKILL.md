---
name: allenhori-review
description: Review a DRE pull request the way the maintainer (allenhori) does, or check your own branch against those standards before you ask for review. Ranks findings as must fix, tidy-up or optional; gives each one a file and line, a concrete failure case and a fix; checks that a test fails without the fix; applies the Google Rust style guide with DRE's overrides; and explains red CI with evidence. Use when asked to review a PR or diff in this repository, to prepare a PR for review, to write a PR description, or to explain a CI failure on a PR.
license: GPL-3.0-only
metadata:
  source: "Review comments by allenhori on get-dre/dre pull requests #1-#254"
---

# Review DRE pull requests like the maintainer

This skill encodes how the maintainer of `get-dre/dre` reviews pull requests. It comes from
every review, inline comment and PR comment that `allenhori` wrote on the repository. Real
examples are in [references/examples.md](references/examples.md).

Use it in two modes:

- **Reviewer mode:** you review someone's PR or diff. You write the review.
- **Author mode:** you check your own branch before you open the PR or ask for review. You fix
  what the review would find, and you write the PR description.

The repository rules in [CONTRIBUTING.md](../../../CONTRIBUTING.md) apply in both modes. This
skill does not repeat them. It tells you how to apply them.

## Core principles

1. **Be exact about what you checked.** Say what you read, built and ran. Say what you did not.
   Example: "I read the diff and the code around it, but I did not build the PR or run its
   tests."
2. **Calibrate confidence.** Separate confirmed bugs from suspected ones. Write "I don't think
   this test can fail on `master`" or "likely" when you have not proved it. Write "Neither is a
   confirmed bug" when that is true.
3. **Every finding is concrete.** Give the `file:line`, a small input that triggers it, and the
   wrong result. Do not write "this might cause issues".
4. **Every finding has a fix.** Give one fix, or two options with the simplest first ("Either
   drop the claim (simplest), or add a guard"). Say whose decision it is when it is a judgment
   call ("Number 2 is your call").
5. **Rank by priority, and say what blocks.** Use the tiers below. Mark non-blocking items
   explicitly. Do not hold a PR for a matter of taste.
6. **Start with what is right.** Thank the author. Say in one sentence what the change gets
   right and what you verified. Then list the findings.
7. **Plain, short sentences.** Active voice. No hype, no filler, no exclamation marks.

## Priority tiers

Group findings under these headings, in this order. Number them across the whole review, so
the author can answer "fixed 1 and 3".

| Tier | Use it for | Blocks approval? |
|---|---|---|
| **Must fix before approval** | A wrong behavior, a claim in code or docs that is false, a test that does not prove the fix, a security or secret leak, a broken compatibility rule. | Yes |
| **Tidy-ups** | Duplicate code where a helper already exists, a loose match, a test that rebuilds setup by hand. | Normally yes, but cheap |
| **Optional** (or **Optional / FYI**) | Structure and naming ideas, small refactors, information the author should mention. | No |

When the review has only one blocking item, write **One change needed:** in front of it.

## Reviewer mode: the workflow

1. **Get the change.** Read the PR description, the linked issue and the full diff:

   ```bash
   gh pr view <number> --comments
   gh pr diff <number>
   ```

2. **Read the code around the diff.** Open each changed function and its callers. Find the
   existing helpers the change could reuse (for tests: `crates/dre-cli/tests/common/`).
3. **Check claims against the code.** Each comment, doc line and PR description sentence that
   says what the code does must be true. A comment that says "this also runs on a panic" when
   no guard exists is a must-fix.
4. **Check that the tests prove the fix.** For a bug fix, a test must fail without the fix.
   When you can, check it: revert the fix on a local branch and run the test.

   ```bash
   gh pr checkout <number>
   git checkout master -- <path/to/fixed/file.rs>
   cargo test -p <crate> --test <test_name>
   git checkout HEAD -- <path/to/fixed/file.rs>
   ```

   If the test still passes, say so. Suggest a test that observes something only the fix does,
   or suggest removing the test and relying on the tests that do prove it.
5. **Look for the failure classes below.** Use the checklist.
6. **Check CI.** If a job is red, find out why before you blame the PR. See
   [Red CI](#red-ci-explain-it-with-evidence).
7. **Write the review.** Use the [review template](#review-template). Put the full detail of each
   finding in an inline comment on its line. Put the ranked summary in the review body.
8. **Re-review.** When the author pushes changes, go through each earlier note. Say which are
   addressed and how ("all addressed with tests"). Then list only what is new or still open.

## Checklist: what this maintainer looks for

### Correctness and failure modes

- **Silent failure.** `unwrap_or_default()`, `let _ =` or an empty `catch` that hides an
  error. Ask: what does the user or a log reader see? An empty line where JSON was expected is a
  bug. Prefer an error or a logged warning.
- **A new failure mode.** Say when the old code could not fail at this point and the new code
  can.
- **Error paths.** Every early return after a resource opens (a plugin process, a file, a
  connection) must release it. Check each path, not only the happy path.
- **A guarantee the code does not give.** Panics, cancellation and signals skip plain cleanup
  code. A `Drop` guard gives the guarantee. A comment alone does not.
- **Small, concrete edge cases.** Duplicate names (`select 1 as a, 2 as a`), keys that collide
  after a transformation, a substring match on an id (`1234` matches `12345`), an unset
  environment variable, a read-only target directory.
- **Behavior that depends on the environment.** If the result changes when a variable is set in
  one place and not in another (CI against local, a rotated secret), stored state can show false
  drift on the next run.
- **Do not make a run fail for a side task.** If writing a snapshot or cache fails, warn and
  continue. Do not stop the run before it delivers anything.

### Secrets and redaction

- Secrets must not reach logs, errors, snapshots or files. See
  [`docs/practices.md`](../../../docs/practices.md) and `skills/shared/secrets.md`.
- Masking must not destroy the diagnostic the user needs. A wrong host or a bad login must still
  show its real error, with only the secret value masked.
- Scope a protection to the thing that needs it (one statement), not to the whole process or
  session.
- Ask for a test that proves both halves: the secret is masked, and the useful detail survives.

### Tests

- A bug fix comes with a test that fails without the fix.
- Reuse the shared test helpers (`TestProject::dre_env` and the rest of
  `crates/dre-cli/tests/common/`). Do not rebuild a `Command` by hand unless the test must spawn
  asynchronously.
- Move a helper that two test files copy into `tests/common`.
- A repeated setup block in several tests becomes a helper function.
- Test names and strings say what they test, not where they came from. Flag leftovers such as
  "PR75".

### Design and readability

- Several bools that each describe one scenario become an enum, so invalid combinations cannot
  exist.
- The same long parameter list passed through two layers becomes a struct, or the layers
  merge.
- These are normally **Optional**. Say "Fine as is" when they are.

### Repository hygiene

- One-off measurements, benchmarks and notes about this change go in the PR description, not in
  a committed file, where they go stale.
- `Cargo.lock` changes beyond the intended bump (the resolver moved another crate) are fine
  when they are in range. The PR summary must mention them.
- A plugin code change bumps that plugin's `version` (CONTRIBUTING).
- A user-visible change updates `README.md` or `docs/`. A generated page changes through its
  source.
- A breaking change is only allowed in a minor release, goes in the release notes and the
  migration guide, and a removed key is an error that says what to write instead.
- Skill sources in `skills/` change, never the generated copies. Then run
  `python3 .github/scripts/skills.py sync`, `generate` and `check`.
- One change per PR. Bundling a closely related fix (a yanked-crate lockfile update) is fine
  when the PR says so.

### Rust style: Google's Rust style guide

Rust code follows the [Google Rust style guide](https://google.github.io/styleguide/rustguide.html),
with the DRE rules below. Where they differ, the DRE rule wins.

**DRE rules over the guide:**

- `rustfmt.toml` and CI decide formatting: stable `cargo fmt` with `max_width = 110`, not the
  guide's 100. Do not ask for nightly-only rustfmt options such as `imports_granularity`.
- Apply the API rules by crate. `dre-protocol` and `dre-core` are libraries that plugins and
  other projects use, so the API rules bind. `dre-cli` and the plugins are applications, so the
  API rules are a strong recommendation, and simple, direct code wins over reuse.
- The rules apply to new and changed code. Do not ask a PR to restyle code it does not touch.
  A style sweep is its own PR.
- Not adopted: the rules marked `[chrome]` or `[Fuchsia]` (small crates, flat file layout, no
  `async`, no threads), and `#[non_exhaustive]` on public enums and structs.

**Must fix before approval:**

- `unsafe` without a `// SAFETY:` comment that says why the code is correct and why safe code
  does not work.
- A panic (`unwrap()`, `expect()`, indexing, `panic!`) on input from a user, a project file, a
  plugin or the network. Return a `Result` with context. A panic is only for a bug or a broken
  invariant. Tests may panic.
- Code that reaches into a poisoned mutex.
- An unstable (nightly-only) feature.

**Tidy-ups:**

- `expect("<why this cannot fail>")` instead of `unwrap()` in non-test code.
- `if let Some(x) = opt` instead of `opt.is_some()` and then `opt.unwrap()`.
- Parameters take `&str`, `&[T]`, `&T` and `Option<&T>`, not `&String`, `&Vec<T>`, `&Box<T>` or
  `&Option<T>`.
- `.clone()` only for a real copy, not to avoid a borrow problem that a small change solves.
- No glob imports, except `use super::*` in a test module.
- An `assert!` gives context: the value that failed, or why the condition must hold.
- `#[derive(...)]` instead of a manual impl that a derive can express.
- Named fields instead of a tuple or a tuple struct with more than one field.
- A type annotation on `.parse()`, `.into()` and long iterator chains when the type is not clear.

**Optional:**

- An enum instead of a `bool` parameter whose meaning is not clear at the call site.
- A borrow or unique ownership instead of `Rc` or `Arc`, when the result stays simple.
- A standard trait (`FromStr`, `From`, `Default`, `Display`) instead of a custom method with the
  same job.
- `Default` on a struct that is a bag of options.
- A doc comment that says when a public function panics.
- `macro_rules!` before a proc macro, and code before either.

## Red CI: explain it with evidence

Do not say "CI is flaky". Find the cause, then write it down:

1. Name the failed job, the failed test and the exact error (for example `connection refused`
   at `127.0.0.1:2990`).
2. Say why it is not caused by this PR, with a fact: "This PR only changes the fixed_width
   plugin, not ftp", or "The sftp plugin does not depend on dre-core."
3. Check `master`. Look at its recent runs for the same failure, and name the commits.

   ```bash
   gh run list --branch master --limit 10
   gh run view <run-id> --log-failed
   ```

4. Give the likely cause and a possible fix, even when nobody will fix it in this PR.
5. Say what you did: "I'm re-running the failed job once." Re-run once, not in a loop.
6. If a maintainer merges with a red job, record each red job and its reason in a PR comment.

## End-to-end evidence

For a change that affects runs, the maintainer runs the demo suite with the PR's build and
posts the counts:

> dre-demo `./demo run all` with this build: 94 passed, 14 failed; all 14 are the Databricks
> scenarios (workspace refusing sessions, as on master).

Report passed, failed and skipped. Explain every failure. Prove that a failure is unrelated by
showing that the released `dre` or `master` fails the same way. Say when a scenario passed on a
rerun. If you did not run the demo, say that instead.

## Closing, superseding and holding PRs

- **Superseded:** name the PR that replaces it and what that PR does differently. "Superseded
  by #113, which moves `arrow` and `parquet` to 59 together." Then close.
- **Folded in:** "Included in #165." or "Folded into #180 (cherry-picked there)."
- **Held:** give the reason and the plan. "Held: minijinja-contrib 3 needs minijinja 3, a major
  template-engine upgrade. Planned for 0.5, not a 0.4.x patch."
- **CLA:** commits must be authored by someone the CLA check can match.

## Author mode: before you ask for review

Run the reviewer checklist on your own diff first. Then:

1. Run the local checks from CONTRIBUTING: `cargo fmt --all -- --check`,
   `cargo clippy --workspace --all-targets -- -D warnings`, and the tests of each crate you
   changed.
2. For a bug fix, revert the fix and confirm your new test fails. Restore the fix.
3. Write the PR title as `Area: what changes`, in plain words. For a long series, number the
   steps: "Typed config, step 3a: report keys and query entries typed".
4. Write the PR description with the sections that apply:

   ```markdown
   <One or two sentences: what changes and why.>

   ## Code
   - <each change, with the crate or file and the reason>

   ## What users see
   1. <each visible change; link the docs page>

   ## Versions
   - <crates, plugins, schemas and skills whose version changes>

   ## Release note
   <one short paragraph for users>

   Local: <what you ran, for example: dre-core and dre-cli tests, fmt, clippy -D warnings>.
   ```

5. Mention transitive `Cargo.lock` changes, a bundled fix, and code that came from somewhere
   else.
6. Keep commit messages in the same style: a short subject, then bullets that say what changed
   and why.

## Review template

Review body:

```markdown
Thanks @<author>, <one sentence: what the change gets right>. <What you verified: read, built,
ran which tests.>

Inline comments, by priority:

**Must fix before approval**
1. `<file>:<line>`: <the problem in one sentence>. <The fix in one sentence.>

**Tidy-ups**
2. `<file>:<line>`: <problem and fix>.

**Optional**
3. `<file>:<line>`: <idea>.

<One line on anything you accept as is, for example: "Bundling the lockfile fix in this PR is
fine.">
```

Inline comment:

```markdown
**Must fix:** <what is wrong, with the code path that shows it>. <Why it matters: input →
wrong result.>

Either:
- <simplest fix>, or
- <stronger fix>.
```

When there is nothing to fix, say so in one line, plus any optional notes: "LGTM. Thanks, the
helper boundary is clean." Mark each optional note "not blocking".

## Do not

- Do not approve a bug fix whose test passes without the fix.
- Do not post a finding without a location and a failure case.
- Do not call a failure flaky or unrelated without evidence.
- Do not block on taste. Mark it **Optional**.
- Do not restate the whole diff. Review what can go wrong.
- Do not post a review on GitHub without the user's go-ahead. Draft it and show it first.
