#!/usr/bin/env python3
"""Record the generator build for this app directory move."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--tree', type=Path, required=True)
parser.add_argument('--evidence-dir', type=Path, required=True)
args = parser.parse_args()
tree = args.tree.resolve(strict=True)
evidence = args.evidence_dir.resolve()
evidence.mkdir(parents=True, exist_ok=False)
command = ['cargo', 'build', '--locked', '--offline', '-p',
           'wamn-schema-generator', '--example', 'materialize_package']

def git(*arguments):
    return subprocess.check_output(['git', *arguments], cwd=tree)

def write_json(name, value):
    (evidence / name).write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')

def source():
    paths = git('ls-files', '-co', '--exclude-standard', '-z').decode().split('\0')
    files = {}
    for name in sorted(set(paths) - {''}):
        if name.startswith(('.beads/', 'docs/')):
            continue
        path = tree / name
        files[name] = hashlib.sha256(path.read_bytes()).hexdigest() if path.is_file() else None
    return {'head': git('rev-parse', 'HEAD').decode().strip(), 'files': files}

env = {key: value for key, value in os.environ.items()
       if not key.startswith(('WAMN_', 'OTEL_', 'GIT_', 'PG'))
       and key not in {'DATABASE_URL', 'CARGO_TARGET_DIR', 'CARGO', 'RUSTC_WORKSPACE_WRAPPER'}}
env.update(RUSTC_WRAPPER='', CARGO_BUILD_JOBS='2', KUBECONFIG='/dev/null',
           GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_NOSYSTEM='1')
before = source()
write_json('source-before.json', before)
write_json('command.json', {'cwd': str(tree), 'argv': command,
                           'environment': {'RUSTC_WRAPPER': '', 'CARGO_BUILD_JOBS': '2',
                                           'CARGO_TARGET_DIR': None, 'KUBECONFIG': '/dev/null'},
                           'default_target': str(tree / 'target'),
                           'capture_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest()})
started = time.monotonic()
with (evidence / 'output.log').open('w') as output:
    completed = subprocess.run(command, cwd=tree, env=env, stdout=output,
                               stderr=subprocess.STDOUT)
after = source()
write_json('source-after.json', after)
binary = tree / 'target/debug/examples/materialize_package'
result = {'exit_code': completed.returncode, 'elapsed_seconds': round(time.monotonic() - started, 3),
          'source_unchanged': before == after, 'binary': str(binary)}
if completed.returncode == 0:
    result['binary_sha256'] = hashlib.sha256(binary.read_bytes()).hexdigest()
    result['binary_bytes'] = binary.stat().st_size
write_json('result.json', result)
print(json.dumps(result, sort_keys=True), flush=True)
raise SystemExit(completed.returncode)
