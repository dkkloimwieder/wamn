#!/usr/bin/env python3
"""Run the exact additive UPDATE regression in a fresh disposable PG18 cluster."""
import argparse
import errno
import hashlib
import json
import os
from pathlib import Path
import re
import socket
import subprocess
import sys
from urllib.parse import quote

TEST = 'receiving_data_access::tests::generated_update_ignores_ungranted_additive_columns'
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--tree', type=Path, required=True)
parser.add_argument('--evidence-dir', type=Path, required=True,
                    help='New evidence directory already created by capture.py')
parser.add_argument('--inside', action='store_true', help=argparse.SUPPRESS)
args = parser.parse_args()
tree = args.tree.resolve(strict=True)
evidence = args.evidence_dir.resolve(strict=True)
script = Path(__file__).resolve()


def save(name, value):
    with (evidence / name).open('x') as output:
        json.dump(value, output, indent=2, sort_keys=True)
        output.write('\n')


if not args.inside:
    command = ['pg_virtualenv', '-t', '-v', '18', sys.executable, str(script),
               '--tree', str(tree), '--evidence-dir', str(evidence), '--inside']
    save('pg18-command.json', {'argv': command,
                              'harness_sha256': hashlib.sha256(script.read_bytes()).hexdigest()})
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(('PG', 'WAMN_', 'OTEL_')) and key != 'DATABASE_URL'}
    result = subprocess.run(command, cwd=tree, env=env)
    cluster_path = evidence / 'cluster.json'
    cleanup = {'verdict': 'fail', 'reason': 'cluster identity was not captured'}
    if cluster_path.exists():
        cluster = json.loads(cluster_path.read_text())
        absent = not Path(cluster['configuration_root']).exists()
        try:
            with socket.create_connection(('127.0.0.1', cluster['port']), timeout=2):
                stopped = False
        except OSError as error:
            stopped = error.errno == errno.ECONNREFUSED
        cleanup = {'configuration_absent': absent, 'listener_stopped': stopped,
                   'verdict': 'pass' if absent and stopped else 'fail'}
    save('cleanup.json', cleanup)
    raise SystemExit(result.returncode or (0 if cleanup['verdict'] == 'pass' else 1))

save('cluster.json', {'configuration_root': os.environ['PG_CLUSTER_CONF_ROOT'],
                      'port': int(os.environ['PGPORT']), 'version': os.environ['PGVERSION']})
# The Rust fixture creates its own role and schema in a rollback-only transaction.
# No shared role bootstrap runs before it.
env = dict(os.environ)
env['WAMN_RECEIVING_PG_URL'] = (
    f"postgresql://{quote(env['PGUSER'], safe='')}:{quote(env['PGPASSWORD'], safe='')}"
    f"@127.0.0.1:{env['PGPORT']}/postgres")
env.update(CARGO_BUILD_JOBS='2', RUSTC_WRAPPER='')
command = ['cargo', 'test', '--locked', '--offline', '-p', 'wamn-proof-integration',
           '--lib', TEST, '--', '--ignored', '--exact', '--nocapture']
save('regression-command.json', {'argv': command, 'cwd': str(tree)})
with (evidence / 'regression.stdout.log').open('x') as output, \
     (evidence / 'regression.stderr.log').open('x') as errors:
    result = subprocess.run(command, cwd=tree, env=env, stdout=output, stderr=errors)
stdout = (evidence / 'regression.stdout.log').read_text()
matched = re.search(r'^test ' + re.escape(TEST) + r' \.\.\. (ok|FAILED)$', stdout, re.MULTILINE)
selected = 'running 1 test\n' in stdout and matched is not None
save('regression-result.json', {'exit_code': result.returncode, 'exact_test_ran': selected,
                                'outcome': matched.group(1) if matched else None})
print(json.dumps({'test': TEST, 'exit_code': result.returncode, 'exact_test_ran': selected}), flush=True)
raise SystemExit(result.returncode or (0 if selected and matched.group(1) == 'ok' else 1))
