#!/usr/bin/env python3
"""Capture the assigned WMS terminal test once."""
import gzip
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import time
from datetime import datetime, timezone

root = Path('/home/kaalin/dev/wamn')
evidence = Path(__file__).resolve().parent
result_dir = root / 'docs/perf/2026.09/consolidation-step3/pty-wms-001'
binary = root / 'target/debug/examples/wms_move'
expected_head = '47391bbc237f2e760f5cddb8b2e8f54a417ccc1f'
expected_binary = '7050209feeea17d8149c0a96c8632472ae45177f009e37d3104dfb78e9746d42'

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
binary_before = item(binary)
assert binary_before['sha256'] == expected_binary and binary_before['bytes'] == 55967408
before = snapshot()
(evidence / 'status-before.txt').write_bytes(git('status', '--porcelain=v1', '--untracked-files=all'))
command = ['python3', '-B', 'docs/perf/2026.09/generated-tui-wms/tools/terminal_preflight.py',
           '--tree', str(root), '--binary', str(binary), '--evidence-dir', str(result_dir)]
write('command.json', command)
write('environment-names.json', {'names': sorted(os.environ), 'values_recorded': False,
      'execution_context': 'Host execution with explicit tool escalation for the local PTY and loopback HTTP sockets.'})
write('binary-before.json', {**binary_before, 'path': str(binary), 'device': binary.stat().st_dev,
      'inode': binary.stat().st_ino, 'exact_build_commit': None,
      'limit': 'The exact build commit was not recorded. The run uses the existing binary and records its actual bytes.'})
depfile = root / 'target/debug/examples/wms_move.d'
(evidence / 'wms_move.d').write_bytes(depfile.read_bytes())
local_paths = [Path(p).resolve().relative_to(root).as_posix() for p in depfile.read_text().partition(': ')[2].split()]
local_rows=[]
for path in local_paths:
    current=(root/path).read_bytes()
    previous=git('show', 'd37f7387:'+path)
    final=git('show', '8671f63c:'+path)
    local_rows.append({'path':path,'sha256':hashlib.sha256(current).hexdigest(),
                       'unchanged_d37f7387_to_8671f63c_and_current':previous==final==current})
assert len(local_rows)==34 and all(row['unchanged_d37f7387_to_8671f63c_and_current'] for row in local_rows)
write('binary-source-comparison.json', {'depfile_sha256':hashlib.sha256(depfile.read_bytes()).hexdigest(),
      'local_source_count':len(local_rows),'sources':local_rows,
      'limit':'Dependency-file source equality does not establish the binary exact build commit.'})
started_utc=datetime.now(timezone.utc).isoformat()
started=time.monotonic()
with (evidence/'stdout').open('wb') as stdout, (evidence/'stderr').open('wb') as stderr:
    result=subprocess.run(command,cwd=root,stdout=stdout,stderr=stderr)
elapsed=time.monotonic()-started
ended_utc=datetime.now(timezone.utc).isoformat()
after=snapshot()
head_after=git('rev-parse','HEAD').decode().strip()
binary_after=item(binary)
(evidence/'status-after.txt').write_bytes(git('status','--porcelain=v1','--untracked-files=all'))
write('mapping-archives.json',[archive('source-before.json',before),archive('source-after.json',after)])
write('binary-after.json',binary_after)
changed=[p for p in sorted(before.keys()|after.keys()) if before.get(p)!=after.get(p)]
write('source-stability.json',{'head_before':head,'head_after':head_after,'head_unchanged':head==head_after,
      'tracked_files':len(before),'changed_paths':changed,'bytes_and_modes_unchanged':before==after,
      'binary_unchanged':binary_before==binary_after})
record={'source_head':head,'started_utc':started_utc,'ended_utc':ended_utc,
        'elapsed_seconds':elapsed,'exit_code':result.returncode,'stdout_sha256':item(evidence/'stdout')['sha256'],
        'stderr_sha256':item(evidence/'stderr')['sha256'],'binary_sha256':binary_before['sha256'],
        'source_unchanged':head==head_after and before==after,'binary_unchanged':binary_before==binary_after}
write('record.json',record)
(evidence/'exit-code.txt').write_text(str(result.returncode)+'\n')
assert record['source_unchanged'] and record['binary_unchanged']
print(json.dumps(record),flush=True)
raise SystemExit(result.returncode)
