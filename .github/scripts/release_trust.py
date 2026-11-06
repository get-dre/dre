#!/usr/bin/env python3
"""Fail closed unless a release source passed master CI and its archive is attested."""
import functools
import hashlib
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from urllib.parse import quote


def api(path):
    return json.loads(subprocess.run(['gh', 'api', path], check=True, capture_output=True, text=True).stdout)


@functools.lru_cache(maxsize=None)
def release_commit(repo, ref):
    sha = api(f'repos/{repo}/commits/{quote(ref, safe="")}')['sha']
    comparison = api(f'repos/{repo}/compare/{sha}...master')
    if comparison['status'] not in ('ahead', 'identical'):
        raise ValueError(f'{ref}: commit {sha} is not on master')
    runs = api(f'repos/{repo}/actions/workflows/ci.yml/runs?head_sha={sha}&event=push&per_page=100')['workflow_runs']
    if not any(r['head_sha'] == sha and r['head_branch'] == 'master' and r['event'] == 'push'
               and r['conclusion'] == 'success' for r in runs):
        raise ValueError(f'{ref}: no successful master CI for {sha}')
    return sha


def verify_archive(repo, tag, name, body, checksum):
    if hashlib.sha256(body).hexdigest() != checksum:
        raise ValueError(f'{tag}/{name}: checksum mismatch')
    sha = release_commit(repo, tag)
    with tempfile.TemporaryDirectory(prefix='dre-attestation-') as temporary:
        archive = Path(temporary) / 'archive'
        archive.write_bytes(body)
        subprocess.run(['gh', 'attestation', 'verify', str(archive), '--repo', repo,
                        '--signer-workflow', f'{repo}/.github/workflows/release.yml',
                        '--source-ref', f'refs/tags/{tag}', '--source-digest', sha],
                       check=True, stdout=sys.stderr)


if __name__ == '__main__':
    print(release_commit(*sys.argv[1:]))
