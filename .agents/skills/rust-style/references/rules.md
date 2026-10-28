# Every rule in the guide, for DRE

One row per rule of the [Google Rust style guide](https://google.github.io/styleguide/rustguide.html),
in the guide's order. **Tier**: must fix, tidy-up, optional, or not adopted. **Lint**: the clippy
lint that finds it, where one exists. "Library" means the rule binds in `dre-protocol` and
`dre-core` only and is optional elsewhere.

## Background

| Rule | Tier | Lint | DRE note |
|---|---|---|---|
| Follow the Rust Style Guide and API Guidelines | tidy-up | | Where they conflict with the guide, the guide wins; DRE rules win over both. |
| API design rules bind libraries, advise applications | library | | Libraries: `dre-protocol`, `dre-core`. |

## Code organisation

| Rule | Tier | Lint | DRE note |
|---|---|---|---|
| Small crates, one file per abstraction `[chrome]` | not adopted | | |
| Flat file layout `[chrome]` | not adopted | | DRE uses module directories (`run/`, `config/`). |
| Name a leaf module after its main item (`foo` in `foo.rs`) | optional | | |
| Non-leaf modules hold only `mod` and re-exports | optional | | Small glue is fine. |
| No glob imports | tidy-up | `wildcard_imports`, `enum_glob_use` | `use super::*` in tests and split-out child modules, and enum variants inside a function, are fine. |
| One module per `use` statement | not adopted | | Needs nightly rustfmt (`imports_granularity`). |
| Import a trait in the function that uses it | optional | | Unless the file uses it a lot. |
| Call free functions through their module (`mem::take`) | optional | | |
| Import types and use them unqualified | optional | | Rename clashing ones: `io::Error as IoError`. |
| `chromium::import!` `[chrome]` | not adopted | | |

## Formatting

| Rule | Tier | Lint | DRE note |
|---|---|---|---|
| Use the codebase's formatter | must fix | `cargo fmt --check` | CI enforces it; `max_width = 110`. |
| `#[rustfmt::skip]` only for alignment or generated code | tidy-up | | |

## Ownership and borrowing

| Rule | Tier | Lint | DRE note |
|---|---|---|---|
| Avoid `Rc` and `Arc` | optional | | Reviewer's judgment. |
| Borrow instead of deep copies | tidy-up | `redundant_clone`, `implicit_clone`, `needless_pass_by_value` | Cloning an `Arc` is fine. |

## Function inputs and outputs

| Rule | Tier | Lint | DRE note |
|---|---|---|---|
| Take `&[T]`, `&str`, `&T`, `Option<&T>` | tidy-up | `ptr_arg` (default), `ref_option`, `borrowed_box` | |
| Return iterators, not `Vec`, from libraries | optional, library | | Only where callers don't need the `Vec`. |
| Take `impl IntoIterator` in libraries when the code only iterates | optional, library | | |
| Accept `impl Into<T>` | optional, library | | Almost never in applications. |
| An enum instead of a `bool` parameter | optional | `fn_params_excessive_bools` | Else an inline comment at the call. |
| `Option` for absence, `Result` for recoverable errors, `panic!` for bugs | must fix on outside input | `unwrap_used`, `panic`, `indexing_slicing` | See SKILL.md "Must fix". |

## Types

| Rule | Tier | Lint | DRE note |
|---|---|---|---|
| A newtype when values mean something distinct | optional | | |
| Named fields over tuples | tidy-up | | Single-field newtypes and local tuples are fine. |

## Traits

| Rule | Tier | Lint | DRE note |
|---|---|---|---|
| Add the trait impls users need | optional, library | | |
| A std trait instead of a custom method with its job | optional | `should_implement_trait` (default) | |
| `#[derive]` instead of a hand-written impl | tidy-up | `derivable_impls` (default) | |
| Generics over `dyn Trait` in libraries | optional, library | | Code size can justify `dyn`. |
| A trait only for generic use, `dyn` use or an extension | optional | | Not to group methods. |
| Extension traits end in `Ext` | tidy-up | | |
| Types implement the traits their fields do; error types `Copy` where possible | optional, library | | |
| A constructor next to an important `From` impl | optional | | |
| Options structs implement `Default` | optional | | |

## Concurrency

| Rule | Tier | Lint | DRE note |
|---|---|---|---|
| Never reach into a poisoned mutex | must fix | grep `into_inner()` | Exception only with a comment and the reviewer's agreement. |
| No `async` `[chrome]` | not adopted | | DRE uses tokio in plugins. |
| No std threads `[chrome]` | not adopted | | |
| `fuchsia_sync::Mutex` `[Fuchsia]` | not adopted | | |

## Macros

| Rule | Tier | Lint | DRE note |
|---|---|---|---|
| Plain code, then `macro_rules!`, then codegen, then proc macros | optional | | |

## Naming

| Rule | Tier | Lint | DRE note |
|---|---|---|---|
| `as_` / `to_` / `into_` follow std's conventions | tidy-up | `wrong_self_convention` (default) | |
| Descriptive lifetime names | optional | | `'a` is fine for one input/output link. |
| Short generic names that hint at the kind | optional | | |
| Extension traits end in `Ext` | tidy-up | | |
| `new`, `new_with_*`, `try_new`, `from_*` constructors | tidy-up | `new_ret_no_self` (default) | |
| No obscure abbreviations, no dropped letters | tidy-up | | `cfg`, `ctx`, `id`, `url` are common enough. |

## Documentation

| Rule | Tier | Lint | DRE note |
|---|---|---|---|
| Document public items (`///`, `//!`), panics and unsafe invariants | optional, library | `missing_panics_doc`, `missing_errors_doc`, `missing_safety_doc` (default) | Don't restate the obvious. |

## Other features

| Rule | Tier | Lint | DRE note |
|---|---|---|---|
| `unsafe` only when needed, wrapped, with `// SAFETY:` | must fix | `undocumented_unsafe_blocks` | |
| No unstable features | must fix | grep `#![feature` | |
| Getter/setter names: `foo`, `foo_mut`, `set_foo`, `take_foo`, `with_foo` | tidy-up | | |
| Closure accessors (`with_foo(\|f\| ..)`) | optional | | |
| `is_empty()` next to `len()` | optional, library | `len_without_is_empty` (default) | |
| Don't use another crate's `#[doc(hidden)]` items | must fix | | |
| `const fn` only when it will stay `const` | optional, library | | |
| Operator overloads only for their maths meaning | tidy-up | | |
| `#[non_exhaustive]` on public enums and structs | not adopted | | DRE matches exhaustively on purpose. |
| `#[must_use]` on functions and error types | optional, library | `must_use_candidate`, `return_self_not_must_use` | |
| No code before `main` | must fix | grep `ctor` | |

## Implementation

| Rule | Tier | Lint | DRE note |
|---|---|---|---|
| Annotate types that inference hides | tidy-up | | `.parse()`, `.into()`, `.collect()`, long chains. |
| `if let` instead of a check then an unwrap | tidy-up | `unnecessary_unwrap` (default), `manual_let_else` | |
| `expect` with a reason instead of `unwrap` | tidy-up | `unwrap_used` | Tests and `lock().unwrap()` exempt. |
| List the variants of a `non_exhaustive` enum in a `match` | optional | `match_wildcard_for_single_variants` | |
| Context strings in `assert!` | tidy-up | grep | Non-test code. |
| Short iterator chains, no `for_each`, no chains in `for` headers | tidy-up | `needless_for_each` | |
| No `catch_unwind` | must fix | grep | Defense in depth at a process boundary only, with a comment. |
| `format!` / `write!` to join more than two strings | tidy-up | | `push_str(&format!(..))` already follows it. |

## Testing

| Rule | Tier | Lint | DRE note |
|---|---|---|---|
| Per-codebase test framework `[chrome]` `[Fuchsia]` | not adopted | | DRE uses `cargo nextest`. |
