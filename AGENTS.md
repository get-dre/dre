# AGENTS.md

Instructions for coding agents and human contributors who work on this repository. The
general rules are in [CONTRIBUTING.md](CONTRIBUTING.md). Read it first.

## Reviews: follow the allenhori-review skill

Every pull request is reviewed to the maintainer's standard. That standard is written down as an
[Agent Skill](https://agentskills.io):
[`.agents/skills/allenhori-review/SKILL.md`](.agents/skills/allenhori-review/SKILL.md).

- **Contributors:** before you open a pull request or ask for a review, run the skill's
  *author mode* on your branch. Fix what it finds. Write the PR description in its format.
- **Reviewers:** use the skill's *reviewer mode*. Rank findings as **Must fix before
  approval**, **Tidy-ups** and **Optional**. Give each one a `file:line`, a concrete failure
  case and a fix. Do not approve a bug fix whose test passes without the fix.
- **Rust style:** Rust code follows the
  [Google Rust style guide](https://google.github.io/styleguide/rustguide.html), with the DRE
  rules in the skill's "Rust style" section. `rustfmt.toml` and CI's clippy still decide
  formatting and lints.
- **Red CI:** explain each failure with evidence (the test, the error, the same failure on
  `master`) before you call it unrelated.

How agents find the skill:

- Agents that read `.agents/skills/` (Codex, Gemini CLI, Cursor, GitHub Copilot and others)
  find it there.
- Claude Code finds it through the link at `.claude/skills/allenhori-review`.
- Any other agent: read `.agents/skills/allenhori-review/SKILL.md` before a review.

This skill is for work on DRE itself. It is not one of the user-facing skills in
[`skills/`](skills/README.md), and it is not shipped to DRE users.
