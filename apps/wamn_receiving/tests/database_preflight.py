#!/usr/bin/env python3
"""Exercise the live driver's seed, observation, and cleanup against fresh PostgreSQL."""
import argparse
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import time
from types import SimpleNamespace
import uuid

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--tree', type=Path, required=True)
parser.add_argument('--evidence-dir', type=Path, required=True)
args = parser.parse_args()
tree = args.tree.resolve(strict=True)
sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('receiving_live', Path(__file__).with_name('receiving_pty.py'))
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)
evidence = driver.Evidence(args.evidence_dir, ['probe'])
container = 'wamn-tui-database-preflight-' + uuid.uuid4().hex[:12]
result = {'passed': False, 'container': container, 'cleanup_complete': False}
try:
    subprocess.run(['docker', 'run', '--detach', '--name', container, '-e', 'POSTGRES_PASSWORD=probe',
                    '-p', '127.0.0.1::5432', 'postgres:18'], check=True, stdout=subprocess.DEVNULL)
    port = subprocess.check_output(['docker', 'port', container, '5432/tcp'], text=True).strip().rsplit(':', 1)[1]
    url = f'postgresql://postgres:probe@127.0.0.1:{port}/postgres'
    deadline = time.monotonic() + 60
    while subprocess.run(['psql', '--dbname', url, '-X', '-Atqc', 'SELECT 1'],
                         stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode:
        if time.monotonic() >= deadline:
            raise RuntimeError('PostgreSQL connection timed out')
        time.sleep(0.2)
    db = driver.Database(url, evidence)
    db.sql('00-schema', 'CREATE SCHEMA receiving;')
    db.sql('00-migration', (tree / 'apps/wamn_receiving/migrations/0001_initial.sql').read_text())
    # apply-package installs these stamp triggers from the declarations. The preflight installs them the same way.
    db.sql('00-record-history', (tree / 'deploy/sql/record-history.sql').read_text())
    for model in json.loads((tree / 'apps/wamn_receiving/wamn.json').read_text())['models'].values():
        columns = model['audit_log']['columns']
        if columns:
            db.sql('00-trigger-' + model['table'], f"CREATE TRIGGER record_history_stamp BEFORE INSERT OR UPDATE ON {model['schema']}.{model['table']} FOR EACH ROW EXECUTE FUNCTION wamn_history.stamp_row({', '.join(repr(column) for column in columns)});")
    # The seed provisions its principal beside the platform rows, so the preflight installs the project system schema.
    db.sql('00-app-schema', 'CREATE ROLE wamn_app NOLOGIN;\n' + (tree / 'deploy/sql/app-schema.sql').read_text())
    # record-history-app-grants.sql grants to wamn_app, so it follows the step that creates that role.
    db.sql('00-record-history-app-grants', (tree / 'deploy/sql/record-history-app-grants.sql').read_text())
    ids = SimpleNamespace(**{key: str(uuid.uuid4()) for key in ('order','supplier','item','dock1','dock2','line1','line2')})
    navigation = driver.seed(db, ids, 'PREFLIGHT-' + uuid.uuid4().hex[:8])
    observed = driver.snapshot(db, '03-observation', ids)
    assert navigation['location_index'] == 1
    assert observed['claims'] == observed['receipts'] == 0
    assert [row['received'] for row in observed['lines']] == ['0.0000', '0.0000']
    driver.cleanup(db, ids)
    result['passed'] = True
except Exception as error:
    result['error'] = type(error).__name__
finally:
    result['cleanup_complete'] = subprocess.run(['docker','rm','--force',container], stdout=subprocess.DEVNULL).returncode == 0
    evidence.json('result.json', result)
    evidence.sums()
print(json.dumps(result))
raise SystemExit(0 if result['passed'] and result['cleanup_complete'] else 1)
