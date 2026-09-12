#!/usr/bin/env python3
"""Prove explicit exclusion ownership through normal package materialization."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time
import uuid

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--tree', type=Path, required=True)
parser.add_argument('--evidence-dir', type=Path, required=True)
args = parser.parse_args()
tree = args.tree.resolve(strict=True)
evidence = args.evidence_dir.resolve()
evidence.mkdir(parents=True, exist_ok=False)
container = 'wamn-exclusion-owner-' + uuid.uuid4().hex[:12]
env = {key: value for key, value in os.environ.items()
       if not key.startswith(('WAMN_', 'PG', 'GIT_', 'OTEL_'))
       and key not in {'DATABASE_URL', 'CARGO_TARGET_DIR'}}
env.update(RUSTUP_TOOLCHAIN='1.98.0', RUSTC_WRAPPER='', CARGO_BUILD_JOBS='2',
           KUBECONFIG='/dev/null', GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_NOSYSTEM='1')
commands = []
result = {'passed': False, 'container': container, 'packages': {}}
source_files = ['crates/schema/generator/src/generate.rs',
                'crates/schema/generator/tests/generation.rs']
source = {'head': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=tree, text=True).strip(),
          'sha256': {name: hashlib.sha256((tree / name).read_bytes()).hexdigest()
                     for name in source_files}}
(evidence / 'source.json').write_text(json.dumps(source, indent=2) + '\n')
started = time.monotonic()


def run(name, argv, *, environment=env, input_text=None, expected=0):
    before = time.monotonic()
    log = evidence / (name + '.log')
    with log.open('w') as output:
        completed = subprocess.run([str(arg) for arg in argv], cwd=tree, env=environment,
                                   input=input_text, text=True, stdout=output,
                                   stderr=subprocess.STDOUT)
    commands.append({'name': name, 'argv': [str(arg) for arg in argv],
                     'exit_code': completed.returncode,
                     'seconds': round(time.monotonic() - before, 3)})
    assert completed.returncode == expected, f'{name}: exit {completed.returncode}, expected {expected}'
    return log.read_text()


def generated_hashes(fixture):
    return {str(path.relative_to(fixture)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in sorted((fixture / 'generated').rglob('*')) if path.is_file()}


try:
    run('start', ['docker', 'run', '--detach', '--name', container,
                 '-e', 'POSTGRES_PASSWORD=probe', '-p', '127.0.0.1::5432', 'postgres:18'])
    run('build', ['cargo', 'build', '--locked', '--offline', '-p', 'wamn-schema-generator',
                  '--example', 'materialize_package'])
    run('ready', ['docker', 'exec', container, 'pg_isready', '-U', 'postgres'])
    port = subprocess.check_output(['docker', 'port', container, '5432/tcp'], text=True).strip().rsplit(':', 1)[1]
    pg_env = env | {'PGHOST': '127.0.0.1', 'PGPORT': port, 'PGUSER': 'postgres',
                    'PGPASSWORD': 'probe', 'PGDATABASE': 'postgres'}
    generator_env = env | {'WAMN_SCHEMA_INTROSPECTION_PG_URL': f'postgresql://postgres:probe@127.0.0.1:{port}/postgres'}
    psql = ['psql', '-X', '-v', 'ON_ERROR_STOP=1', '-qAt']
    run('schema', psql, environment=pg_env,
        input_text='CREATE SCHEMA receiving; CREATE EXTENSION btree_gist;')
    with tempfile.TemporaryDirectory(prefix='wamn-exclusion-owner-') as scratch:
        for package, field in [('receiving', 'supplier_id'),
                               ('client_acme_receiving', 'acme_quality_status')]:
            fixture = Path(scratch) / package
            shutil.copytree(tree / 'packages' / package, fixture,
                            ignore=shutil.ignore_patterns('generated'))
            for migration in sorted((tree / 'packages' / package / 'migrations').glob('*.sql')):
                run(package + '-' + migration.stem, psql + ['-f', migration], environment=pg_env)
            constraint = f'purchase_order_{field}_excl'
            ddl = f'ALTER TABLE receiving.purchase_order ADD CONSTRAINT {constraint} EXCLUDE USING gist ({field} WITH =);\n'
            (evidence / (package + '-constraint.sql')).write_text(ddl)
            run(package + '-constraint', psql, environment=pg_env, input_text=ddl)
            server_names = run(package + '-catalog', psql, environment=pg_env,
                               input_text="SELECT conname FROM pg_constraint WHERE contype = 'x' AND conrelid = 'receiving.purchase_order'::regclass ORDER BY conname;\n")
            assert constraint in server_names.splitlines()
            manifest_path = fixture / 'wamn.json'
            manifest = json.loads(manifest_path.read_text())
            model = manifest['models']['purchase_order']
            owners = {'purchase_order_supplier_id_excl': 'wamn_receiving'}
            if package == 'client_acme_receiving':
                owners[constraint] = 'client_acme_receiving'
            model['constraint_owners'] = owners.copy()
            model['operations']['update']['error_details']['exclusion_violation'] = {'required': ['constraint']}
            manifest_path.write_text(json.dumps(manifest, indent=2) + '\n')
            materialize = [tree / 'target/debug/examples/materialize_package', 'write', fixture]
            run(package + '-owned', materialize, environment=generator_env)
            errors_path = 'generated/contracts/purchase_order/update.errors.json'
            errors = json.loads((fixture / errors_path).read_text())
            assert errors['closed'] is True
            cases = [case for case in errors['cases'] if case['literal'] == 'exclusion_violation']
            assert cases == [{'literal': 'exclusion_violation', 'from': 'exclusion_violation',
                              'constraint': constraint, 'detail': {'required': ['constraint']}}], cases
            (evidence / (package + '-owned.errors.json')).write_bytes((fixture / errors_path).read_bytes())
            (evidence / (package + '-owned.manifest.json')).write_bytes(manifest_path.read_bytes())
            before = generated_hashes(fixture)
            model['constraint_owners']['purchase_order_missing_excl'] = manifest['package']['id']
            manifest_path.write_text(json.dumps(manifest, indent=2) + '\n')
            refusal = run(package + '-unknown-constraint', materialize, environment=generator_env, expected=1)
            assert 'InvalidModel: purchase_order owns unknown constraint purchase_order_missing_excl' in refusal
            assert generated_hashes(fixture) == before
            model['constraint_owners'] = owners.copy()
            model['constraint_owners'][constraint] = 'undeclared_package'
            manifest_path.write_text(json.dumps(manifest, indent=2) + '\n')
            refusal = run(package + '-undeclared-owner', materialize, environment=generator_env, expected=1)
            assert f'InvalidModel: purchase_order.{constraint} owner undeclared_package is not the package or a declared base' in refusal
            assert generated_hashes(fixture) == before
            result['packages'][package] = {'owners': owners, 'server_exclusions': server_names.splitlines(),
                                           'generated_cases': cases, 'invalid_declarations_refused_before_write': True}
    result['passed'] = True
except Exception as error:
    result['failure'] = str(error)
finally:
    with (evidence / 'cleanup.log').open('w') as output:
        cleaned = subprocess.run(['docker', 'rm', '--force', '--volumes', container],
                                 stdout=output, stderr=subprocess.STDOUT)
    result['cleanup_complete'] = cleaned.returncode == 0
    result['source_unchanged'] = all(hashlib.sha256((tree / name).read_bytes()).hexdigest() == digest
                                   for name, digest in source['sha256'].items())
    result['seconds'] = round(time.monotonic() - started, 3)
    (evidence / 'commands.json').write_text(json.dumps(commands, indent=2) + '\n')
    (evidence / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(result, sort_keys=True))
raise SystemExit(0 if result['passed'] and result['cleanup_complete'] and result['source_unchanged'] else 1)
