#!/usr/bin/env python3
"""Regenerate the three claim packages against one fresh PostgreSQL 18 container."""
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
binary = tree / 'target/debug/examples/materialize_package'
assert binary.is_file(), 'build materialize_package before running this gate'
evidence = args.evidence_dir.resolve()
evidence.mkdir(parents=True, exist_ok=False)
packages = ('receiving', 'client_acme_receiving', 'wms')
container = 'wamn-claim-materialize-' + uuid.uuid4().hex[:12]
env = {key: value for key, value in os.environ.items()
       if not key.startswith(('WAMN_', 'PG', 'GIT_', 'OTEL_'))
       and key not in {'DATABASE_URL', 'CARGO_TARGET_DIR'}}
env.update(KUBECONFIG='/dev/null')
commands = []
result = {'passed': False, 'container': container, 'packages': {}, 'cleanup_complete': False}
started = time.monotonic()


def write_json(name, value):
    (evidence / (name + '.json')).write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')


def hashes(paths):
    return {str(path.relative_to(tree)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in sorted(paths)}


def generated_hashes():
    return hashes(path for package in packages
                  for path in (tree / 'packages' / package / 'generated').rglob('*')
                  if path.is_file())


def run(name, argv, *, environment=env, input_text=None, timeout=120, check=True):
    before = time.monotonic()
    record = {'name': name, 'argv': [str(arg) for arg in argv], 'exit_code': None}
    commands.append(record)
    if input_text is not None:
        (evidence / (name + '.sql')).write_text(input_text)
        record['stdin'] = name + '.sql'
    try:
        with (evidence / (name + '.log')).open('w') as output:
            completed = subprocess.run(record['argv'], cwd=tree, env=environment,
                                       input=input_text, text=True, stdout=output,
                                       stderr=subprocess.STDOUT, timeout=timeout)
        record['exit_code'] = completed.returncode
        if check and completed.returncode:
            raise RuntimeError(f'{name}: exit {completed.returncode}')
        return completed.returncode, (evidence / (name + '.log')).read_text()
    finally:
        record['seconds'] = round(time.monotonic() - before, 3)
        write_json('commands', commands)


source_paths = [binary]
for package in packages:
    root = tree / 'packages' / package
    source_paths.append(root / 'wamn.json')
    source_paths.extend((root / 'migrations').glob('*.sql'))
source = hashes(source_paths)
write_json('source-sha256', source)
before = generated_hashes()
write_json('before-sha256', before)
try:
    run('start', ['docker', 'run', '--detach', '--name', container,
                 '-e', 'POSTGRES_PASSWORD=probe', '-p', '127.0.0.1::5432', 'postgres:18'])
    ready_started = time.monotonic()
    deadline = ready_started + 30
    attempts = 0
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise RuntimeError('PostgreSQL did not accept TCP connections within 30 seconds')
        attempts += 1
        status, _ = run(f'ready-{attempts:02d}',
                        ['docker', 'exec', container, 'pg_isready', '-h', '127.0.0.1',
                         '-U', 'postgres', '-t', '1'], timeout=remaining, check=False)
        if status == 0:
            break
        if status not in (1, 2):
            raise RuntimeError(f'PostgreSQL readiness command exited {status}')
        time.sleep(min(0.2, max(0, deadline - time.monotonic())))
    result['readiness'] = {'attempts': attempts, 'limit_seconds': 30,
                           'seconds': round(time.monotonic() - ready_started, 3)}
    _, published = run('port', ['docker', 'port', container, '5432/tcp'])
    address, port = published.strip().rsplit(':', 1)
    assert address == '127.0.0.1' and port.isdigit(), 'expected one loopback PostgreSQL port'
    pg_env = env | {'PGHOST': address, 'PGPORT': port, 'PGUSER': 'postgres',
                    'PGPASSWORD': 'probe', 'PGDATABASE': 'postgres'}
    generator_env = env | {'WAMN_SCHEMA_INTROSPECTION_PG_URL':
                          f'postgresql://postgres:probe@127.0.0.1:{port}/postgres'}
    psql = ['psql', '-X', '-v', 'ON_ERROR_STOP=1', '-qAt']
    _, version = run('version', psql, environment=pg_env, input_text='SHOW server_version_num;\n')
    assert 180000 <= int(version.strip()) < 190000, 'expected PostgreSQL 18'
    result['server_version_num'] = int(version.strip())
    run('image', ['docker', 'inspect', '--format', '{{.Image}}', container])
    run('schema', psql, environment=pg_env,
        input_text='CREATE SCHEMA receiving; CREATE SCHEMA wms;\n')
    for package in packages:
        root = tree / 'packages' / package
        migrations = sorted((root / 'migrations').glob('*.sql'))
        for migration in migrations:
            run(package + '-' + migration.stem, psql + ['-f', migration], environment=pg_env)
        completed_modes = []
        result['packages'][package] = {'completed_modes': completed_modes,
                                       'migrations': [str(path.relative_to(tree)) for path in migrations]}
        for mode in ('write', 'check'):
            run(package + '-' + mode, [binary, mode, root], environment=generator_env, timeout=600)
            completed_modes.append(mode)
    result['passed'] = True
except Exception as error:
    result['failure'] = str(error)
    run('container', ['docker', 'logs', '--timestamps', container], check=False, timeout=30)
finally:
    try:
        status, _ = run('cleanup', ['docker', 'rm', '--force', '--volumes', container],
                        check=False, timeout=60)
        result['cleanup_complete'] = status == 0
    except Exception as error:
        result['cleanup_failure'] = str(error)
    after = generated_hashes()
    write_json('after-sha256', after)
    result['changed_generated_paths'] = sorted(path for path in before.keys() | after.keys()
                                               if before.get(path) != after.get(path))
    result['source_unchanged'] = hashes(source_paths) == source
    result['passed'] = result['passed'] and result['cleanup_complete'] and result['source_unchanged']
    result['seconds'] = round(time.monotonic() - started, 3)
    write_json('result', result)
print(json.dumps(result, sort_keys=True))
raise SystemExit(0 if result['passed'] else 1)
