#!/usr/bin/env python3
"""Compare every shared Wasm guest from two completed component builds."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--tree', type=Path, required=True)
parser.add_argument('--evidence', type=Path, required=True)
parser.add_argument('--output', type=Path, required=True)
args = parser.parse_args()
tree = args.tree.resolve(strict=True)
evidence = args.evidence.resolve(strict=True)
out = args.output.resolve()
out.mkdir(exist_ok=False)
artifacts = {}
for profile in ('m1', 'proof'):
    captured = evidence / (profile + '-build-001')
    assert json.loads((captured / 'result.json').read_text())['exit_code'] == 0
    plan = json.loads((captured / 'stdout.log').read_text())
    assert plan['profile'] == profile
    assert all(len(leg['packages']) == 1 for leg in plan['build'])
    selected = [(leg['manifest'], leg['packages'][0]) for leg in plan['build']]
    assert len(selected) == len(set(selected))
    env = dict(os.environ, RUSTC_WRAPPER='', RUSTUP_TOOLCHAIN='1.98.0')
    env.pop('CARGO_TARGET_DIR', None)
    if profile == 'proof':
        env['CARGO_TARGET_DIR'] = str(tree / 'target/guest-proof')
    workspace_metadata = {}
    for manifest in sorted({manifest for manifest, _ in selected}):
        command = ['cargo', 'metadata', '--locked', '--offline', '--no-deps',
                   '--format-version', '1', '--manifest-path', manifest]
        process = subprocess.run(command, cwd=tree, env=env, check=True,
                                 stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        metadata = json.loads(process.stdout)
        label = profile + '-' + manifest.replace('/', '-')
        (out / (label + '.json')).write_bytes(process.stdout)
        (out / (label + '.stderr')).write_bytes(process.stderr)
        workspace_metadata[manifest] = metadata
    guests = {}
    support = []
    for manifest, package in selected:
        metadata = workspace_metadata[manifest]
        matches = [item for item in metadata['packages'] if item['name'] == package]
        assert len(matches) == 1
        targets = [item for item in matches[0]['targets'] if 'cdylib' in item['crate_types'] or 'bin' in item['kind']]
        if not targets:
            support.append(package)
            continue
        assert len(targets) == 1
        path = Path(metadata['target_directory']) / 'wasm32-wasip2/release' / (targets[0]['name'] + '.wasm')
        content = path.read_bytes()
        assert content.startswith(b'\0asm'), str(path)
        guests[package] = {'path': str(path), 'bytes': len(content),
                           'sha256': hashlib.sha256(content).hexdigest()}
    artifacts[profile] = {'guests': guests, 'support_packages': support,
                           'selected_packages': len(selected)}
    for item in plan['virtualization']['artifacts']:
        assert item['sha256'] == guests[item['package']]['sha256']
first = artifacts['m1']['guests']
second = artifacts['proof']['guests']
assert first, 'm1 must contain Wasm guests'
missing = sorted(first.keys() - second.keys())
different = [name for name in first.keys() & second.keys()
             if first[name]['sha256'] != second[name]['sha256']]
result = {'passed': not missing and not different, 'shared_guests': sorted(first),
          'missing': missing, 'different': sorted(different), 'artifacts': artifacts,
          'capture_tool_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest()}
(out / 'result.json').write_text(json.dumps(result, indent=2, sort_keys=True) + '\n')
print(json.dumps({'passed': result['passed'], 'shared_guests': len(first),
                  'missing': missing, 'different': sorted(different)}))
raise SystemExit(0 if result['passed'] else 1)
