#!/usr/bin/env python3
"""Reproduce wildcard RETURNING authority drift using production package ACLs."""
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import socket
import subprocess
import sys
from urllib.parse import quote

HERE = Path(__file__).resolve().parent
TREE = Path('/home/kaalin/.cache/wamn-lanes/receiving-postcommit-20260910')
CTL = TREE / 'target/debug/wamn-ctl'
BASE = TREE / 'packages/receiving'
OVERLAY = TREE / 'packages/client_acme_receiving'
TENANT = 'returning-privilege-diagnosis'
commands = []


def save(name, value):
    (HERE / name).write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(name, argv, *, env=None, success=True):
    commands.append({'name': name, 'argv': list(map(str, argv))})
    save('commands.json', commands)
    result = subprocess.run(argv, cwd=TREE, env=env, text=True, capture_output=True)
    (HERE / (name + '.stdout')).write_text(result.stdout)
    (HERE / (name + '.stderr')).write_text(result.stderr)
    if success and result.returncode:
        raise RuntimeError(f'{name} exited {result.returncode}; inspect retained stderr')
    return result


if '--inside' not in sys.argv:
    command = ['pg_virtualenv', '-t', '-v', '18', sys.executable, str(Path(__file__).resolve()), '--inside']
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(('PG', 'WAMN_', 'OTEL_', 'GIT_')) and key != 'DATABASE_URL'}
    save('invocation.json', {'argv': command, 'harness_sha256': sha(Path(__file__))})
    with (HERE / 'stdout.log').open('w') as output, (HERE / 'stderr.log').open('w') as errors:
        result = subprocess.run(command, env=env, stdout=output, stderr=errors)
    cluster = json.loads((HERE / 'cluster.json').read_text())
    absent = not Path(cluster['configuration_root']).exists()
    try:
        with socket.create_connection(('127.0.0.1', cluster['port']), timeout=2):
            stopped = False
    except ConnectionRefusedError:
        stopped = True
    cleanup = {'configuration_absent': absent, 'listener_stopped': stopped,
               'verdict': 'pass' if absent and stopped else 'fail'}
    save('cleanup.json', cleanup)
    save('result.json', {'exit_code': result.returncode, 'cleanup': cleanup['verdict']})
    print(json.dumps({'exit_code': result.returncode, 'cleanup': cleanup['verdict'], 'evidence': str(HERE)}))
    raise SystemExit(result.returncode or (0 if cleanup['verdict'] == 'pass' else 1))

save('cluster.json', {'configuration_root': os.environ['PG_CLUSTER_CONF_ROOT'],
                      'port': int(os.environ['PGPORT']), 'version': os.environ['PGVERSION']})
inputs = [BASE / 'wamn.json', OVERLAY / 'wamn.json',
          BASE / 'generated/sql/purchase_order/update.sql',
          OVERLAY / 'generated/sql/purchase_order/update.sql',
          BASE / 'generated/platform-policy/data-access.json',
          OVERLAY / 'generated/platform-policy/data-access.json',
          *sorted((BASE / 'migrations').glob('*.sql')), *sorted((OVERLAY / 'migrations').glob('*.sql')),
          TREE / 'deploy/sql/postgres-init.sql', TREE / 'deploy/sql/catalog-schema.sql', TREE / 'deploy/sql/app-schema.sql']
source = {str(path.relative_to(TREE)): sha(path) for path in inputs}
save('source.json', {'provided_lane_head': '95dc61c1', 'files_sha256': source,
                     'existing_ctl_binary_sha256': sha(CTL), 'binary_rebuilt': False})
psql = ['psql', '-X', '-q', '-A', '-t', '-v', 'ON_ERROR_STOP=1', '-v', 'VERBOSITY=verbose']
bootstrap = HERE / 'roles.sql'
bootstrap.write_text("DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'postgres') THEN CREATE ROLE postgres SUPERUSER; END IF; END $$;\nCREATE ROLE wamn_db_owner NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOINHERIT NOREPLICATION NOBYPASSRLS;\n")
run('roles', psql + ['-f', str(bootstrap)])
run('postgres-init', psql + ['-f', str(TREE / 'deploy/sql/postgres-init.sql')])
version = run('version', psql + ['-c', "SELECT current_setting('server_version_num')::integer"]).stdout.strip()
assert 180000 <= int(version) < 190000
copy = Path(os.environ['PG_CLUSTER_CONF_ROOT']).parent / 'additive-receiving'
shutil.copytree(BASE, copy)
migration = copy / 'migrations/0001_initial.sql'
text = migration.read_text()
header = 'CREATE TABLE receiving.purchase_order (\n'
assert text.count(header) == 1
text = text.replace(header, header + '    overlay_compatibility_note text,\n', 1)
migration.write_text(text)
(HERE / 'additive-initial.sql').write_text(text)
results = []
for candidate in ('baseline', 'additive'):
    database = 'returning_' + candidate
    run(candidate + '-database', psql + ['-c', f'CREATE DATABASE {database} OWNER wamn_db_owner'])
    env = dict(os.environ, PGDATABASE=database)
    env['WAMN_PG_ADMIN_URL'] = (
        f"postgresql://{quote(env['PGUSER'], safe='')}:{quote(env['PGPASSWORD'], safe='')}"
        f"@127.0.0.1:{env['PGPORT']}/{database}")
    for floor in ('catalog-schema.sql', 'app-schema.sql'):
        run(candidate + '-' + floor, psql + ['-f', str(TREE / 'deploy/sql' / floor)], env=env)
    base = BASE if candidate == 'baseline' else copy
    for name, package in (('base', base), ('overlay', OVERLAY)):
        run(candidate + '-apply-' + name, [str(CTL), 'apply-package', '--package', str(package), '--tenant', TENANT], env=env)
    run(candidate + '-reconcile', [str(CTL), 'reconcile-package-data-access', '--package', str(base), '--package', str(OVERLAY), '--tenant', TENANT], env=env)
    run(candidate + '-seed', psql + ['-c', "INSERT INTO receiving.purchase_order (id, purchase_order_number, supplier_id) VALUES ('00000000-0000-0000-0000-000000000301', 'PO-PROBE', '00000000-0000-0000-0000-000000000401')"], env=env)
    acl = run(candidate + '-acl', psql + ['-c', "SELECT jsonb_build_object('table_select', has_table_privilege('wamn_app', 'receiving.purchase_order', 'SELECT'), 'columns', (SELECT jsonb_agg(jsonb_build_object('name', attname, 'select', has_column_privilege('wamn_app', attrelid, attnum, 'SELECT'), 'update', has_column_privilege('wamn_app', attrelid, attnum, 'UPDATE')) ORDER BY attnum) FROM pg_attribute WHERE attrelid = 'receiving.purchase_order'::regclass AND attnum > 0 AND NOT attisdropped))"], env=env)
    grants = json.loads(acl.stdout)
    assert grants['table_select'] is False
    if candidate == 'additive':
        assert next(field for field in grants['columns'] if field['name'] == 'overlay_compatibility_note')['select'] is False
    for package_name, package, types, values in (
        ('base', BASE, 'uuid, bigint, boolean, uuid', "'00000000-0000-0000-0000-000000000301', 1, true, '00000000-0000-0000-0000-000000000402'"),
        ('overlay', OVERLAY, 'uuid, bigint, boolean, boolean, boolean, text', "'00000000-0000-0000-0000-000000000301', 1, true, true, false, NULL"),
    ):
        original = (package / 'generated/sql/purchase_order/update.sql').read_text()
        policy = json.loads((package / 'generated/platform-policy/data-access.json').read_text())
        declared = next(relation['all_fields'] for relation in policy['relations'] if relation['table'] == 'purchase_order')
        assert original.count('RETURNING model.*') == 1
        modes = ('wildcard',) if candidate == 'baseline' else ('wildcard', 'declared')
        for mode in modes:
            statement = original if mode == 'wildcard' else original.replace('RETURNING model.*', 'RETURNING ' + ', '.join('model.' + name for name in declared), 1)
            name = '-'.join((candidate, package_name, mode))
            path = HERE / (name + '.sql')
            path.write_text('BEGIN;\nSET LOCAL ROLE wamn_app;\nSET LOCAL search_path = receiving, pg_catalog;\n' + f'PREPARE probe({types}) AS\n' + statement + f'\nEXECUTE probe({values});\nROLLBACK;\n')
            result = run(name, psql + ['-f', str(path)], env=env, success=False)
            error = re.search(r'ERROR:\s+([A-Z0-9]{5}):\s+([^\n]+)', result.stderr)
            expected_refusal = candidate == 'additive' and mode == 'wildcard'
            if expected_refusal:
                assert result.returncode == 3 and error and error.group(1) == '42501', result.stderr
                assert 'permission denied for table purchase_order' in error.group(2)
            else:
                assert result.returncode == 0 and result.stdout.startswith('updated|1|'), (result.stdout, result.stderr)
            results.append({'candidate': candidate, 'package': package_name, 'projection': mode,
                            'exit_code': result.returncode, 'sqlstate': error.group(1) if error else '00000',
                            'error': error.group(2) if error else None, 'returned': result.stdout.strip(),
                            'role': 'wamn_app', 'statement_sha256': hashlib.sha256(statement.encode()).hexdigest()})
    after = run(candidate + '-row-after-rollback', psql + ['-c', "SELECT row_version FROM receiving.purchase_order WHERE id = '00000000-0000-0000-0000-000000000301'"], env=env)
    assert after.stdout.strip() == '1'
assert source == {str(path.relative_to(TREE)): sha(path) for path in inputs}
save('diagnosis.json', {'result': 'confirmed', 'server_version_num': int(version),
                        'grants': 'production reconcile-package-data-access', 'cases': results,
                        'source_unchanged': True, 'all_statement_mutations_rolled_back': True})
