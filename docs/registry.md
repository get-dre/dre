---
title: "Plugin packages, the registry and `dre.lock`"
description: "Plugin packages, the registry index and dre.lock: how DRE finds, installs and pins plugins, extensions and macro packages, and how to publish a release."
section: plugins
position: 2
---

# Plugin packages, the registry and `dre.lock`

The complete [tutorial project](../examples/tutorial/) declares its dependencies and illustrates
a generated lock after `dre deps`. The [dependency reference](reference-dependencies.md) lists
keys; the [glossary](glossary.md) defines plugin package, macro package, registry and `dre.lock`.

Plugins come in **packages**: one download, one executable, serving every plugin the package
provides. The `databricks` package is the Databricks source and the Databricks destination;
`object_store` is the `s3`, `gcs` and `azure_blob` destinations; `csv` is the `csv` and
`delimited` formats. A project declares the packages it needs, once each, under `plugins:` in
`dependencies.yml` (or any project YAML file):

```yaml
plugins:
  - duckdb
  - databricks        # the databricks source and destination
  - object_store      # s3, gcs, azure_blob
  - csv: ">=0.0.1-alpha"
```

Every plugin a declared package provides can then be used as a profile's `type:` or an output's
`format:`. One that no declared package provides is an error (`undeclared-plugin`), which names
the package to add when DRE's registry has one. DRE only ever installs the packages a project
declares; it never infers them from a `type:`.

## Companion extensions

Extensions are separate executables, not source, format or destination plugins. The registry's
optional top-level `extensions` array uses entries with `name`, `kind: "extension"`, `license`,
`proprietary_license` and `versions`. Each version has the same platform-keyed `artifacts`
structure as a plugin, with a required SHA-256 checksum. Existing plugin index readers ignore
this sibling array; plugin installation continues to read `plugins` only.

`dre extension install <name>` installs, or updates to the newest version (`<name>@<version>`
for an exact one), in `~/.dre/extensions/<name>/<version>/`; a `current` pointer selects the
installed version. Every install asks first, showing the licence the registry names, or takes
`--accept-proprietary`. The artifact must come over `https`, match its SHA-256 checksum and
carry a minisign signature from the key built into `dre`; anything else is refused.
Installation does not make an extension's source part of the GPL engine.

`dre mcp` forwards arguments to the `dre-mcp` companion, preserving its exit status.
`DRE_MCP_BIN` can point to a local build for testing. A missing companion produces installation
guidance, or an interactive install offer. DRE MCP RC binaries are distributed only through the public
`get-dre/dre` GitHub Releases, tagged `dre-mcp-v<version>`. Its proprietary source remains private.
The same registry and checksum verification used for plugins handles extension downloads.
Read the bundled EULA before accepting it; RC versions have no automatic expiry.

## The registry

DRE installs packages from a static JSON index. The index is a plain file, hosted alongside
GitHub Releases, and there's no registry service to run. The default location is:

```
https://github.com/get-dre/dre/releases/download/registry/packages.json
```

`DRE_REGISTRY_URL` points DRE somewhere else: an `https://` URL, a `file://` URL or a plain file
path. An internal mirror and an offline copy both work this way. (`index.json`, next to it, is
the index DRE 0.0.1-alpha-6 and earlier read. It lists the plugins as they were then and isn't
updated any more.)

### Index format

```json
{
  "schema": 2,
  "plugins": [
    {
      "name": "object_store",
      "description": "Amazon S3, Google Cloud Storage and Azure Blob Storage",
      "provides": ["destination/s3", "destination/gcs", "destination/azure_blob"],
      "versions": [
        {
          "version": "1.2.0",
          "protocol": 0,
          "artifacts": {
            "macos-aarch64":  { "url": "https://…/dre-plugin-object_store-1.2.0-macos-aarch64.tar.gz", "sha256": "…" },
            "linux-x86_64":   { "url": "https://…/dre-plugin-object_store-1.2.0-linux-x86_64.tar.gz",  "sha256": "…" },
            "windows-x86_64": { "url": "https://…/dre-plugin-object_store-1.2.0-windows-x86_64.exe",   "sha256": "…" }
          }
        }
      ]
    }
  ]
}
```

- `name` is the package name (`[a-z0-9_]+`), the name a project declares under `plugins:`.
- `provides` lists its plugins as `<kind>/<name>`: `kind` is `source`, `format` or
  `destination`, and `name` is the string used in `profiles.yml` `type:` or `output.format`.
- `version` is semver. A pre-release is installed when asked for explicitly
  (`dre plugin install duckdb@=1.3.0-rc.1`), or when no stable version matches: a package whose
  only releases are `0.0.1-alpha` installs that, and one with a stable `0.1.0` ignores a later
  `0.2.0-rc.1` until asked. Newest means semver order, except that numbers inside a pre-release
  label compare as numbers: `0.0.1-alpha-10` is newer than `0.0.1-alpha-9` (plain semver
  compares `alpha-10` as text and puts it first).
- `protocol` is the plugin protocol version the release speaks (see [protocol.md](protocol.md)).
  DRE skips versions it can't talk to.
- Artifacts are keyed by platform, `<os>-<arch>`, using Rust's names: `macos`, `linux` or
  `windows`, and `x86_64` or `aarch64`.
- An artifact is either the package's executable itself, or a `.tar.gz`. An archive can have the
  executable at its top level (the legacy form), or one leading version directory such as
  `jdbc/dre-plugin-jdbc` plus any runtime files beside it. The installer rejects absolute paths,
  `..`, links, duplicate entries, path conflicts and more than one package executable. It writes
  `plugin.json` and launches only the manifest's executable.
- `sha256` is the hex SHA-256 of the downloaded file. A download that doesn't match is discarded
  and the install fails. Signatures come after V1.

An index in the earlier schema 1 still works: each entry there is one plugin, with `kind` and
`name` in place of `provides`, and reads as a package of that one plugin, named after it. Its
artifacts hold `dre-<kind>-<name>`.

## Font packages

The optional top-level `fonts` array carries platform-independent fonts. Older DRE index
readers ignore this sibling and continue reading `plugins`. A font entry has `name`, `scripts`,
`license`, and `versions`. Each immutable version has `version` (semver), plus `regular`,
`bold`, and `license` artifacts, each with `url` and a required `sha256`. Fonts are raw `.otf`
or `.ttf` files, not plugin executables; the licence artifact is its complete text.

The packages and their upstream sources, all under SIL OFL 1.1 (see
[document fonts](plugin-document.md#fonts-for-other-languages)). Each version's `notice` artifact
records its upstream repository, tag, pinned commit and the unmodified upstream files.

| Package | Version | Upstream | Tag | Commit |
| --- | --- | --- | --- | --- |
| `noto_sans_jp` | 2.4.0 | `notofonts/noto-cjk` 2.004 | `Sans2.004` | `523d033d6cb47f4a80c58a35753646f5c3608a78` |
| `noto_sans_sc` | 2.4.0 | `notofonts/noto-cjk` 2.004 | `Sans2.004` | `523d033d6cb47f4a80c58a35753646f5c3608a78` |
| `noto_sans_tc` | 2.4.0 | `notofonts/noto-cjk` 2.004 | `Sans2.004` | `523d033d6cb47f4a80c58a35753646f5c3608a78` |
| `noto_sans_kr` | 2.4.0 | `notofonts/noto-cjk` 2.004 | `Sans2.004` | `523d033d6cb47f4a80c58a35753646f5c3608a78` |
| `noto_sans_arabic` | 2.13.0 | `notofonts/arabic` 2.013 | `NotoSansArabic-v2.013` | `1b2b7e5c6ce3ab4d50681c854892325530084c35` |
| `noto_sans_hebrew` | 3.1.0 | `notofonts/hebrew` 3.001 | `NotoSansHebrew-v3.001` | `036f3206f67caac235cf8546a7751d3440771a7e` |
| `noto_sans_devanagari` | 2.7.0 | `notofonts/devanagari` 2.007 | `NotoSansDevanagari-v2.007` | `e123d230c160ebe949d731cc19017cdb354180d1` |
| `noto_sans_thai` | 2.2.0 | `notofonts/thai` 2.002 | `NotoSansThai-v2.002` | `f8b482c158650260bba5d5edba9da3e8bb7185b4` |

The registry version is the upstream version as semver (2.013 is 2.13.0). The CJK files are
`Sans/OTF/<language>/NotoSansCJK<xx>-{Regular,Bold}.otf`; the others are
`full/otf/NotoSans<Script>-{Regular,Bold}.otf` from the upstream release archive. Assets on the
`registry` release are named `<file stem>-{Regular,Bold}-<upstream>.otf`,
`<stem>-<upstream>-OFL.txt` and `<stem>-<upstream>-NOTICE.txt`, for example
`NotoSansArabic-Regular-2.013.otf`.

| Asset on the `registry` release | SHA-256 |
| --- | --- |
| `NotoSansCJKjp-Regular-2.004.otf` | `68a3fc98800b2a27b371f2fb79991daf3633bd89309d4ffaa6946fd587f375b5` |
| `NotoSansCJKjp-Bold-2.004.otf` | `e53dcb0dcb2922e45d01aae1ebd2f382bb81d4229b18b6b883bd170678af1f76` |
| `NotoSansCJKjp-2.004-OFL.txt` | `6a73f9541c2de74158c0e7cf6b0a58ef774f5a780bf191f2d7ec9cc53efe2bf2` |
| `NotoSansCJKjp-2.004-NOTICE.txt` | `92c4e030be30bea02f6520302164f63f9dbfbe2ff241ec699402a866f4017d62` |
| `NotoSansCJKsc-Regular-2.004.otf` | `2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b` |
| `NotoSansCJKsc-Bold-2.004.otf` | `b5f0d1a190a7f9b43c310a8850630af12553df32c4c050543f9059732d9b4c0a` |
| `NotoSansCJKsc-2.004-OFL.txt` | `6a73f9541c2de74158c0e7cf6b0a58ef774f5a780bf191f2d7ec9cc53efe2bf2` |
| `NotoSansCJKsc-2.004-NOTICE.txt` | `9d4404d7f685b9a1f1963aa97d3e57476a9799480ecd91dfa66eea60e583e4e6` |
| `NotoSansCJKtc-Regular-2.004.otf` | `dce08bd4fd91aa8aa76ed8fea4b694c2dfb8550f67871e326843212ddbeb88b4` |
| `NotoSansCJKtc-Bold-2.004.otf` | `3ee160e5015106e3ec1a394301df54fa9bbbf8a251519984aec5c0abc50840c0` |
| `NotoSansCJKtc-2.004-OFL.txt` | `6a73f9541c2de74158c0e7cf6b0a58ef774f5a780bf191f2d7ec9cc53efe2bf2` |
| `NotoSansCJKtc-2.004-NOTICE.txt` | `9d71465ca64b88dfd8c1fd3c5611d9cd799f595fc38a251502a0b199408b79ea` |
| `NotoSansCJKkr-Regular-2.004.otf` | `6bcb2a0703aa137e874fc2dffa85f6c21ba9a67fa329e81b8c801663af7e992a` |
| `NotoSansCJKkr-Bold-2.004.otf` | `26d0c6748500a0444844280b308f5b62c7ae92ac6c6ac88148e502dd211eb52a` |
| `NotoSansCJKkr-2.004-OFL.txt` | `6a73f9541c2de74158c0e7cf6b0a58ef774f5a780bf191f2d7ec9cc53efe2bf2` |
| `NotoSansCJKkr-2.004-NOTICE.txt` | `badcb9a70e1fe9ae86297fc58012404df58c8b8c0c8be1bb0403ddc3f4225542` |
| `NotoSansArabic-Regular-2.013.otf` | `a51b3dcc5286ad3b814ecdb761ca4110fec6066056d99203dfb44e5d22946f14` |
| `NotoSansArabic-Bold-2.013.otf` | `2f780cc6365d5adfb4390fca8bf4ad11a404138a1b8088f8259a6adde27e37ac` |
| `NotoSansArabic-2.013-OFL.txt` | `a7a5a25eb188bf1cd96982030d53e23c33485c69b1044a562254226857ee13af` |
| `NotoSansArabic-2.013-NOTICE.txt` | `4c0979967e0629ef0e1f1c6a017e240193feadc9fcc207a3e5f00d758b310cdf` |
| `NotoSansHebrew-Regular-3.001.otf` | `422ad95aee46670467b2f8433bb0ab9c24ece00f14a1201a032490d9cdaaede4` |
| `NotoSansHebrew-Bold-3.001.otf` | `3cf6ce9b431d456e3a4eb3c7aded56fb278a7517705e53b3bcb890ec2c79cb97` |
| `NotoSansHebrew-3.001-OFL.txt` | `9b9fe028b5ba74d231659a1bbaf0ed09b11e759d1ca6a070999e16d151616b47` |
| `NotoSansHebrew-3.001-NOTICE.txt` | `37978fe766ff709aeaa703fbb7ff09ada62d0d2781517228f6a6a7e0c37d1e70` |
| `NotoSansDevanagari-Regular-2.007.otf` | `c58d52da54093511c4fe16d6b64ea423fb742e4f8ae7fd2f3411a25584667dea` |
| `NotoSansDevanagari-Bold-2.007.otf` | `9ccf4130234912ce4b8ab15d29a4a5bdedeb8719b6285ffaa1bd99abe9480e2a` |
| `NotoSansDevanagari-2.007-OFL.txt` | `a216f6f8d85c7228093e0ee5e258d9d377e6671f68acb4db1930b29583d0f331` |
| `NotoSansDevanagari-2.007-NOTICE.txt` | `b85d2a720511fe6156d07207ece30fddaad7c31d208589d724cee3edc02a013a` |
| `NotoSansThai-Regular-2.002.otf` | `d01313df8cd6131148f2e00c2973cff525944f0050bcd29cf566c5efc953757b` |
| `NotoSansThai-Bold-2.002.otf` | `a6cc6ae257e2c8c196a551fcd6654a7f14b3a5a2b50ad5ced329aec2cc066922` |
| `NotoSansThai-2.002-OFL.txt` | `2e98fd23a52d253db8612cd5942c8f2ff4111b21d2367050fdca91d8ccc374a0` |
| `NotoSansThai-2.002-NOTICE.txt` | `a703ed1e117c9e663ee915284138f575abbd76a2d7fc4bc1febce1b4447180e5` |

To publish a font version, upload these immutable, versioned assets to the existing `registry`
release without replacing older assets, then add its version to `packages.json` and upload the
updated index. Preserve its existing plugin, extension and font entries. No new component
release or tag is needed. Registry rebuilds preserve the `fonts` array. Mirrors can copy the
index and artifacts and replace their URLs with mirror locations, as for plugins. Downloads use
core's existing registry transport and SHA-256 verification; verified font files and their OFL
text are cached under `~/.dre/fonts/<name>/<version>/`. `dre fonts install <script>...` fills that cache ahead of
a run with each matching package's newest version, matching a script to an entry's `scripts`
case-insensitively (`ar`, `he`, `hi`, `mr`, `ne`, `th` and the `zh` region codes also name their
script); `dre fonts ls` lists the cache.

## Publishing a release

Each first-party package has its own version, independent of DRE's: `plugins/<package>/Cargo.toml`,
or `go/<package>/VERSION` for a Go package (`databricks`, `bigquery`, `snowflake`, `clickhouse`).
Compatibility between DRE and a package comes from the [protocol](protocol.md) version, never from matching numbers: DRE 0.1.0 runs
`duckdb` 1.0.0 and whatever `duckdb` releases later that speak the same protocol.

A package is released when it changes, by pushing a `<package>-v<version>` tag, e.g.
`duckdb-v1.0.1` or `object_store-v1.2.0-rc.1`. `.github/workflows/release.yml` checks the tag
against the package's version, builds that package alone for each platform, publishes it as a
GitHub Release (a pre-release when the version has a suffix such as `-rc.1`), and adds it to
`packages.json` on the `registry` release, which it creates the first time. A `v<version>` tag
releases DRE itself and touches no package. What each first-party package provides, and its
description, is in `.github/scripts/packages.json`.

The index update adds every package release it doesn't list yet, not only the one just made, so
several packages can be tagged at once. `gh workflow run release.yml -f tag=registry` runs it on
its own.

To try a release before tagging it, run the workflow as a dry run from a branch:
`gh workflow run release.yml --ref <branch> -f tag=duckdb-v1.0.1 -f dry_run=true` runs the
security scan and builds and checks everything, and publishes nothing.

For a package released some other way:

1. Build the executable for every platform and upload the artifacts to a GitHub Release.
2. Compute each artifact's checksum with `shasum -a 256 <file>`.
3. Add a `versions` entry (version, protocol, one artifact per platform) to the index.
4. Upload the updated index to your registry location, replacing the old one.

Packages are versioned independently of DRE core. A package release never needs a core release.

## Where packages are installed

A project's packages live inside it, in `dre_deps/plugins/` (gitignore `dre_deps/`), next to its
macro packages in `dre_deps/packages/`. Versions sit side by side, each with a `plugin.json`
naming the executable and the plugins it provides:

```
my_reports/dre_deps/plugins/object_store/1.2.0/dre-plugin-object_store
my_reports/dre_deps/plugins/object_store/1.2.0/plugin.json
my_reports/dre_deps/plugins/object_store/1.3.0/...
```

Downloads go to a shared cache, `~/.dre/plugins`, and are hard-linked into each project (copied
when the cache is on another disk), so ten projects using DuckDB store it once. A project whose
`dre.lock` pins a version already in the cache links it without contacting the registry.

`DRE_PLUGINS_DIR` replaces both: every project uses, and installs into, that one directory. An
executable placed by hand directly in a plugins directory is used when no installed version is
pinned or matches: `dre-plugin-<package>` (DRE asks it what it provides), or
`dre-<kind>-<name>` for a package of that one plugin, called `<name>`. This is meant for
developing a plugin.

## `dre.lock`

`dre.lock` records the exact version of every package a project resolved, what it provides, and
the checksum of its build for each platform. Commit it, so every machine and CI runner uses the
same builds, whether it's a Mac, Linux or a Databricks job.

```yaml
# Generated by DRE: the exact plugin versions and package commits this project uses. Commit this file.
plugins:
  csv:
    version: 1.0.4
    sha256:
      linux-x86_64: 51b8…
      macos-aarch64: 9a01…
    provides:
    - format/csv
    - format/delimited
  duckdb:
    version: 1.2.0
    sha256:
      linux-aarch64: 77d2…
      linux-x86_64: 0c4e…
      macos-aarch64: 3f5c…
    provides:
    - source/duckdb
packages:
  dre_utils:
    git: https://github.com/acme/dre_utils.git
    revision: v1.0.0
    commit: 4be1c0…
```

How the commands use it:

- **`dre run`, `dre validate` and `dre compile`**: install whatever the project declares but
  hasn't installed, before they start, and log each install. Nothing needs to run first.
- **`dre deps`**: installs every declared package too, and also resolves again any package the
  lock doesn't pin, so it picks up the newest version allowed. Use it to refresh `dre.lock` on
  purpose, or to install everything in a separate CI or image-building step.
- **`--no-auto-install`**: nothing is downloaded. A missing package is a hard failure for
  `dre run`, and a warning for `dre validate`; what a package provides then comes from what's
  installed and from `dre.lock`.
- **`dre plugin install <package>[@req]`**: installs one package, respecting the project's
  declared constraint and pin, and pins the result when the project declares that package.
- **`dre plugin update <package>`**: installs the newest version the constraint allows and
  re-pins it.
- **`dre plugin remove <package>[@version]`**: removes installed versions, and drops the pin if
  the pinned version is removed.
- **`dre plugin list`**: the installed packages, what each provides, and their versions.

The checksums are the ones the registry publishes for every platform of that version. When a
source publishes none (a GitHub release without `.sha256` files), each platform's first
download is recorded, and later downloads on that platform must match it. A `dre.lock` from
before per-platform checksums has a single `sha256:` value: it's accepted when it matches the
registry's checksum for any platform of that version, and rewritten in the new form.

## Other places to install from

A `plugins:` entry can name where a package comes from instead of the default registry. Bare
names and `name: "<version>"` keep working; the map form adds a source:

```yaml
plugins:
  - duckdb                                              # the default registry
  - {name: foo, github: acme/dre-foo, version: ">=1.2, <2"}
  - {name: bar, local: ../dre-bar/target/release/dre-plugin-bar}
  - {name: baz, registry: https://plugins.acme.internal/packages.json, version: "^1"}
```

- **`github: owner/repo`**: the repo's GitHub Releases. A release tagged `v1.2.0` (or `1.2.0`)
  offers version 1.2.0 for every platform it has an asset for, named like the registry's
  artifacts: `dre-plugin-<name>-<version>-<os>-<arch>.tar.gz`, or the bare executable (`.exe` on
  Windows). A single plugin's release named `dre-<kind>-<name>-<version>-<os>-<arch>` works too.
  Drafts and tags that aren't versions are skipped. DRE asks the installed executable what it
  provides. The checksum comes from a `<asset>.sha256` file in the same release; without one,
  the first download's checksum is pinned in `dre.lock` and every later download must match it.
  `GITHUB_TOKEN` is sent when set (private repos, rate limits), and `DRE_GITHUB_API_URL` points
  at GitHub Enterprise.
- **`local: <path>`**: an executable on disk, relative to the project root, used where it is and
  never copied. It has no version. This is the way to try a package you're developing.
- **`registry: <url or path>`**: another index in the [format above](#index-format), for this
  package only.

`dre.lock` records where each package came from (`from: github:acme/dre-foo`); local packages go
under `local:`. When an entry's source changes, the old pin no longer counts and the package is
resolved again. `dre plugin install` and `update` use the declared source, and `update` has
nothing to do for a local package. A package declared in several files must name the same source
in each.

Before packages, projects declared plugins one by one under `sources:`, `formats:` and
`destinations:`. Those blocks are now an error that says to use `plugins:`. (Since DRE 0.2,
`sources:` declares tables instead, in dbt's format; a list of plugin names there gets the same
error.)

## Macro packages

A macro package is a folder with a `dre_package.yml` (`name: dre_utils`) and a `macros/` folder.
Declare packages under `packages:` in `dependencies.yml` or `packages.yml` at the project root
(either file, or both):

```yaml
packages:
  - git: https://github.com/acme/dre_utils.git
    revision: v1.0.0            # a tag, branch or commit
  - local: ../shared/finance_macros
```

- **`dre deps`** (and `dre run`/`dre validate`, unless `--no-auto-install`) clones git packages
  into `dre_deps/packages/<name>` and pins the commit in `dre.lock`. A later `dre deps` reinstalls
  that exact commit until the declared `revision` changes. `local` packages are read in place.
- **Calling**: package macros are called through the package's name,
  `{{ dre_utils.star_except('customers', ['client']) }}`. The project's own macros keep plain
  names, so a local `star_except` and `dre_utils.star_except` never collide.
- **Per-database variants**: a package macro can hand over to `dispatch('name', 'dre_utils')`,
  which picks `<source type>__name` (e.g. `databricks__name`) or else `default__name`, looking in
  the project first and then the package. A project overrides a package's variant by defining
  the same `<source type>__name` or `default__name` in its own `macros/`. `dispatch:` in
  `dre_project.yml` changes the search order:

  ```yaml
  dispatch:
    - macro_namespace: dre_utils
      search_order: [dre_utils]        # ignore project overrides
  ```

  `macro_namespace` names the package whose dispatch call is being configured; `search_order`
  names the project and packages to search, first match winning. Omitting the project name
  prevents its local variants from replacing the package's implementation.

- **Trust**: package macros can query your database through `run_query()`, so install packages
  you trust; `dre.lock` makes sure you keep getting the commit you reviewed.
- Registry packages (`package: dre_utils`) aren't available yet.

`conflicting-plugin-sources` means one package is declared from different registries, GitHub
repositories or local paths. Choose one source across all project files. `conflicting-plugin-constraints`
means no release satisfies all declarations; align their ranges. `package-missing` needs
`dre deps` for a Git package or the correct path for a local package. These are stable
[error codes](reference-error-codes.md), explained by `dre explain <code>`.

<!-- docs-nav: generated by .github/scripts/docs_sections.py from docs/sections.json -->

---

**Previous:** [Managing plugins](managing-plugins.md) · **Next:** [First-party plugins](plugins.md)
