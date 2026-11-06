#!/usr/bin/env python3
"""First-party CLA status bot. Reads PR metadata only; never checks out PR code."""
import base64
import json
import os
from pathlib import Path
import subprocess

SIGN = 'I have read the CLA Document and I hereby sign the CLA'
ALLOW = {'allenhori', 'dependabot[bot]', 'github-actions[bot]'}
MARKER = '<!-- dre-cla -->'
SIGNATURES = 'contents/signatures/cla.json'


def api(path, method='GET', body=None, paginate=False):
    command = ['gh', 'api', path, '--method', method]
    if paginate:
        command += ['--paginate', '--slurp']
    if body is not None:
        command += ['--input', '-']
    result = subprocess.run(command, input=json.dumps(body) if body is not None else None,
                            capture_output=True, text=True, check=True)
    return json.loads(result.stdout)


def signatures(repo):
    file = api(f'repos/{repo}/{SIGNATURES}?ref=cla-signatures')
    return file, json.loads(base64.b64decode(file['content']))


def save_signature(repo, signature):
    for attempt in range(3):
        file, data = signatures(repo)
        if any(s['id'] == signature['id'] for s in data['signedContributors']):
            return
        data['signedContributors'].append(signature)
        try:
            api(f'repos/{repo}/{SIGNATURES}', 'PUT', {
                'message': f"CLA signature: {signature['name']}", 'branch': 'cla-signatures',
                'sha': file['sha'], 'content': base64.b64encode(json.dumps(data, indent=2).encode()).decode()})
            return
        except subprocess.CalledProcessError:
            if attempt == 2:
                raise


def sign_comment(repo, number, event):
    comment = event.get('comment', {})
    if comment.get('body') == SIGN:
        user = comment['user']
        save_signature(repo, {'id': user['id'], 'name': user['login'], 'comment_id': comment['id'],
                              'created_at': comment['created_at'], 'pullRequestNo': number})


def unsigned(commits, signed):
    ids = {s['id'] for s in signed}
    missing = set()
    for commit in commits:
        author = commit['author']
        if author is None:
            missing.add('unlinked commit author')
        elif author['login'] not in ALLOW and author['id'] not in ids:
            missing.add(author['login'])
    return sorted(missing)


def main():
    event = json.loads(Path(os.environ['GITHUB_EVENT_PATH']).read_text())
    repo = os.environ['GITHUB_REPOSITORY']
    number = event.get('number') or event['issue']['number']
    pr = api(f'repos/{repo}/pulls/{number}')
    if pr['state'] != 'open':
        return
    sign_comment(repo, number, event)
    _, data = signatures(repo)
    pages = api(f'repos/{repo}/pulls/{number}/commits?per_page=100', paginate=True)
    missing = unsigned([commit for page in pages for commit in page], data['signedContributors'])
    if missing:
        comments = api(f'repos/{repo}/issues/{number}/comments?per_page=100', paginate=True)
        if not any(MARKER in c['body'] and c['user']['login'] == 'github-actions[bot]'
                   for page in comments for c in page):
            api(f'repos/{repo}/issues/{number}/comments', 'POST', {'body':
                f'{MARKER}\nPlease read [CLA.md](https://github.com/{repo}/blob/master/CLA.md) '
                f'and post this exact sentence to sign:\n\n{SIGN}\n\n'
                'Every commit author must sign. Link unlinked commit emails to a GitHub account, '
                'then comment `recheck`.'})
    api(f"repos/{repo}/statuses/{pr['head']['sha']}", 'POST', {
        'context': 'license/cla', 'state': 'failure' if missing else 'success',
        'description': ('Missing CLA: ' + ', '.join(missing))[:140] if missing else 'All commit authors signed the CLA',
        'target_url': f'https://github.com/{repo}/pull/{number}'})


if __name__ == '__main__':
    main()
