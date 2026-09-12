#!/usr/bin/env python3
"""Capture the assigned ordinary and live WMS startup tests once."""
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import time
from datetime import datetime, timezone

root = Path('/home/kaalin/.cache/wamn-lanes/native-c-completion-20260911')
evidence = Path(__file__).resolve().parent
result_dir = evidence.parent / 'wms-startup-live-002'
expected_head = 'fb6c2b2e28fbcb90a8a373672a60e2397488cae5'

def git(*args):
    return subprocess.check_output(['git', *args], cwd=root)

def write(name, value):
    (evidence / name).write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')

def item(path):
    info = path.lstat()
    data = os.fsencode(os.readlink(path)) if path.is_symlink() else path.read_bytes()
    return {'mode': oct(stat.S_IMODE(info.st_mode)), 'bytes': len(data),
            'sha256': hashlib.sha256(data).hexdigest(), 'kind': 'symlink' if path.is_symlink() else 'file'}

def snapshot():
    return {os.fsdecode(p): item(root / os.fsdecode(p)) for p in git('ls-files', '-z').split(b'\0')
            if p and not p.startswith(b'.beads/')}

def archive(name, value):
    data = (json.dumps(value, indent=2, sort_keys=True) + '\n').encode()
    compressed = gzip.compress(data, mtime=0)
    assert gzip.decompress(compressed) == data
    (evidence / (name + '.gz')).write_bytes(compressed)
    return {'original': name, 'original_bytes': len(data), 'original_sha256': hashlib.sha256(data).hexdigest(),
            'archive': name + '.gz', 'archive_bytes': len(compressed), 'archive_sha256': hashlib.sha256(compressed).hexdigest()}

assert not result_dir.exists()
head = git('rev-parse', 'HEAD').decode().strip()
assert head == expected_head
status = git('status', '--porcelain=v1', '--untracked-files=all')
(evidence / 'status-before.txt').write_bytes(status)
assert not status
before = snapshot()
archives = [archive('source-before.json', before)]
write('source-archives.json', archives)
environment = dict(os.environ)
overrides = {'RUSTC_WRAPPER': '', 'CARGO_BUILD_JOBS': '2',
             'CARGO_TARGET_DIR': str(root / 'target'), 'WAMN_WMS_EVIDENCE_DIR': str(result_dir)}
environment.update(overrides)
write('environment.json', {'names': sorted(environment), 'overrides': overrides,
      'execution_context': 'Host execution with explicit tool escalation for owned WMS services.'})
versions = {}
for name, argv in [('cargo', ['cargo', '+1.98.0', '--version']), ('rustc', ['rustc', '+1.98.0', '--version'])]:
    run = subprocess.run(argv, cwd=root, env=environment, capture_output=True)
    (evidence / (name + '-version.stdout')).write_bytes(run.stdout)
    (evidence / (name + '-version.stderr')).write_bytes(run.stderr)
    versions[name] = {'command': argv, 'exit_code': run.returncode}
    assert run.returncode == 0
write('versions.json', versions)
base = ['cargo', '+1.98.0', 'test', '--locked', '--offline', '-p', 'wamn-wms-tests', '--lib']
cases = [('ordinary', ['cluster::startup::tests::', '--', '--nocapture']),
         ('live', ['cluster::restarted_wms_host_retains_compiled_code_and_serves_requests',
                   '--', '--exact', '--ignored', '--nocapture'])]
records = []
for phase, args in cases:
    command = base + args
    record = {'source_commit': head, 'source_directory': str(root), 'command': command,
              'environment': overrides, 'phase': phase, 'started_utc': datetime.now(timezone.utc).isoformat()}
    write(phase + '-command.json', record)
    started = time.monotonic()
    log = evidence / (phase + '-cargo.log')
    with log.open('wb') as stream:
        result = subprocess.run(command, cwd=root, env=environment, stdout=stream, stderr=subprocess.STDOUT)
    record.update({'elapsed_seconds': time.monotonic() - started, 'exit_code': result.returncode,
                   'ended_utc': datetime.now(timezone.utc).isoformat(), 'log': log.name,
                   'log_sha256': item(log)['sha256']})
    matches = re.findall(r'Running unittests .*?\(([^)]+)\)', log.read_text(errors='replace'))
    binaries = []
    for name in sorted(set(matches)):
        binary = Path(name)
        if not binary.is_absolute():
            binary = root / binary
        if binary.is_file():
            binaries.append({'path': str(binary), **item(binary)})
    record['test_binaries'] = binaries
    after = snapshot()
    after_head = git('rev-parse', 'HEAD').decode().strip()
    archives.append(archive('source-after-' + phase + '.json', after))
    write('source-archives.json', archives)
    (evidence / ('status-after-' + phase + '.txt')).write_bytes(git('status', '--porcelain=v1', '--untracked-files=all'))
    changed = [name for name in sorted(before.keys() | after.keys()) if before.get(name) != after.get(name)]
    record['source_unchanged'] = head == after_head and before == after
    write('source-stability-' + phase + '.json', {'head_before': head, 'head_after': after_head,
          'tracked_files': len(before), 'changed_paths': changed, 'source_unchanged': record['source_unchanged']})
    write(phase + '-record.json', record)
    records.append(record)
    write('results.json', records)
    print(json.dumps(record), flush=True)
    assert record['source_unchanged']
    if result.returncode:
        raise SystemExit(result.returncode)
