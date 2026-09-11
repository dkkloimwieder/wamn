#!/usr/bin/env python3
"""Generate the prepared replay-reset control using the normal PG18 authoring recipe."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
from urllib.parse import quote

sys.dont_write_bytecode = True
import mutation

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--inactive-worktree', type=Path, required=True)
parser.add_argument('--cargo-target-dir', type=Path, required=True)
parser.add_argument('--mutation-evidence-dir', type=Path, required=True)
parser.add_argument('--evidence-dir', type=Path, required=True,
                    help='New directory already created by the retained capture.py wrapper')
parser.add_argument('--inside', action='store_true', help=argparse.SUPPRESS)
args = parser.parse_args()
tree = args.inactive_worktree.resolve(strict=True)
target = args.cargo_target_dir.resolve()
prepared = args.mutation_evidence_dir.resolve(strict=True)
evidence = args.evidence_dir.resolve(strict=True)
script = Path(__file__).resolve()
root = script.parents[1]
mutation.require((tree / '.git').is_file(), 'Use an explicit inactive, separate worktree')
mutation.require(target.is_relative_to(tree) and target != tree,
                 'Use a dedicated Cargo target directory inside the inactive worktree')
mutation.require(evidence.is_relative_to(root) and not evidence.is_relative_to(root / 'tools')
                 and evidence != root and evidence != prepared,
                 'Generation evidence must be a separate main postcommit run directory')


def save(name, data):
    with (evidence / name).open('x') as output:
        json.dump(data, output, indent=2, sort_keys=True)
        output.write('\n')


if not args.inside:
    state = mutation.load(tree, prepared)
    mutation.require(state['control'] == 'replay-reset' and state['status'] == 'prepared',
                     'Generate only the captured, unsealed replay-reset control')
    mutation.unchanged(tree, state, state['files'])
    command = ['pg_virtualenv', '-t', '-v', '18', sys.executable, str(script),
               '--inactive-worktree', str(tree), '--cargo-target-dir', str(target),
               '--mutation-evidence-dir', str(prepared), '--evidence-dir', str(evidence), '--inside']
    save('cluster-command.json', {'argv': command,
                                 'harness_sha256': hashlib.sha256(script.read_bytes()).hexdigest()})
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(('PG', 'WAMN_', 'OTEL_', 'GIT_'))
           and key not in ('DATABASE_URL', 'CARGO_TARGET_DIR')}
    env.update(GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_NOSYSTEM='1')
    result = subprocess.run(command, cwd=tree, env=env)
    cluster_path = evidence / 'cluster.json'
    cleanup = {'verdict': 'fail', 'reason': 'cluster identity was not captured'}
    if cluster_path.exists():
        cluster = json.loads(cluster_path.read_text())
        absent = not Path(cluster['configuration_root']).exists()
        try:
            with socket.create_connection(('127.0.0.1', cluster['port']), timeout=2):
                stopped = False
        except ConnectionRefusedError:
            stopped = True
        cleanup = {'configuration_absent': absent, 'listener_stopped': stopped,
                   'verdict': 'pass' if absent and stopped else 'fail'}
    save('cleanup.json', cleanup)
    if result.returncode or cleanup['verdict'] != 'pass':
        raise SystemExit(result.returncode or 1)
    mutation.seal_generated(tree, prepared, mutation.load(tree, prepared))
    save('generation.json', {'result': 'pass', 'mutation_evidence': str(prepared),
                            'generated_paths': list(mutation.GENERATED),
                            'scope': 'authoring generation only; no guest control result'})
    raise SystemExit(0)

save('cluster.json', {'configuration_root': os.environ['PG_CLUSTER_CONF_ROOT'],
                      'port': int(os.environ['PGPORT']), 'version': os.environ['PGVERSION']})
psql = ['psql', '-X', '-v', 'ON_ERROR_STOP=1']
commands = [psql + ['-c', 'CREATE SCHEMA receiving']]
migrations = [tree / 'packages/receiving/migrations/0001_initial.sql',
              tree / 'packages/client_acme_receiving/migrations/0001_add_inspection_required.sql',
              tree / 'packages/client_acme_receiving/migrations/0002_quality_inspection.sql']
commands.extend(psql + ['-f', str(path)] for path in migrations)
materialize = ['cargo', 'run', '-p', 'wamn-schema-generator', '--example', 'materialize_package',
               '--locked', '--offline', '--']
commands.extend(materialize + [mode, 'packages/client_acme_receiving'] for mode in ('write', 'check'))
save('generation-commands.json', {
    'argv': commands, 'cargo_target_dir': str(target),
    'migration_sha256': {str(path.relative_to(tree)): hashlib.sha256(path.read_bytes()).hexdigest()
                         for path in migrations},
    'fixture': 'docs/operations/build-and-test.md migration-only authoring recipe; no installed ACLs'})
env = dict(os.environ)
env['WAMN_SCHEMA_INTROSPECTION_PG_URL'] = (
    f"postgresql://{quote(env['PGUSER'], safe='')}:{quote(env['PGPASSWORD'], safe='')}"
    f"@127.0.0.1:{env['PGPORT']}/postgres")
env.update(CARGO_TARGET_DIR=str(target), CARGO_BUILD_JOBS='2', RUSTC_WRAPPER='')
for command in commands:
    subprocess.run(command, cwd=tree, env=env, check=True)
