---
title: "The target path"
description: "Where DRE writes compiled SQL, outputs, snapshots and the manifest."
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
  left by a process on this machine that has gone (a crash, a kill) is taken over with a warning;
  one from another machine (a shared folder) is never guessed about: `dre unlock <report>
  [--binding <set>]` shows it and removes it.
- **A crash leaves the previous run current**, and only an unfinished folder behind, which the
  next run removes.
- **An older run never replaces a newer one.** A rerun for an earlier instant (`DRE_RUN_AT`) than
  the current run's finishes in its own folder but doesn't become current, and doesn't touch the
  drift snapshot.
- **Each file DRE reads back** (`current`, `run_results.json`, the schema snapshots, the manifest,
  `dre.lock`) is written whole: a temporary file next to it, flushed to disk, then renamed into
  place, so a run stopped mid-write leaves the previous file or the new one, never a broken one.

Before 0.4 a Binding's files were straight in `target/run/<report>/<set or default>/`. The first
0.4 run moves them into a `runs/<time>-legacy/` folder (and the drift snapshot carries on). Don't
run 0.3 and 0.4 on one target path.

## Cleaning

Inspect and prune a report's history with explicit storage and Binding names:

```bash
dre history monthly --binding domestic --output json
dre history monthly --binding domestic --latest --path
dre clean --prune --keep-runs 3
```

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

The [monthly finance project](https://github.com/get-dre/dre/blob/master/examples/monthly-finance/) provides a complete report to inspect.
See [CLI options](cli-reference.md#dre-history), [error codes](reference-error-codes.md) and the
[target path](glossary.md#target-path) and [Binding](glossary.md#binding) terms.

`dre clean` deletes the target folder only if DRE created it
(it leaves a `.dre_target` file there) or it's the project's own `target/`, so a mistyped
`--target-path` can't delete anything else.

<!-- docs-nav: generated by .github/scripts/docs_sections.py from docs/sections.json -->

---

**Previous:** [Orchestration recipe](orchestration.md) · **Next:** [Managing plugins](managing-plugins.md)
