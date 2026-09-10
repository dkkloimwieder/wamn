#!/usr/bin/env python3
"""Prove generated exclusion policies with a disposable PostgreSQL database."""
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
container = 'wamn-exclusion-' + uuid.uuid4().hex[:12]
env = {key: value for key, value in os.environ.items()
       if not key.startswith(('WAMN_', 'PG', 'GIT_', 'OTEL_'))
       and key not in {'DATABASE_URL', 'CARGO_TARGET_DIR'}}
env.update(RUSTUP_TOOLCHAIN='1.98.0', RUSTC_WRAPPER='', CARGO_BUILD_JOBS='2',
           KUBECONFIG='/dev/null', GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_NOSYSTEM='1')
packages = ['receiving', 'client_acme_receiving']
crates = ['wamn-receiving-data-access', 'wamn-client-acme-receiving-data-access']
paths = [tree / 'packages' / package / 'generated/wamn/purchase_order.rs' for package in packages]
original = {path: path.read_bytes() for path in paths}
commands = []
result = {'passed': False, 'container': container, 'mutants': {}, 'diagnostics': {}}
started = time.monotonic()

def digest(data):
    return hashlib.sha256(data).hexdigest()

def run(name, argv, *, environment=env, input_text=None, expected=0):
    log = evidence / (name + '.log')
    before = time.monotonic()
    with log.open('w') as output:
        process = subprocess.run([str(arg) for arg in argv], cwd=tree, env=environment,
                                 input=input_text, text=True, stdout=output,
                                 stderr=subprocess.STDOUT)
    commands.append({'name': name, 'argv': [str(arg) for arg in argv],
                     'exit_code': process.returncode,
                     'seconds': round(time.monotonic() - before, 3)})
    if expected is not None and process.returncode != expected:
        raise RuntimeError(f'{name} exited {process.returncode}, expected {expected}')
    return process.returncode, log.read_text()


def test(name, *, crate=None, expected=0):
    argv = ['cargo', 'test', '--manifest-path', 'components/Cargo.toml', '--locked',
            '--offline', '--no-fail-fast']
    for package in [crate] if crate else crates:
        argv += ['-p', package]
    argv += ['--lib', 'operation::tests::generated_update_exclusion_from_postgres',
             '--', '--exact', '--include-ignored', '--nocapture']
    return run(name, argv, environment=env | {
        'WAMN_EXCLUSION_DIAGNOSTICS': str(evidence / 'diagnostics.json')}, expected=expected)

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
    with tempfile.TemporaryDirectory(prefix='wamn-exclusion-fixtures-') as scratch:
        for package in packages:
            fixture = Path(scratch) / package
            shutil.copytree(tree / 'packages' / package, fixture,
                            ignore=shutil.ignore_patterns('generated'))
            field = 'supplier_id' if package == 'receiving' else 'acme_quality_status'
            constraint = f'purchase_order_{field}_excl'
            manifest = json.loads((fixture / 'wamn.json').read_text())
            model = manifest['models']['purchase_order']
            model['operations']['update']['error_details']['exclusion_violation'] = {
                'required': ['constraint']}
            (fixture / 'wamn.json').write_text(json.dumps(manifest, indent=2) + '\n')
            database = 'proof_' + package
            pg_env = env | {'PGHOST': '127.0.0.1', 'PGPORT': port, 'PGUSER': 'postgres',
                            'PGPASSWORD': 'probe', 'PGDATABASE': database}
            psql = ['psql', '-X', '-v', 'ON_ERROR_STOP=1', '-qAt']
            run(package + '-database', ['docker', 'exec', container, 'createdb', '-U', 'postgres', database])
            run(package + '-schema', psql, environment=pg_env,
                input_text='CREATE SCHEMA receiving; CREATE EXTENSION btree_gist;')
            bases = ['receiving'] if package == 'receiving' else packages
            for base in bases:
                for migration in sorted((tree / 'packages' / base / 'migrations').glob('*.sql')):
                    run(package + '-' + base + '-' + migration.stem, psql + ['-f', migration], environment=pg_env)
            ddl = f'ALTER TABLE receiving.purchase_order ADD CONSTRAINT {constraint} EXCLUDE USING gist ({field} WITH =);\n'
            (evidence / (package + '-constraint.sql')).write_text(ddl)
            run(package + '-constraint', psql, environment=pg_env, input_text=ddl)
            url = f'postgresql://postgres:probe@127.0.0.1:{port}/{database}'
            run(package + '-materialize', ['cargo', 'run', '--locked', '--offline',
                '-p', 'wamn-schema-generator', '--example', 'materialize_package', '--',
                'write', fixture], environment=env | {'WAMN_SCHEMA_INTROSPECTION_PG_URL': url})
            retained = evidence / package
            retained.mkdir()
            for relative in ['wamn/purchase_order.rs', 'source-map/purchase_order.json',
                             'contracts/purchase_order/update.errors.json', 'sql/purchase_order/update.sql']:
                source = fixture / 'generated' / relative
                target = retained / relative
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(source, target)
            errors = json.loads((fixture / 'generated/contracts/purchase_order/update.errors.json').read_text())
            cases = [case for case in errors['cases'] if case['literal'] == 'exclusion_violation']
            assert cases == [{'literal': 'exclusion_violation', 'from': 'exclusion_violation',
                              'constraint': constraint,
                              'detail': {'required': ['constraint']}}], cases
            projection = (fixture / 'generated/wamn/purchase_order.rs').read_bytes()
            (tree / 'packages' / package / 'generated/wamn/purchase_order.rs').write_bytes(projection)
            if package == 'receiving':
                seed = "INSERT INTO receiving.purchase_order(id,purchase_order_number,supplier_id) VALUES ('10000000-0000-0000-0000-000000000001','proof-1','20000000-0000-0000-0000-000000000001'), ('10000000-0000-0000-0000-000000000002','proof-2','20000000-0000-0000-0000-000000000002');"
                arguments = "'10000000-0000-0000-0000-000000000002',1,true,'20000000-0000-0000-0000-000000000001'"
            else:
                seed = "INSERT INTO receiving.purchase_order(id,purchase_order_number,supplier_id,acme_quality_status) VALUES ('10000000-0000-0000-0000-000000000001','proof-1','20000000-0000-0000-0000-000000000001','pending'), ('10000000-0000-0000-0000-000000000002','proof-2','20000000-0000-0000-0000-000000000002','approved');"
                arguments = "'10000000-0000-0000-0000-000000000002',1,false,NULL,true,'pending'"
            generated_sql = (fixture / 'generated/sql/purchase_order/update.sql').read_text()
            sql = f'''SET search_path = receiving, public;
{seed}
PREPARE exclusion_update AS {generated_sql}
CREATE FUNCTION pg_temp.refusal() RETURNS json LANGUAGE plpgsql AS $proof$
DECLARE state text; constraint_name text; message text;
BEGIN
    EXECUTE $command$EXECUTE exclusion_update({arguments})$command$;
    RAISE EXCEPTION 'generated update did not violate the exclusion';
EXCEPTION WHEN exclusion_violation THEN
    GET STACKED DIAGNOSTICS state = RETURNED_SQLSTATE,
        constraint_name = CONSTRAINT_NAME, message = MESSAGE_TEXT;
    RETURN json_build_object('sqlstate', state, 'constraint', constraint_name, 'message', message);
END
$proof$;
SELECT pg_temp.refusal();
'''
            (retained / 'execute.sql').write_text(sql)
            _, output = run(package + '-server-refusal', psql, environment=pg_env, input_text=sql)
            diagnostic = json.loads(output.strip())
            assert diagnostic['sqlstate'] == '23P01', diagnostic
            assert diagnostic['constraint'] == constraint, diagnostic
            result['diagnostics'][package] = diagnostic
        (evidence / 'diagnostics.json').write_text(json.dumps(result['diagnostics'], indent=2) + '\n')
        test('generated-policies')
        for package, crate, source, needle in [
            ('receiving', crates[0], 'components/data/receiving-data/src/purchase_order.rs', '    generated::UPDATE_EXCLUSION_CONSTRAINTS,'),
            ('client_acme_receiving', crates[1], 'components/data/client-acme-receiving-data/src/operation.rs', '    exclusion: purchase_order_sql::UPDATE_EXCLUSION_CONSTRAINTS,'),
        ]:
            path = tree / source
            before = path.read_bytes()
            replacement = '    &[],' if package == 'receiving' else '    exclusion: &[],'
            text = before.decode()
            assert text.count(needle) == 1
            try:
                path.write_text(text.replace(needle, replacement, 1))
                code, output = test(package + '-disconnected-policy', crate=crate, expected=None)
                assert code == 101 and 'generated_update_exclusion_from_postgres ... FAILED' in output, output[-2000:]
                assert 'internal_error' in output and 'exclusion_violation' in output, output[-2000:]
                result['mutants'][package] = {'exit_code': code, 'named_test_failed': True,
                                               'source_sha256': digest(before)}
            finally:
                path.write_bytes(before)
                assert path.read_bytes() == before
            test(package + '-restored-policy', crate=crate)
        result['passed'] = True
except Exception as error:
    result['failure'] = str(error)
finally:
    for path, content in original.items():
        path.write_bytes(content)
    result['generated_outputs_restored'] = all(path.read_bytes() == content for path, content in original.items())
    result['original_generated_sha256'] = {str(path.relative_to(tree)): digest(content) for path, content in original.items()}
    with (evidence / 'cleanup.log').open('w') as output:
        cleaned = subprocess.run(['docker', 'rm', '--force', '--volumes', container],
                                 stdout=output, stderr=subprocess.STDOUT)
    result['cleanup_complete'] = cleaned.returncode == 0
    result['seconds'] = round(time.monotonic() - started, 3)
    (evidence / 'commands.json').write_text(json.dumps(commands, indent=2) + '\n')
    (evidence / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(result, sort_keys=True))
raise SystemExit(0 if result['passed'] and result['generated_outputs_restored'] and result['cleanup_complete'] else 1)
