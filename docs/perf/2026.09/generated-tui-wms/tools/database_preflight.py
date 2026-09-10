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
spec = importlib.util.spec_from_file_location('wms_live', Path(__file__).with_name('wms_pty.py'))
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)
evidence = driver.support.Evidence(args.evidence_dir, ['probe'])
container = 'wamn-wms-tui-preflight-' + uuid.uuid4().hex[:12]
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
    db = driver.support.Database(url, evidence)
    db.sql('00-schema', 'CREATE SCHEMA wms;')
    db.sql('00-migration', (tree / 'packages/wms/migrations/0001_initial.sql').read_text())
    ids = SimpleNamespace(**{key: str(uuid.uuid4()) for key in ('pallet','product','source','destination')})
    driver.seed(db, ids, 'PREFLIGHT-' + uuid.uuid4().hex[:8])
    observed = driver.snapshot(db, '03-observation', ids)
    assert observed['claims'] == observed['movements'] == 0
    assert observed['pallet'] == {'location_id': ids.source, 'status': 'available', 'row_version': 1}
    assert observed['quantity'] == [{'product_id': ids.product, 'quantity': '10.0000', 'status': 'available'}]
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
