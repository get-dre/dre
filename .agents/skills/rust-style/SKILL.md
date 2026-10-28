---
name: rust-style
description: Check or write DRE's Rust code against the Google Rust style guide, with DRE's own rules where they differ. Sorts each rule into must fix, tidy-up or optional, maps the rules that clippy can find to lints, and gives an audit command for a crate, a diff or the whole workspace. Use when writing or reviewing Rust in this repository, when asked about Rust style, idioms, unwrap, clone, imports, panics or unsafe, or when asked to audit the codebase for style.
license: GPL-3.0-only
metadata:
  source: "Google Rust style guide, https://google.github.io/styleguide/rustguide.html (CC BY 3.0), adapted for DRE"
---

# DRE Rust style

DRE's Rust follows the [Google Rust style guide](https://google.github.io/styleguide/rustguide.html)
(the guide), which builds on the official Rust Style Guide and the Rust API Guidelines. The DRE
rules below come first; where they differ from the guide, the DRE rule wins.

Every rule, with its tier, its clippy lint and its DRE status, is in
[references/rules.md](references/rules.md). Read it when you need a rule that is not summarised
here.

## DRE rules over the guide

1. **Formatting is `cargo fmt` on stable**, with `rustfmt.toml`'s `max_width = 110`. Don't ask
   for nightly-only rustfmt options (`imports_granularity`, `group_imports`). The guide's "one
   module per `use`" rule is therefore not enforced.
2. **CI's clippy is the floor**: `cargo clippy --workspace --all-targets -- -D warnings`. A rule
   in this skill never asks for code that clippy rejects.
3. **API rules by crate.** `dre-protocol` and `dre-core` are libraries (plugins and other
   programs use them), so the API rules bind there. `dre-cli`, `dre-ssh` and `plugins/*` are
   applications: the API rules are advice, and plain, direct code beats reuse.
4. **New and changed code only.** Don't ask a PR to restyle code it doesn't touch. A style sweep
   is its own PR.
5. **Not adopted:** the rules marked `[chrome]`, `[Fuchsia]` or `[Android]` in the guide (small
   crates, flat file layout, no `async`, no threads, their test frameworks and mutex types), and
   `#[non_exhaustive]` on public enums and structs.
6. **Tests may panic.** `unwrap()`, `expect()`, `panic!` and indexing are fine in `#[cfg(test)]`
   code, `tests/` and test fixtures.

## Must fix

- **A panic on outside input.** `unwrap()`, `expect()`, indexing, slicing or `panic!` on data
  from a user, a project file, a profile, a plugin or the network. Return a `Result` with
  context. A panic is only for a bug: a broken invariant the code itself guarantees.
- **`unsafe` without a `// SAFETY:` comment** that says why the block is sound. Wrap it in a safe
  function; keep the block as small as it can be.
- **Reaching into a poisoned mutex** (`lock().unwrap_or_else(|e| e.into_inner())`,
  `PoisonError::into_inner`). A poisoned lock means a thread panicked mid-update; let the panic
  spread. An exception needs a comment that says why the data cannot be half-updated, and the
  reviewer's agreement.
- **`std::panic::catch_unwind` that the code relies on.** Allowed only as defense in depth at a
  process boundary (a plugin turning a panic into an error reply), with a comment saying so.
- **An unstable feature** (`#![feature]`), or code that runs before `main` (`ctor`).
- **Reading `#[doc(hidden)]` items** of a crate DRE doesn't own.

## Tidy-ups

- **`expect("<why this cannot fail>")` instead of `unwrap()`** in non-test code. The message says
  why the value is there, not what failed: `expect("the regex is a literal")`, not
  `expect("regex failed")`.
- **`if let` / `let ... else` instead of a check then an unwrap** (`is_some()` then `unwrap()`).
- **Borrowed parameters**: `&str`, `&[T]`, `&T`, `Option<&T>`, not `&String`, `&Vec<T>`,
  `&Box<T>`, `&Option<T>`.
- **No needless `.clone()`**: clone only for a real copy, not to dodge a borrow a small change
  fixes. Cloning an `Rc`/`Arc` is fine.
- **No glob imports** (`use foo::*`), except `use super::*` in a test module and enum variants
  in a short `match`-heavy function.
- **`assert!` with context** in non-test code: the value that failed or why it must hold. Not a
  restatement of the condition.
- **`#[derive]`** instead of a hand-written impl that a derive produces.
- **Named fields** instead of a tuple or a tuple struct with more than one field, outside a very
  local scope.
- **`format!` / `write!`** instead of chains of `+` or `push_str(&format!(..))` with more than two
  parts.
- **Types you can read**: annotate `.parse()`, `.into()`, `.collect()` and long iterator chains
  when the type isn't obvious; prefer `let x: T = ...` over a turbofish.
- **No side effects in iterator chains**: no `for_each`, no long chain in a `for` header; use a
  plain loop.
- **`#[rustfmt::skip]`** only for deliberate alignment or generated code.
- **Names**: no abbreviations an outsider wouldn't know; `as_`/`to_`/`into_` follow std's
  ownership conventions; constructors are `new`, `new_with_*`, `try_new`, `from_*`; getters have
  no `get_` prefix.

## Optional

- An enum instead of a `bool` parameter whose meaning isn't clear at the call site, or an
  `/* name= */` comment at the call.
- Unique ownership and borrows instead of `Rc`/`Arc` when the result stays simple.
- A standard trait (`FromStr`, `From`, `Display`, `Default`) instead of a custom method with the
  same job.
- `Default` on an options struct, so callers can write `..Default::default()`.
- A newtype instead of a type alias when the values mean something different (an id, a path in
  the project, a duration in seconds).
- `#[must_use]` on a library function whose result is the point of calling it, and on error types.
- Doc comments on `dre-protocol` and `dre-core` public items, with a `# Panics` section when the
  function can panic.
- `is_empty()` next to every public `len()`.
- Plain code before `macro_rules!`, and `macro_rules!` before a proc macro.

## Audit with clippy

Clippy finds part of the guide. Run the audit lints as warnings, never as a CI change, from a
scratch config so the repository stays untouched:

```bash
conf=$(mktemp -d)
printf 'allow-unwrap-in-tests = true\nallow-expect-in-tests = true\nallow-panic-in-tests = true\n' > "$conf/clippy.toml"
CLIPPY_CONF_DIR="$conf" cargo clippy -p <crate> --all-targets --message-format=short -- \
  -W clippy::unwrap_used -W clippy::panic -W clippy::undocumented_unsafe_blocks \
  -W clippy::wildcard_imports -W clippy::enum_glob_use -W clippy::redundant_clone \
  -W clippy::implicit_clone -W clippy::ref_option -W clippy::manual_let_else \
  -W clippy::format_push_string -W clippy::needless_for_each -W clippy::fn_params_excessive_bools \
  2> clippy-audit.txt
grep ': warning:' clippy-audit.txt | sed 's/.*warning: //' | sort | uniq -c | sort -rn   # hits per kind
grep ': warning:' clippy-audit.txt                                                     # each hit
```

Use `--workspace` for the whole codebase. Add `-W clippy::missing_panics_doc
-W clippy::must_use_candidate -W clippy::missing_errors_doc` for `dre-protocol` and `dre-core`
only (library rules). Then grep for what clippy can't see:

```bash
grep -rnE 'catch_unwind|into_inner\(\)|#!\[feature|rustfmt::skip' --include='*.rs' crates plugins
grep -rnE '^\s*(debug_)?assert!\([^,]*\);' --include='*.rs' crates/*/src plugins/*/src
```

A lint hit is a candidate, not a finding: read the code. `unwrap()` on a value the code just
checked, or on a literal regex, is a tidy-up; `unwrap()` on a parsed project file is a must fix.

## Reporting

In a review, give each finding its tier, `file:line`, the input that breaks it (must fix) or the
rule it breaks (tidy-up), and the fix. For a whole-codebase audit, report counts per rule and per
crate first, then the must-fix items one by one.
