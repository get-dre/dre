---
title: "The target path"
description: "The target path: where DRE writes compiled SQL, outputs, snapshots and the project manifest, how overlapping runs are kept apart, and how to clean it."
section: schedule-and-run
position: 4
---

# The target path

The target path is the folder DRE writes its generated files to. It has nothing to do with a
run's target (`--target`, the environment every profile uses). It's `target/` in the project unless set,
highest first, by:

1. `--target-path <path>` on `compile`, `validate`, `run`, `clean` and `ls`;
2. `DRE_TARGET_PATH`;
3. `target_path:` in `dre_project.yml`.

A relative path is relative to the project root, whichever of the three sets it; `~` is your home
directory. Compiled SQL, run outputs, schema snapshots, `run_results.json` and the manifest move
together. The path must be local or mounted: object storage URLs (`s3://`, `gs://`, `abfss://`)
are refused, but a filesystem mount that supports renames works, such as a Databricks Unity
Catalog Volume (`/Volumes/...` on Databricks compute), an NFS/EFS share or a gcsfuse mount. DRE
refuses a target path that is the project root, contains the project, or sits inside `reports/`,
`macros/`, `lookups/` or `dre_deps/`. A path elsewhere inside the project is skipped when DRE
reads the project; add it to `.gitignore` (a new project's `.gitignore` covers `target/` only).

The Binding lock prefers hard-link publication and falls back to exclusive file creation
on filesystems without hard links. Test the actual mount before scheduling runs; FUSE, SMB
and other mounted filesystems vary. Local destination delivery requires hard links when
`atomic: true` combines with `if_exists: error` or `number`. There is no copy fallback for that no-replace operation.
Use a supporting filesystem, or set that local destination's `atomic: false` if you accept
writing directly to its final path.

Schema-drift detection compares each run with the snapshot the last successful run left in the
target path, so on an ephemeral runner (a job cluster, a CI runner) point it at a folder that
outlives the run:

```bash
dre run --schedule close_monthly --target-path /mnt/shared/dre/target
```

In a Databricks job, a Volume keeps the snapshots and the manifest between runs. Use a Volume
(or local disk), not Workspace files (`/Workspace/...`): those don't reliably keep the renames
behind DRE's atomic writes and run pointer, and `dre validate` warns about them.

```bash
export DRE_TARGET_PATH=/Volumes/main/reporting/dre/target
dre run --schedule close_monthly
```

## Runs, and runs that overlap

Each run of a Binding writes into its own folder, and a small `current` file says which run is
the latest that finished:

```text
target/run/<report>/<set or default>/
  current          the id of the latest finished run
  lock             only while a run is going on: its run id, host, process and start time
  runs/
    20261009T060000Z-k3f9/   one folder per run: its files and run_results.json
```

A run id is the run's UTC start time and four random characters. Each run's log, `dre.log`, is in
its folder too (other commands write to the console only). `dre history <report>` lists a
report's runs and which is current; `dre history <report> --latest --path` prints the folder
with the latest files, for scripts.

By default only the latest run is kept. To keep a history (an audit trail of exactly what was
sent, and when), set how many runs of each report and Set to keep, of any status; the current run
always stays:

```yaml
flags:
  keep_runs: 30
```

`dre run --keep-runs` and `DRE_KEEP_RUNS` override it. Older runs are removed after each run, and
`dre clean --prune` removes them without deleting anything else (`dre clean` alone deletes the
whole folder). Recommended on servers and anything scheduled; mind the size of large xlsx or
parquet outputs.

So several runs can share one target path safely:

- **The same report and Set never run twice at once.** A second `dre run` of it finds the `lock`,
  stops at once and changes nothing (no files deleted, nothing delivered, the drift snapshot left
  alone); its error names the run in progress. Different reports run in parallel freely. A lock
  left by a process on this machine that has gone (a crash, a kill, a restarted container that
  reuses the host name and process id) is taken over with a warning; one from another machine (a
  shared folder) is never guessed about. `DRE_HOST_NAME` overrides the host identity for
  containers; reuse it only for the same machine/process namespace, never unrelated hosts.
  `dre unlock <report> [--binding <set>]` shows it and
  removes it. `dre clean --prune` skips a report or Set whose lock is held.
- **Parallel runs of one project install plugins and git packages one at a time**: a later run
  waits, then uses what the first one installed.
- **A crash leaves the previous run current**, plus an unfinished run folder. Pruning removes
  that folder once it falls outside `keep_runs`; the next run does not necessarily remove it.
- **An older run never replaces a newer one.** A rerun for an earlier instant (`DRE_RUN_AT`) than
  the current run's finishes in its own folder but doesn't become current, and doesn't touch the
  drift snapshot.
- **Each file DRE reads back** (`current`, `run_results.json`, the schema snapshots, the manifest,
  `dre.lock`) is written whole: a temporary file next to it, flushed to disk, then renamed into
  place, so a run stopped mid-write leaves the previous file or the new one, never a broken one.

Before 0.4 a Binding's files were straight in `target/run/<report>/<set or default>/`.
DRE 0.12 leaves that older layout untouched; it no longer moves those files automatically.
Archive them before switching versions, and avoid sharing a target path across incompatible layouts.
See [Updating DRE](updating.md#legacy-configuration-removed-in-the-012-candidate).

## Cleaning

Inspect and prune a report's history with explicit storage and Binding names:

```bash
dre history monthly --binding domestic --output json
dre history monthly --binding domestic --latest --path
dre clean --prune --keep-runs 3
```

`--output json` prints an array with one entry per run. Each entry has
`"schema_version": "dre/history/v1"` and `results_schema_version`, the stored
`run_results.json`'s own `schema_version` (null before 0.4, or when the run never finished, whose `status` is
`unfinished`). Older runs keep their own status vocabulary, so check `results_schema_version`
before reading `status`.

`--binding` names a Set, or `default` for a report without Sets. `--latest` selects the current
finished run and `--path` prints only its directory. The `current` run may have failed: inspect
its `run_results.json` before treating its files as delivered.

For a lock from a stopped process on another machine, inspect its holder before unlocking:

```bash
dre unlock monthly --binding domestic --target-path /mnt/shared/dre/target
```

`--yes` skips the confirmation in an automated recovery procedure. Use it only after that
procedure has confirmed the holder stopped; removing a live lock permits overlapping runs.
`run-in-progress` means a lock still exists, not that the output is corrupt. `target-path-unwritable`
means permissions, a mount or disk capacity needs fixing before another run.

The [monthly finance project](../examples/monthly-finance/) provides a complete report to inspect.
See [CLI options](cli-reference.md#dre-history), [error codes](reference-error-codes.md) and the
[target path](glossary.md#target-path) and [Binding](glossary.md#binding) terms.

`dre clean` deletes the target folder only if DRE created it
(it leaves a `.dre_target` file there) or it's the project's own `target/`, so a mistyped
`--target-path` can't delete anything else.

<!-- docs-nav: generated by .github/scripts/docs_sections.py from docs/sections.json -->

---

**Previous:** [Orchestration recipe](orchestration.md) · **Next:** [Data handling and local files](data-handling.md)
