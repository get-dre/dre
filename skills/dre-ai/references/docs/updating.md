---
title: "Updating DRE"
description: "Update DRE with dre system update or your install method (Homebrew, Scoop, pip, cargo), and rehearse an update against a small report first."
section: upgrading
position: 1
---

# Updating DRE

`dre system update` checks GitHub Releases and updates DRE the way it was installed.
`dre system update --check` only reports whether a newer release exists, and
`dre system update <version>` installs that release (to pin one, or to roll back):

- **install.sh or the release zip**: `dre` downloads the new release, checks it against the
  release's `SHA256SUMS`, and replaces its own binary. A failed update leaves the old one as it
  was. Plugins aren't touched; `dre plugin update` updates those.
- **Homebrew or Scoop**: `dre` changes nothing and prints `brew upgrade dre` or `scoop update dre`.
- **pip, uv or pipx**: `dre` changes nothing and prints the command to run:
  `<python> -m pip install -U dre-cli`, `uv tool upgrade dre-cli` or `pipx upgrade dre-cli`.
  If `dre-cli` is pinned in a project's or a Databricks job's dependencies, bump the pin there.
- **cargo**: `dre` changes nothing and prints the `cargo install` command for the new version.
- **A build of your own** (`cargo build`, `cargo install --path`): `dre` refuses to overwrite it.

While every release is a pre-release, "newest" includes pre-releases. Once there are stable
releases, a stable `dre` updates to the newest stable one. Only `dre system update` checks for
new versions; no other command calls the network for it. `GITHUB_TOKEN` is sent when set, which
avoids GitHub's anonymous rate limit on shared IPs and CI runners.

## Rehearse an update against a small report

```bash
dre --version
dre system update --check
```

After applying the update command for your install method, validate and preview the complete
[tutorial project](https://github.com/get-dre/dre/blob/master/examples/tutorial/). For production work, preview your own report with
the same target and variables it normally uses. Keep the old binary or dependency pin available
until those checks pass. Read the relevant [migration notes](migrating-to-0.2.md),
[0.10](new-in-0.10.md), [0.4](new-in-0.4.md) and [0.5](new-in-0.5.md) before crossing a
minor version.

A checksum failure means the archive cannot be verified: download it again from the release;
do not disable verification. A permission failure replacing an install means that directory
needs the install method's normal permissions. A source build's refusal to self-update is
intentional; rebuild it with [source instructions](building-from-source.md).
See [CLI reference](cli-reference.md#dre-system-update),
[plugin updates](managing-plugins.md) and [plugin package](glossary.md#plugin-package).

## Legacy configuration removed in the 0.12 candidate

The 0.12 candidate removes the following pre-freeze compatibility paths. Rehearse these edits
on a copy of the project and keep its previous binary and lock file until validation passes.

| Earlier form | Required form |
| --- | --- |
| `sources:` in **profiles.yml** | Rename the section to `connections:`. Preserve each profile and target unchanged. Project source-table declarations still use `sources:`. |
| `plugins:` in a report, dre_project.yml or another project YAML file | Move all package declarations into the root `dependencies.yml` `plugins:` list. Keep constraints and source settings; repeated package constraints must still agree. |
| Root `packages.yml` | Move its `packages:`, `plugins:` and `extensions:` declarations into `dependencies.yml`, then remove the old file. Merge lists instead of creating duplicate YAML keys. |
| A scalar `sha256` in dre.lock | Replace it with a map keyed by the original artifact's platform, as below. Preserve the version, source and checksum. |
| An archive containing only an inferred `dre-<kind>-<name>` executable | Package the canonical `dre-plugin-<package>` executable, or provide an explicit `plugin.json` naming the executable and its `provides` list. |
| Protocol 0 peers | Update the custom plugin or host to protocol 1 and run the shared conformance suite. Current SDKs no longer negotiate protocol 0. |

For example, a checksum recorded for a Linux x86-64 artifact becomes:

```yaml
plugins:
  example:
    version: "1.2.3"
    sha256:
      linux-x86_64: "<the original checksum>"
```

Use the platform that produced the original artifact, from its deployment or build record;
do not guess from the machine doing the upgrade. If that platform cannot be established,
verify the artifact and its published checksum before creating the map. Keep the old lock
file for that comparison. `dre deps` cannot read a scalar checksum to perform this conversion.
After conversion it can add checksums for other platforms while preserving the pinned version.

Canonical package executables at the top of an archive remain supported, including the layout
used by DRE's own plugin releases. A manually placed development executable is separate from
an archive installed by the package manager; see [plugin discovery](protocol.md).

A new run no longer automatically moves files from the old flat Binding folder into a generated
`runs/*-legacy/` folder. Existing files remain in place; new runs use their own run folders.
Archive old outputs explicitly before cleaning the target directory.

SDK callers of Rust `Rules::legacy()` or Go `LegacyRules()` must select their intended rules
explicitly. To preserve direct writes without retries, start from the default rules, set
`atomic` to false and `retries` to zero. This does not change the default delivery rules.
See the [protocol](protocol.md) for the delivery contract.

<!-- docs-nav: generated by .github/scripts/docs_sections.py from docs/sections.json -->

---

**Previous:** [Glossary](glossary.md) · **Next:** [New in 0.10](new-in-0.10.md)
