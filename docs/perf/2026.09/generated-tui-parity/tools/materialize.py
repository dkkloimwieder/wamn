#!/usr/bin/env python3
"""Regenerate package output against fresh PostgreSQL databases and retain receipts."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time
import uuid

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--tree', type=Path, required=True)
parser.add_argument('--evidence-dir', type=Path, required=True)
args = parser.parse_args()
tree = args.tree.resolve(strict=True)
evidence = args.evidence_dir.resolve()
evidence.mkdir(parents=True, exist_ok=False)
container = 'wamn-tui-parity-materialize-' + uuid.uuid4().hex[:12]
packages = {
    'receiving': ['receiving'],
    'client_acme_receiving': ['receiving', 'client_acme_receiving'],
    'wms': ['wms'],
}
env = os.environ.copy()
env.pop('CARGO_TARGET_DIR', None)
env.update(RUSTC_WRAPPER='', PGPASSWORD='probe')
result = {'container': container, 'packages': {}, 'passed': False, 'cleanup_complete': False}
commands = []

def run(command, name, *, environment=env, input_text=None):
    commands.append({'name': name, 'argv': [str(value) for value in command]})
    with (evidence / (name + '.log')).open('w') as output:
        process = subprocess.run(command, cwd=tree, env=environment, input=input_text,
                                 text=True, stdout=output, stderr=subprocess.STDOUT)
    if process.returncode:
        raise RuntimeError(f'{name} exited {process.returncode}')

def fingerprints():
    return {str(path.relative_to(tree)): hashlib.sha256(path.read_bytes()).hexdigest()
            for package in packages
            for path in sorted((tree / 'packages' / package / 'generated').rglob('*'))
            if path.is_file()}

before = fingerprints()
started = time.monotonic()
try:
    run(['docker', 'run', '--detach', '--name', container, '-e', 'POSTGRES_PASSWORD=probe',
         '-p', '127.0.0.1::5432', 'postgres:18'], 'postgres-start')
    port = subprocess.check_output(['docker', 'port', container, '5432/tcp'], text=True).strip().rsplit(':', 1)[1]
    deadline = time.monotonic() + 60
    while subprocess.run(['docker', 'exec', container, 'pg_isready', '-U', 'postgres'],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
        if time.monotonic() >= deadline:
            raise RuntimeError('PostgreSQL startup timed out')
        time.sleep(0.2)
    for package, bases in packages.items():
        database = 'wamn_tui_' + package
        url = f'postgresql://postgres:probe@127.0.0.1:{port}/{database}'
        run(['docker', 'exec', container, 'createdb', '-U', 'postgres', database], package + '-create')
        run(['psql', url, '-X', '-v', 'ON_ERROR_STOP=1', '-c', 'CREATE SCHEMA ' + bases[0]], package + '-schema')
        migrations = [path for base in bases for path in sorted((tree / 'packages' / base / 'migrations').glob('*.sql'))]
        for index, migration in enumerate(migrations):
            run(['psql', url, '-X', '-v', 'ON_ERROR_STOP=1', '-f', migration], f'{package}-migrate-{index}')
        for mode in ['write', 'check']:
            run(['cargo', 'run', '--locked', '--offline', '-p', 'wamn-schema-generator',
                 '--example', 'materialize_package', '--', mode, 'packages/' + package],
                package + '-' + mode, environment=env | {'WAMN_SCHEMA_INTROSPECTION_PG_URL': url})
        result['packages'][package] = {'migrations': [str(p.relative_to(tree)) for p in migrations],
                                       'completed_modes': ['write', 'check']}
    result['passed'] = True
except Exception as error:
    result['failure'] = str(error)
finally:
    with (evidence / 'cleanup.log').open('w') as output:
        cleanup = subprocess.run(['docker', 'rm', '--force', container], stdout=output, stderr=subprocess.STDOUT)
    result['cleanup_complete'] = cleanup.returncode == 0
    result['elapsed_seconds'] = round(time.monotonic() - started, 3)
    after = fingerprints()
    result['changed_paths'] = sorted(path for path in before.keys() | after.keys() if before.get(path) != after.get(path))
    for name, value in [('result', result), ('commands', commands), ('before-sha256', before), ('after-sha256', after)]:
        (evidence / (name + '.json')).write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')
print(json.dumps(result, sort_keys=True))
raise SystemExit(0 if result['passed'] and result['cleanup_complete'] else 1)
