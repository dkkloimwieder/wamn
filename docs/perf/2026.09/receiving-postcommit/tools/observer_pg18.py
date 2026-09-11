#!/usr/bin/env python3
"""Run the exact installed-schema observer check in a disposable PG18 cluster."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
from urllib.parse import quote

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--tree', type=Path, required=True)
parser.add_argument('--evidence-dir', type=Path, required=True)
parser.add_argument('--inside', action='store_true', help=argparse.SUPPRESS)
args = parser.parse_args()
tree = args.tree.resolve(strict=True)
evidence = args.evidence_dir.resolve(strict=True)
script = Path(__file__).resolve()

if not args.inside:
    command = ['pg_virtualenv', '-t', '-v', '18', sys.executable, str(script),
               '--tree', str(tree), '--evidence-dir', str(evidence), '--inside']
    result = subprocess.run(command, cwd=tree)
    cluster = json.loads((evidence / 'cluster.json').read_text())
    absent = not Path(cluster['configuration_root']).exists()
    try:
        with socket.create_connection(('127.0.0.1', cluster['port']), timeout=2):
            stopped = False
    except ConnectionRefusedError:
        stopped = True
    cleanup = {'configuration_absent': absent, 'listener_stopped': stopped,
               'verdict': 'pass' if absent and stopped else 'fail'}
    (evidence / 'cleanup.json').write_text(json.dumps(cleanup, indent=2) + '\n')
    raise SystemExit(result.returncode or (0 if cleanup['verdict'] == 'pass' else 1))

cluster = {'configuration_root': os.environ['PG_CLUSTER_CONF_ROOT'],
           'port': int(os.environ['PGPORT']), 'version': os.environ['PGVERSION']}
(evidence / 'cluster.json').write_text(json.dumps(cluster, indent=2) + '\n')
psql = ['psql', '-X', '-v', 'ON_ERROR_STOP=1']
subprocess.run(psql + ['-c', "DO $$ BEGIN IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'postgres') THEN CREATE ROLE postgres SUPERUSER; END IF; END $$"], check=True)
bootstrap = tree / 'deploy/sql/postgres-init.sql'
subprocess.run(psql + ['-f', str(bootstrap)], check=True)
env = dict(os.environ)
env['WAMN_OVERLAY_OBSERVER_DATABASE_URL'] = (
    f"postgresql://{quote(env['PGUSER'], safe='')}:{quote(env['PGPASSWORD'], safe='')}"
    f"@127.0.0.1:{env['PGPORT']}/postgres")
env['WAMN_OVERLAY_OBSERVER_EVIDENCE_FILE'] = str(evidence / 'observer.json')
env.update(CARGO_BUILD_JOBS='2', RUSTC_WRAPPER='')
command = ['cargo', 'test', '--locked', '--offline', '-p', 'wamn-proof-integration',
           '--lib', 'route_authentication_live::overlay_compatibility::installed_contract_observer_preserves_acls_and_refuses_changed_requirements',
           '--', '--ignored', '--exact', '--nocapture']
(evidence / 'observer-command.json').write_text(json.dumps({
    'argv': command, 'bootstrap_sha256': hashlib.sha256(bootstrap.read_bytes()).hexdigest(),
    'harness_sha256': hashlib.sha256(script.read_bytes()).hexdigest()}, indent=2) + '\n')
raise SystemExit(subprocess.run(command, cwd=tree, env=env).returncode)
