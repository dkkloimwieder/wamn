#!/usr/bin/env python3
"""Refresh the Receiving pin after a successful normal m1 component build."""
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
build_evidence = args.build_evidence.resolve(strict=True)
binary = tree / 'target/debug/examples/materialize_package'
assert binary.is_file(), 'build materialize_package before running this gate'
built = json.loads((build_evidence / 'result.json').read_text())
assert built['exit_code'] == 0, 'normal m1 build did not pass'
command = json.loads((build_evidence / 'command.json').read_text())
assert command['argv'] == ['env', 'CARGO_TARGET_DIR=' + str(tree / 'target'),
                           'tools/build-components', 'm1'], 'pin requires the normal m1 profile and exclusive lane cache'
artifact = tree / 'target/virtualized/std-empty-environment/receiving.wasm'
artifact_bytes = artifact.read_bytes()
assert artifact_bytes.startswith(b'\0asm'), 'expected a built Wasm component'
digest = 'sha256:' + hashlib.sha256(artifact_bytes).hexdigest()
manifest_path = tree / 'packages/client_acme_receiving/wamn.json'
manifest_bytes = manifest_path.read_bytes()
manifest = json.loads(manifest_bytes)
dependency = manifest['base_dependencies']['base_receiving']
assert dependency['package'] == 'wamn_receiving'
old_digest = dependency['digest']
assert manifest_bytes.count(old_digest.encode()) == 1, 'expected one authored Receiving digest'
replacement = manifest_bytes.replace(old_digest.encode(), digest.encode(), 1)
evidence = args.evidence_dir.resolve()
evidence.mkdir(parents=True, exist_ok=False)
container = 'wamn-guest-digest-remint-' + uuid.uuid4().hex[:12]
env = {key: value for key, value in os.environ.items()
       if not key.startswith(('WAMN_', 'PG', 'GIT_', 'OTEL_'))
       and key not in {'DATABASE_URL', 'CARGO_TARGET_DIR'}}
env.update(KUBECONFIG='/dev/null')
commands = []
result = {'passed': False, 'container': container, 'before_digest': old_digest,
          'after_digest': digest, 'build_evidence': str(build_evidence),
          'cleanup_complete': False, 'packages': {}}
started = time.monotonic()


def write_json(name, value):
    (evidence / (name + '.json')).write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')


def hashes(paths):
    return {str(path.relative_to(tree)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in sorted(paths)}


def package_paths(generated):
    return (path for root in (tree / 'packages').iterdir() if root.is_dir()
            for path in root.rglob('*') if path.is_file()
            and ('generated' in path.relative_to(root).parts) == generated)


def source_hashes():
    return hashes([binary, artifact, *package_paths(False)])


def generated_hashes():
    return hashes(package_paths(True))


def run(name, argv, *, environment=env, input_text=None, timeout=120, check=True):
    before = time.monotonic()
    record = {'name': name, 'argv': [str(arg) for arg in argv], 'exit_code': None}
    commands.append(record)
    if input_text is not None:
        (evidence / (name + '.sql')).write_text(input_text)
        record['stdin'] = name + '.sql'
    try:
        with (evidence / (name + '.log')).open('w') as output:
            completed = subprocess.run(record['argv'], cwd=tree, env=environment,
                                       input=input_text, text=True, stdout=output,
                                       stderr=subprocess.STDOUT, timeout=timeout)
        record['exit_code'] = completed.returncode
        if check and completed.returncode:
            raise RuntimeError(f'{name}: exit {completed.returncode}')
        return completed.returncode, (evidence / (name + '.log')).read_text()
    except subprocess.TimeoutExpired:
        record['timed_out'] = True
        raise
    finally:
        record['seconds'] = round(time.monotonic() - before, 3)
        write_json('commands', commands)


source_before = source_hashes()
generated_before = generated_hashes()
write_json('source-before-sha256', source_before)
write_json('before-sha256', generated_before)
write_json('build-receipt-sha256', {
    name: hashlib.sha256((build_evidence / name).read_bytes()).hexdigest()
    for name in ('command.json', 'result.json')})
expected_generated = sorted('packages/client_acme_receiving/generated/' + path for path in (
    'contracts/receiving/record_receipt.inherited-tests.json',
    'contracts/receiving/record_receipt.operation.json',
    'platform-policy/data-access.json',
    'source-map/receiving_record_receipt.json')) if old_digest != digest else []
try:
    run('start', ['docker', 'run', '--detach', '--name', container,
                 '-e', 'POSTGRES_PASSWORD=probe', '-p', '127.0.0.1::5432', 'postgres:18'])
    ready_started = time.monotonic()
    deadline = ready_started + 30
    attempts = 0
    while True:
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise RuntimeError('PostgreSQL did not accept TCP connections within 30 seconds')
        attempts += 1
        status, _ = run(f'ready-{attempts:02d}',
                        ['docker', 'exec', container, 'pg_isready', '-h', '127.0.0.1',
                         '-U', 'postgres', '-t', '1'], timeout=remaining, check=False)
        if status == 0:
            break
        if status not in (1, 2):
            raise RuntimeError(f'PostgreSQL readiness command exited {status}')
        time.sleep(min(0.2, max(0, deadline - time.monotonic())))
    result['readiness'] = {'attempts': attempts, 'limit_seconds': 30,
                           'seconds': round(time.monotonic() - ready_started, 3)}
    _, published = run('port', ['docker', 'port', container, '5432/tcp'])
    address, port = published.strip().rsplit(':', 1)
    assert address == '127.0.0.1' and port.isdigit(), 'expected one loopback PostgreSQL port'
    pg_env = env | {'PGHOST': address, 'PGPORT': port, 'PGUSER': 'postgres',
                    'PGPASSWORD': 'probe', 'PGDATABASE': 'postgres'}
    generator_env = env | {'WAMN_SCHEMA_INTROSPECTION_PG_URL':
                          f'postgresql://postgres:probe@127.0.0.1:{port}/postgres'}
    psql = ['psql', '-X', '-v', 'ON_ERROR_STOP=1', '-qAt']
    _, version = run('version', psql, environment=pg_env, input_text='SHOW server_version_num;\n')
    assert 180000 <= int(version.strip()) < 190000, 'expected PostgreSQL 18'
    result['server_version_num'] = int(version.strip())
    run('image', ['docker', 'inspect', '--format', '{{.Image}}', container])
    run('schema', psql, environment=pg_env, input_text='CREATE SCHEMA receiving;\n')
    for package in ('receiving', 'client_acme_receiving'):
        root = tree / 'packages' / package
        migrations = sorted((root / 'migrations').glob('*.sql'))
        for migration in migrations:
            run(package + '-' + migration.stem, psql + ['-f', migration], environment=pg_env)
        run(package + '-check-before', [binary, 'check', root],
            environment=generator_env, timeout=600)
        result['packages'][package] = {
            'shipped_check_passed': True,
            'migrations': [str(path.relative_to(tree)) for path in migrations]}
    assert generated_hashes() == generated_before, 'pre-write checks changed generated files'
    assert source_hashes() == source_before, 'inputs changed before the pin refresh'
    manifest_path.write_bytes(replacement)
    for mode in ('write', 'check'):
        run('client_acme_receiving-' + mode,
            [binary, mode, tree / 'packages/client_acme_receiving'],
            environment=generator_env, timeout=600)
    result['passed'] = True
except Exception as error:
    result['failure'] = str(error)
    try:
        run('container', ['docker', 'logs', '--timestamps', container], check=False, timeout=30)
    except Exception as diagnostic_error:
        result['diagnostic_failure'] = str(diagnostic_error)
finally:
    try:
        status, _ = run('cleanup', ['docker', 'rm', '--force', '--volumes', container],
                        check=False, timeout=60)
        result['cleanup_complete'] = status == 0
    except Exception as error:
        result['cleanup_failure'] = str(error)
    source_after = source_hashes()
    generated_after = generated_hashes()
    write_json('source-after-sha256', source_after)
    write_json('after-sha256', generated_after)
    result['changed_generated_paths'] = sorted(
        path for path in generated_before.keys() | generated_after.keys()
        if generated_before.get(path) != generated_after.get(path))
    result['expected_changed_generated_paths'] = expected_generated
    result['generated_diff_matches'] = result['changed_generated_paths'] == expected_generated
    source_expected = source_before | {
        str(manifest_path.relative_to(tree)): hashlib.sha256(replacement).hexdigest()}
    result['only_authored_pin_changed'] = source_after == source_expected
    result['passed'] = (result['passed'] and result['cleanup_complete']
                        and result['generated_diff_matches'] and result['only_authored_pin_changed'])
    result['seconds'] = round(time.monotonic() - started, 3)
    write_json('result', result)
print(json.dumps(result, sort_keys=True))
raise SystemExit(0 if result['passed'] else 1)
