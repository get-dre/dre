#!/usr/bin/env bash
# Smoke-test the Linux x86_64 wheel in wheels/: it installs, knows it's a pip install, holds no
# plugins (a project's are downloaded from the registry on demand), and runs a report.
set -euo pipefail
python3 -m venv /tmp/venv
/tmp/venv/bin/pip install -q wheels/*manylinux*_x86_64.whl
/tmp/venv/bin/dre --version
/tmp/venv/bin/python -m dre_cli plugin list
# The wheel's receipt makes this a pip-managed install (--check changes nothing).
/tmp/venv/bin/dre system update --check | tee /tmp/check.txt
grep -q "installed with pip" /tmp/check.txt
p="${RUNNER_TEMP:-/tmp}/smoke"
mkdir -p "$p/reports/r" && cd "$p"
printf 'name: smoke\ndefault_profile: warehouse\n' > dre_project.yml
printf 'plugins:\n  - duckdb\n  - csv\n' > dependencies.yml
printf 'connections:\n  warehouse:\n    target: dev\n    targets:\n      dev: {type: duckdb, path: ":memory:"}\n' > profiles.yml
printf "select 1 as n, 'a' as s\n" > reports/r/numbers.sql
printf 'queries: [numbers]\noutput: {format: csv}\n' > reports/r/r.yml
# Progress ("Installed  plugin package ...") goes to stderr.
/tmp/venv/bin/dre run 2>&1 | tee /tmp/run.txt
grep -q "Installed  plugin package \`duckdb\`" /tmp/run.txt
# The run's folder (DRE 0.4: one per run), from dre itself. CRLF line endings, as RFC 4180 has them.
grep -q '^1,a' "$(/tmp/venv/bin/dre history r --latest --path)/r.csv"
