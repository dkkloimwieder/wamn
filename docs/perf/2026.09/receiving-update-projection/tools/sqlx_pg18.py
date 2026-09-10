#!/usr/bin/env python3
"""Refresh and check Receiving SQLx metadata using the normal fresh PG18 recipe."""
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


def save(name, value):
    (evidence / name).write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')


if not args.inside:
    command = ['pg_virtualenv', '-t', '-v', '18', sys.executable, str(Path(__file__).resolve()),
               '--tree', str(tree), '--evidence-dir', str(evidence), '--inside']
    save('cluster-command.json', {'argv': command})
    result = subprocess.run(command, cwd=tree)
    cleanup = {'verdict': 'fail', 'reason': 'no cluster identity receipt'}
    if (evidence / 'cluster.json').is_file():
        cluster = json.loads((evidence / 'cluster.json').read_text())
        absent = not Path(cluster['configuration_root']).exists()
        try:
            with socket.create_connection(('127.0.0.1', cluster['port']), timeout=2):
                stopped = False
        except ConnectionRefusedError:
            stopped = True
        cleanup = {'configuration_absent': absent, 'listener_stopped': stopped,
                   'verdict': 'pass' if absent and stopped else 'fail'}
    save('cleanup.json', cleanup)
    raise SystemExit(result.returncode or (0 if cleanup['verdict'] == 'pass' else 1))

save('cluster.json', {'configuration_root': os.environ['PG_CLUSTER_CONF_ROOT'],
                      'port': int(os.environ['PGPORT']), 'version': os.environ['PGVERSION']})
psql = ['psql', '-X', '-v', 'ON_ERROR_STOP=1']
migrations = [path for package in ('receiving', 'client_acme_receiving')
              for path in sorted((tree / 'packages' / package / 'migrations').glob('*.sql'))]
commands = [psql + ['-c', 'CREATE SCHEMA receiving']]
commands.extend(psql + ['-f', str(path)] for path in migrations)
env = dict(os.environ)
env.pop('SQLX_OFFLINE', None)
env.pop('SQLX_OFFLINE_DIR', None)
env['DATABASE_URL'] = (
    f"postgresql://{quote(env['PGUSER'], safe='')}:{quote(env['PGPASSWORD'], safe='')}"
    f"@127.0.0.1:{env['PGPORT']}/postgres?options=-csearch_path%3Dreceiving%2Cpublic")
env.update(CARGO_TARGET_DIR=str(tree / 'target'), CARGO_BUILD_JOBS='2', RUSTC_WRAPPER='')
selection = ['--', '--package', 'wamn-proof-conformance', '--test', 'receiving_sqlx_verifier',
             '--locked', '--offline']
commands.extend([
    ['cargo', 'sqlx', 'prepare', '--workspace', *selection],
    ['cargo', 'sqlx', 'prepare', '--check', '--workspace', *selection],
])
save('commands.json', {'argv': commands, 'migration_sha256': {
    str(path.relative_to(tree)): hashlib.sha256(path.read_bytes()).hexdigest() for path in migrations},
    'database_url': 'owned disposable PG18 with receiving,public search_path'})


def metadata():
    return {str(path.relative_to(tree)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in sorted((tree / '.sqlx').glob('*.json'))}


before = metadata()
save('before-sha256.json', before)
before_bytes = {name: (tree / name).read_bytes() for name in before}
for name, data in before_bytes.items():
    destination = evidence / 'before' / name
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_bytes(data)
# These are the two unchanged baseline query files at b8c52797.
replaced = {
    '.sqlx/query-ae9bc70de2245821b2776e694c9f169e7bd6dde8895767adeafc42f36f00e71f.json',
    '.sqlx/query-df5a9977e52e7386bd4f49fc96f713418fbf095d63f3c9c284a7725485531645.json',
}
replacement = {
    '.sqlx/query-' + hashlib.sha256(
        (tree / 'packages' / package / 'generated/sql/purchase_order/update.sql').read_bytes()
    ).hexdigest() + '.json'
    for package in ('receiving', 'client_acme_receiving')
}
assert replaced <= before.keys() and len(replacement) == 2 and not (replacement & replaced)
restored = []


def preserve_unrelated():
    for name, data in before_bytes.items():
        if name not in replaced and (not (tree / name).exists() or (tree / name).read_bytes() != data):
            restored.append(name)
            (tree / name).write_bytes(data)


try:
    for command in commands:
        subprocess.run(command, cwd=tree, env=env, check=True)
        if command[:4] == ['cargo', 'sqlx', 'prepare', '--workspace']:
            save('normal-prepare-sha256.json', metadata())
            preserve_unrelated()
            assert replacement <= metadata().keys(), 'normal preparation did not emit both new queries'
finally:
    preserve_unrelated()
    after = metadata()
    save('after-sha256.json', after)
    save('changed-paths.json', sorted(path for path in before.keys() | after.keys()
                                    if before.get(path) != after.get(path)))
    save('preserved-unrelated.json', {'restored_verbatim': sorted(set(restored)),
                                    'source': 'before/.sqlx retained metadata bytes'})
