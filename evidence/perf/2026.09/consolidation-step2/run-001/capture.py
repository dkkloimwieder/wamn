#!/usr/bin/env python3
"""Capture source stability around the retained stage 2 workspace command."""
import gzip
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys
import time
from datetime import datetime, timezone

root = Path('/home/kaalin/dev/wamn')
expected = sys.argv[1]
relative = Path('docs/perf/2026.09/consolidation-step2/run-001')
evidence = root / relative
assert not evidence.exists()
head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip()
assert head == expected
paths = [os.fsdecode(path) for path in subprocess.check_output(
    ['git', 'ls-files', '-z'], cwd=root).split(b'\0')
    if path and not path.startswith(b'.beads/')]

def snapshot():
    result = {}
    for relative_path in paths:
        path = root / relative_path
        info = path.lstat()
        data = os.fsencode(os.readlink(path)) if stat.S_ISLNK(info.st_mode) else path.read_bytes()
        result[relative_path] = {'mode': oct(stat.S_IMODE(info.st_mode)),
            'kind': 'symlink' if stat.S_ISLNK(info.st_mode) else 'file',
            'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest()}
    return result

def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()

def write(name, value):
    (evidence / name).write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')

before = snapshot()
status_before = subprocess.check_output(['git', 'status', '--porcelain=v1', '--untracked-files=all'], cwd=root)
command = ['python3', 'docs/perf/2026.09/generated-tui-integration/tools/workspace.py',
           '--evidence-dir', str(relative)]
environment_names = sorted(key for key in os.environ
    if not key.startswith(('WAMN_', 'OTEL_', 'GIT_', 'PG'))
    and key not in {'DATABASE_URL', 'CARGO_TARGET_DIR'})
environment_names = sorted(set(environment_names) | {
    'KUBECONFIG', 'GIT_CONFIG_GLOBAL', 'GIT_CONFIG_NOSYSTEM', 'RUSTUP_TOOLCHAIN',
    'RUSTC_WRAPPER', 'CARGO_BUILD_JOBS', 'TMPDIR', 'HELM_CACHE_HOME',
    'HELM_CONFIG_HOME', 'HELM_DATA_HOME'})
started_utc = datetime.now(timezone.utc).isoformat()
started = time.monotonic()
result = subprocess.run(command, cwd=root)
elapsed = time.monotonic() - started
after = snapshot()
head_after = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip()
archives = []
for label, value in [('before', before), ('after', after)]:
    name = f'source-{label}.json'
    data = (json.dumps(value, indent=2, sort_keys=True) + '\n').encode()
    compressed = gzip.compress(data, mtime=0)
    assert gzip.decompress(compressed) == data
    (evidence / (name + '.gz')).write_bytes(compressed)
    archives.append({'original': name, 'original_bytes': len(data),
        'original_sha256': hashlib.sha256(data).hexdigest(), 'archive': name + '.gz',
        'archive_bytes': len(compressed), 'archive_sha256': hashlib.sha256(compressed).hexdigest()})
write('mapping-archives.json', archives)
write('launch-command.json', command)
write('environment-names.json', {
    'ambient_names': sorted(os.environ),
    'removed_ambient_names': sorted(set(os.environ) - set(environment_names)),
    'controlled_names': ['KUBECONFIG', 'GIT_CONFIG_GLOBAL', 'GIT_CONFIG_NOSYSTEM',
        'RUSTUP_TOOLCHAIN', 'RUSTC_WRAPPER', 'CARGO_BUILD_JOBS', 'TMPDIR',
        'HELM_CACHE_HOME', 'HELM_CONFIG_HOME', 'HELM_DATA_HOME'],
    'child_environment_names': environment_names,
    'explicitly_armed_names': [], 'values_recorded': False,
    'scope': 'Names from the current process and unchanged retained runner. No live inputs are armed.'})
write('run.json', {'source_head': head, 'started_utc': started_utc,
    'ended_utc': datetime.now(timezone.utc).isoformat(), 'elapsed_seconds': round(elapsed, 3),
    'exit_code': result.returncode, 'workspace_log_sha256': hashlib.sha256((evidence / 'workspace.log').read_bytes()).hexdigest()})
write('source-stability.json', {'head_before': head, 'head_after': head_after,
    'head_unchanged': head == head_after, 'tracked_files': len(paths),
    'before_sha256': digest(before), 'after_sha256': digest(after),
    'bytes_and_modes_unchanged': before == after,
    'changed_paths': [path for path in paths if before[path] != after[path]]})
(evidence / 'status-before.txt').write_bytes(status_before)
(evidence / 'status-after.txt').write_bytes(subprocess.check_output(
    ['git', 'status', '--porcelain=v1', '--untracked-files=all'], cwd=root))
(evidence / 'capture.py').write_bytes(Path(__file__).read_bytes())
assert head == head_after and before == after, 'source changed during the workspace run'
print(json.dumps({'exit_code': result.returncode, 'seconds': round(elapsed, 3),
                  'head': head, 'source_unchanged': True}), flush=True)
raise SystemExit(result.returncode)
