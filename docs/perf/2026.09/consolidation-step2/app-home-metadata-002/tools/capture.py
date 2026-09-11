import hashlib
import gzip
import json
import os
from pathlib import Path
import subprocess
import time

before_root = Path('/home/kaalin/dev/wamn')
after_root = Path('/home/kaalin/.cache/wamn-lanes/consolidation-apps-final-20260911')
output = Path(__file__).resolve().parent.parent
moves = json.loads((output / 'path-mapping.json').read_text())
moves.sort(key=lambda pair: len(pair[0]), reverse=True)
(output / 'path-mapping.json').write_text(json.dumps(moves, indent=2) + '\n')
environment = dict(os.environ)
environment.pop('CARGO_TARGET_DIR', None)
environment['RUSTC_WRAPPER'] = ''
records = []
results = []

def canonical(value, root, old, ids):
    if isinstance(value, str):
        value = ids.get(value, value).replace(str(root), '/repository')
        if old:
            for source, destination in moves:
                prefix = '/repository/' + source
                if prefix in value:
                    value = value.replace(prefix, '/repository/' + destination)
                    break
        if value.startswith('/repository/') and '#' not in value:
            value = os.path.normpath(value)
        return value
    if isinstance(value, list):
        return sorted((canonical(item, root, old, ids) for item in value), key=lambda item: json.dumps(item, sort_keys=True))
    if isinstance(value, dict):
        return {key: canonical(item, root, old, ids) for key, item in value.items()}
    return value

for label, old_manifest, new_manifest in [
    ('native', 'Cargo.toml', 'Cargo.toml'),
    ('std', 'components/Cargo.toml', 'apps/Cargo.toml'),
    ('no-std', 'components/no-std/Cargo.toml', 'apps/platform/no-std/Cargo.toml'),
]:
    captures = []
    for side, root, manifest in [('before', before_root, old_manifest), ('after', after_root, new_manifest)]:
        command = ['cargo', 'metadata', '--locked', '--offline', '--format-version', '1', '--manifest-path', manifest]
        start = time.monotonic()
        run = subprocess.run(command, cwd=root, env=environment, capture_output=True, timeout=120)
        (output / f'{label}-{side}.json.gz').write_bytes(gzip.compress(run.stdout, mtime=0))
        (output / f'{label}-{side}.stderr').write_bytes(run.stderr)
        records.append({'workspace': label, 'side': side, 'cwd': str(root), 'command': command, 'exit': run.returncode, 'seconds': round(time.monotonic() - start, 3), 'stdout_sha256': hashlib.sha256(run.stdout).hexdigest()})
        (output / 'commands.json').write_text(json.dumps(records, indent=2) + '\n')
        if run.returncode:
            raise SystemExit(f'{label} {side} metadata failed: {run.returncode}')
        captures.append(json.loads(run.stdout))
    before, after = captures
    key = lambda package: (package['name'], package['version'], package['source'])
    old_packages = {key(package): package for package in before['packages']}
    new_packages = {key(package): package for package in after['packages']}
    assert old_packages.keys() == new_packages.keys(), label
    ids = {package['id']: new_packages[identity]['id'].replace(str(after_root), str(before_root)) for identity, package in old_packages.items()}
    normalized_before = canonical(before, before_root, True, ids)
    normalized_after = canonical(after, after_root, False, {})
    # A mapped Cargo package ID already contains the final path.
    for suffix, normalized in [('before', normalized_before), ('after', normalized_after)]:
        data = (json.dumps(normalized, indent=2, sort_keys=True) + '\n').encode()
        (output / f'{label}-normalized-{suffix}.json.gz').write_bytes(gzip.compress(data, mtime=0))
    assert normalized_before == normalized_after, f'{label} metadata differs beyond paths'
    local = [package for package in after['packages'] if package['source'] is None]
    if label != 'native':
        workspace = Path(after['workspace_root'])
        for package in local:
            assert Path(package['manifest_path']).is_relative_to(workspace)
            for dependency in package['dependencies']:
                if dependency.get('path'):
                    assert Path(dependency['path']).is_relative_to(workspace)
    results.append({'workspace': label, 'members': len(after['workspace_members']), 'local_packages': len(local), 'metadata_equal_after_path_mapping': True, 'local_guest_dependencies_inside_workspace': True if label != 'native' else None})

(output / 'results.json').write_text(json.dumps({'base': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=after_root, text=True).strip(), 'workspaces': results, 'guest_builds_run': False}, indent=2) + '\n')
print(json.dumps(results, indent=2))
