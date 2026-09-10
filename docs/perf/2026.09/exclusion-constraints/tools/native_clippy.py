#!/usr/bin/env python3
"""Compare native Clippy diagnostics with the issue's unmodified base."""
import argparse
from collections import Counter
import json
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--tree', required=True, type=Path)
parser.add_argument('--evidence-dir', required=True, type=Path)
args = parser.parse_args()
tree = args.tree.resolve(strict=True)
evidence = args.evidence_dir.resolve()
evidence.mkdir(parents=True, exist_ok=False)
paths = subprocess.check_output(['git', 'diff', '--name-only', 'HEAD', '-z'], cwd=tree).decode().split('\0')
paths = [path for path in paths if path and not path.startswith(('docs/', '.beads/'))]
original = {path: (tree / path).read_bytes() for path in paths}
command = ['cargo', 'clippy', '--manifest-path', 'components/Cargo.toml', '--locked', '--offline',
           '-p', 'wamn-receiving-data-access', '-p', 'wamn-client-acme-receiving-data-access',
           '-p', 'receiving', '-p', 'client-acme-receiving', '--all-targets', '--no-deps',
           '--message-format=json']


def run(name):
    subprocess.run(['python3', 'docs/perf/2026.09/effects-response/tools/capture.py',
                    '--tree', str(tree), '--evidence-dir', str(evidence / name), '--', *command],
                   cwd=tree, check=True)


def diagnostics(name):
    rows = Counter()
    for line in (evidence / name / 'command.log').read_text().splitlines():
        if not line.startswith('{'):
            continue
        entry = json.loads(line)
        if entry.get('reason') != 'compiler-message':
            continue
        message = entry['message']
        if message['level'] not in ('warning', 'error'):
            continue
        sites = []
        for span in message['spans']:
            if span['is_primary']:
                path = (tree / span['file_name']).resolve()
                sites.append((str(path.relative_to(tree)), tuple(row['text'].strip() for row in span['text'])))
        row = (message['level'], (message.get('code') or {}).get('code'), message['message'], tuple(sites))
        rows[row] += 1
    return rows

try:
    for path in paths:
        (tree / path).write_bytes(subprocess.check_output(['git', 'show', 'HEAD:' + path], cwd=tree))
    run('baseline')
finally:
    for path, content in original.items():
        (tree / path).write_bytes(content)
    assert all((tree / path).read_bytes() == content for path, content in original.items())
run('current')
baseline = diagnostics('baseline')
current = diagnostics('current')
result = {'baseline_count': sum(baseline.values()), 'current_count': sum(current.values()),
          'added': [list(key) + [count] for key, count in (current - baseline).items()],
          'removed': [list(key) + [count] for key, count in (baseline - current).items()],
          'source_restored': True}
(evidence / 'comparison.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(result))
raise SystemExit(0 if not result['added'] else 1)
