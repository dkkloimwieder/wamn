#!/usr/bin/env python3
"""Refresh the authored Receiving digest and regenerate its dependent package."""
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
parser.add_argument('--build-evidence', type=Path, required=True)
args = parser.parse_args()
tree = args.tree.resolve(strict=True)
evidence = args.evidence_dir.resolve()
evidence.mkdir(parents=True, exist_ok=False)
built = json.loads((args.build_evidence / 'result.json').read_text())
assert built['exit_code'] == 0, 'normal m1 build did not pass'
command = json.loads((args.build_evidence / 'command.json').read_text())
assert command['argv'] == ['tools/build-components', 'm1'], 'pin requires the normal m1 profile'
artifact = tree / 'components/target/virtualized/std-empty-environment/receiving.wasm'
assert artifact.read_bytes().startswith(b'\0asm'), 'expected a built Wasm component'
digest = 'sha256:' + hashlib.sha256(artifact.read_bytes()).hexdigest()
manifest_path = tree / 'packages/client_acme_receiving/wamn.json'
manifest = json.loads(manifest_path.read_text())
assert manifest['base_dependencies']['base_receiving']['package'] == 'wamn_receiving'
old_digest = manifest['base_dependencies']['base_receiving']['digest']
container = 'wamn-exclusion-remint-' + uuid.uuid4().hex[:12]
env = {key: value for key, value in os.environ.items()
       if not key.startswith(('WAMN_', 'PG', 'GIT_', 'OTEL_'))
       and key not in {'DATABASE_URL', 'CARGO_TARGET_DIR'}}
env.update(RUSTUP_TOOLCHAIN='1.98.0', RUSTC_WRAPPER='', CARGO_BUILD_JOBS='2',
           KUBECONFIG='/dev/null', GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_NOSYSTEM='1')
commands = []
result = {'passed': False, 'container': container, 'before_digest': old_digest,
          'after_digest': digest, 'build_evidence': str(args.build_evidence)}

def files():
    return {str(path.relative_to(tree)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in sorted((tree / 'packages/client_acme_receiving/generated').rglob('*'))
            if path.is_file()}

def run(name, argv, *, environment=env, input_text=None):
    started = time.monotonic()
    with (evidence / (name + '.log')).open('w') as output:
        process = subprocess.run([str(arg) for arg in argv], cwd=tree, env=environment,
                                 input=input_text, text=True, stdout=output, stderr=subprocess.STDOUT)
    commands.append({'name': name, 'argv': [str(arg) for arg in argv], 'exit_code': process.returncode,
                     'seconds': round(time.monotonic() - started, 3)})
    if process.returncode:
        raise RuntimeError(f'{name} exited {process.returncode}')

before = files()
try:
    run('start', ['docker', 'run', '--detach', '--name', container,
                 '-e', 'POSTGRES_PASSWORD=probe', '-p', '127.0.0.1::5432', 'postgres:18'])
    port = subprocess.check_output(['docker', 'port', container, '5432/tcp'], text=True).strip().rsplit(':', 1)[1]
    deadline = time.monotonic() + 60
    while subprocess.run(['docker', 'exec', container, 'pg_isready', '-U', 'postgres'],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
        if time.monotonic() >= deadline:
            raise RuntimeError('PostgreSQL startup timed out')
        time.sleep(0.2)
    pg_env = env | {'PGHOST': '127.0.0.1', 'PGPORT': port, 'PGUSER': 'postgres',
                    'PGPASSWORD': 'probe', 'PGDATABASE': 'postgres'}
    psql = ['psql', '-X', '-v', 'ON_ERROR_STOP=1']
    run('schema', psql, environment=pg_env, input_text='CREATE SCHEMA receiving;')
    for package in ['receiving', 'client_acme_receiving']:
        for migration in sorted((tree / 'packages' / package / 'migrations').glob('*.sql')):
            run(package + '-' + migration.stem, psql + ['-f', migration], environment=pg_env)
    manifest['base_dependencies']['base_receiving']['digest'] = digest
    manifest_path.write_text(json.dumps(manifest, indent=2) + '\n')
    url = f'postgresql://postgres:probe@127.0.0.1:{port}/postgres'
    for mode in ['write', 'check']:
        run('materialize-' + mode, ['cargo', 'run', '--locked', '--offline',
            '-p', 'wamn-schema-generator', '--example', 'materialize_package', '--',
            mode, 'packages/client_acme_receiving'],
            environment=env | {'WAMN_SCHEMA_INTROSPECTION_PG_URL': url})
    result['passed'] = True
except Exception as error:
    result['failure'] = str(error)
finally:
    with (evidence / 'cleanup.log').open('w') as output:
        cleaned = subprocess.run(['docker', 'rm', '--force', '--volumes', container],
                                 stdout=output, stderr=subprocess.STDOUT)
    result['cleanup_complete'] = cleaned.returncode == 0
    after = files()
    result['changed_generated_paths'] = sorted(path for path in before.keys() | after.keys()
                                               if before.get(path) != after.get(path))
    for name, value in [('result', result), ('commands', commands), ('before-sha256', before),
                        ('after-sha256', after)]:
        (evidence / (name + '.json')).write_text(json.dumps(value, indent=2) + '\n')
print(json.dumps(result, sort_keys=True))
raise SystemExit(0 if result['passed'] and result['cleanup_complete'] else 1)
