# Contributing to DRE

Thanks for helping. Bug reports, ideas and pull requests are all welcome.

## Before you start

- **Bugs and ideas:** open an issue first. For a bug, include the `dre --version` output, your
  OS, the plugin and its version, and the smallest project that shows the problem.
- **Larger changes** (a new feature, a new plugin, a change to the plugin protocol or to project
  YAML): open an issue to discuss it before writing the code, so we agree on the shape first.
- **Security problems:** don't open a public issue. Report them privately through the
  repository's **Security → Report a vulnerability** tab.

## How changes get in

`master` is protected. Nobody pushes to it directly:

1. Fork the repository and create a branch from `master`.
2. Make your change, with tests (see below).
3. Open a pull request against `master` and describe what it changes and why.
4. Sign the [Contributor License Agreement](CLA.md) (a bot asks you on your first pull request;
   see below).
5. CI must pass, and a maintainer must approve the pull request. New commits after an approval
   need a fresh one, and every review conversation must be resolved.
6. A maintainer merges it, squashing it into a single commit.

## Contributor License Agreement

DRE is released under the GPL-3.0, and is also offered under a commercial license. To be able
to do both with your contribution, we need you to sign the [CLA](CLA.md) once. You keep the
copyright in your work.

On your first pull request, the CLA check fails and the CLA bot posts a comment. To sign, post
this sentence as a new comment on the pull request, exactly as written and with nothing else in
the comment:

```text
I have read the CLA Document and I hereby sign the CLA
```

The bot records your signature and the check turns green. If it stays red, post a comment
containing only `recheck`. You sign once; later pull requests pass without it. Pull requests
can't be merged until every author has signed.

## Building and testing

You need a recent stable Rust toolchain, and Go for the Go packages in `go/` (`databricks`,
`bigquery`, `snowflake`, and the protocol module they share, `go/plugin`).

```sh
cargo build --workspace --bins
cargo test --workspace
```

Before you push, run the same checks as CI:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
gofmt -l go                              # prints nothing when the Go code is formatted
for m in go/*/; do (cd "$m" && go vet ./... && go test ./...); done
```

The plugin integration tests (Postgres, object store, SFTP, FTP, email) run against local
emulators, and each one skips itself when its service isn't configured. See the `integration`
job in [`.github/workflows/ci.yml`](.github/workflows/ci.yml) for the services and environment
variables they use.

## Security scans

The `Security scan` workflow ([`.github/workflows/security.yml`](.github/workflows/security.yml))
checks every Rust crate and Go module DRE depends on for known vulnerabilities and for licences
that aren't compatible with DRE's GPL-3.0-only. It runs on every pull request, before every release
and weekly. To run it locally (Python 3.11 or later):

```sh
cargo install --locked cargo-deny
go install golang.org/x/vuln/cmd/govulncheck@latest
go install github.com/google/go-licenses/v2@latest
python3 .github/scripts/security_scan.py rust
python3 .github/scripts/security_scan.py go
```

`rust` runs `cargo deny` with [`deny.toml`](deny.toml); `go` runs `govulncheck` and `go-licenses`
in each module under `go/`.

The same workflow checks the workflows themselves with [zizmor](https://docs.zizmor.sh/)
(`pipx run zizmor .github/workflows`). Every third-party action is pinned by its full commit SHA,
with the version as a comment (`uses: actions/checkout@<sha> # v7.0.1`), and Dependabot updates
the pins. Every container image is pinned by digest (`postgres:17-alpine@sha256:…`), and the
weekly `Image digests` workflow proposes new digests. Each job asks only for the permissions it
needs. Findings accepted on purpose are listed in [`.github/zizmor.yml`](.github/zizmor.yml), each
with its reason. In CI, the JSON reports are kept as the run's `security-report-*`
artifacts.

### When the security scan fails

- **A vulnerability** (RUSTSEC-… or GO-…): update the dependency to a fixed version
  (`cargo update -p <crate>`, or `go get <module>@<version>` and `go mod tidy`), and bump the
  version of every plugin whose dependencies changed. A Go standard-library advisory is fixed by
  the newest Go patch release, which CI and the release builds use.
- **No fix exists yet**, and DRE isn't exposed or the risk is acceptable for now: add an entry to
  [`.github/vulnerability-exceptions.toml`](.github/vulnerability-exceptions.toml) with the exact
  advisory id, why it's acceptable, an owner and an expiry date (a few months out, never open
  ended). Never exempt a whole crate, module or severity.
- **An exception expired:** check the advisory again. Remove the entry once a fix is in, or renew
  it with an up-to-date reason and a new date.
- **A yanked crate:** `cargo update -p <crate>`.
- **An unmaintained crate** only warns. Plan to replace it.
- **A licence that isn't allowed:** use another dependency. If its licence is in fact compatible
  with GPL-3.0-only, add it to `licenses.allow` in `deny.toml` (both languages use that list), or,
  for a single crate or module, add an exception (`licenses.exceptions` in `deny.toml`, or
  [`.github/go-license-exceptions.toml`](.github/go-license-exceptions.toml)) with the reason.

## Guidelines

- **Keep pull requests focused:** one change per pull request.
- **Tests:** a bug fix comes with a test that fails without it; a feature comes with tests
  for it.
- **Docs:** update `README.md` or `docs/` when you change behaviour users can see. Practices
  and opinions go in [`docs/practices.md`](docs/practices.md). A new page names its `section` and
  `position` in its front matter (the sections are in [`docs/sections.json`](docs/sections.json));
  then run `python3 .github/scripts/docs_sections.py sync`, which updates
  [`docs/README.md`](docs/README.md) and the previous/next links. Generated pages say so at the
  top (the YAML, CLI and error codes references): change their source, not the page.
- **Plugins** are versioned on their own. If you change a plugin's code, bump the `version` in
  that plugin's `Cargo.toml` (CI warns when you forget).
- **Compatibility:** a patch release (0.2.x) never breaks a project. A minor release (0.2 → 0.3)
  may, since DRE is pre-1.0 and fixing a design properly wins over keeping compatibility layers.
  Every break goes in the release notes and the migration guide, and a removed key or name is an
  error that says what to write instead, never silently ignored. Renamed keys may keep working,
  with a warning, where that costs little. The one exception so far is 0.2.1, which corrected
  0.2.0's targets (see the migration guide).
- **Agent skills** in `skills/`: edit the sources, never the generated copies (the
  `references/` folders and the `shared/` blocks in `SKILL.md`), then run
  `.github/scripts/skills.py sync` and `generate`. See [`skills/README.md`](skills/README.md).
- **Your own work only:** submit only code you wrote, or code whose license allows it to be
  included here (say where it came from in the pull request). Don't copy code from other
  projects with incompatible licenses.

## Code of conduct

Be respectful and constructive. Maintainers may remove comments, or block people, who aren't.
