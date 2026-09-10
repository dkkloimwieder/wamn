#!/usr/bin/env python3
"""Apply one lock control through normal package materialization."""
import hashlib
import json
import os
from pathlib import Path
import subprocess

lane = Path('/home/kaalin/.cache/wamn-lanes/receiving-correctness-20260909')
evidence = Path(__file__).resolve().parent
baseline = '0673f3f54f2db1250c6b023a11d7dc5877a944da'
branch = 'proof/receiving-contention-control-20260909'
paths = [
    'packages/receiving/command/record_receipt/lock_purchase_order.sql',
    'packages/receiving/generated/wamn/receiving_record_receipt.rs',
    'packages/receiving/generated/contracts/receiving/record_receipt.operation.json',
    'packages/receiving/generated/package-weld.json',
]


def git(*args):
    return subprocess.check_output(['git', *args], cwd=lane, text=True).strip()


def hashes():
    return {name: hashlib.sha256((lane / name).read_bytes()).hexdigest() for name in paths}


def save(name, value):
    (evidence / name).write_text(json.dumps(value, indent=2) + '\n')


assert git('rev-parse', 'HEAD') == baseline
assert git('branch', '--show-current') == 'work/receiving-correctness-20260909'
assert not git('status', '--porcelain')
owned = json.loads((evidence / 'materialization/owned-container.json').read_text())
container = json.loads(subprocess.check_output(['docker', 'inspect', owned['name']], text=True))[0]
assert container['Id'] == owned['id']
assert container['Config']['Labels']['wamn.dev/proof'] == 'receiving-contention-control'
before = hashes()
source = lane / paths[0]
original = source.read_bytes()
assert original.count(b'FOR UPDATE') == 1
subprocess.run(['git', 'switch', '-c', branch], cwd=lane, check=True)
source.write_bytes(original.replace(b'FOR UPDATE', b'FOR KEY SHARE'))
environment = os.environ.copy()
environment['RUSTC_WRAPPER'] = ''
environment['CARGO_TARGET_DIR'] = str(lane / 'target')
environment['WAMN_SCHEMA_INTROSPECTION_PG_URL'] = (
    f"postgresql://postgres:probe@{owned['host']}:{owned['port']}/{owned['database']}"
)
try:
    for mode in ['write', 'check']:
        command = ['cargo', 'run', '-p', 'wamn-schema-generator', '--example',
                   'materialize_package', '--locked', '--offline', '--', mode,
                   'packages/receiving']
        save(f'materialization/{mode}-command.json', {'argv': command, 'cwd': str(lane)})
        with (evidence / f'materialization/{mode}.log').open('xb') as log:
            result = subprocess.run(command, cwd=lane, env=environment,
                                    stdout=log, stderr=subprocess.STDOUT, timeout=600)
        save(f'materialization/{mode}-result.json', {'exit_code': result.returncode})
        result.check_returncode()
    changed = git('diff', '--name-only').splitlines()
    assert sorted(changed) == sorted(paths), changed
    after = hashes()
    assert all(before[name] != after[name] for name in paths)
    weld = json.loads((lane / paths[3]).read_text())
    assert after[paths[0]] == '8e0df44ac99da3dd5ac680f4ce55b715dd8da34004b2a78b46a16293f8fc0d8a'
    assert weld['application_sql_corpus_identity'] == 'sha256:43443f80f2c5d262d91d3c1929652dcb5fc2c932df4af3c292b61322b570ccf6'
    assert weld['verified_schema_state_id'] == 'sha256:8127f449e48e5a608324c7c3da0446ae1c7ff6520be2527ac3c7d2c4bd0f04ec'
    (evidence / 'mutation.patch').write_bytes(subprocess.check_output(['git', 'diff', '--', *paths], cwd=lane))
    subprocess.run(['git', 'diff', '--check'], cwd=lane, check=True)
    subprocess.run(['git', 'add', '--', *paths], cwd=lane, check=True)
    message = evidence / 'commit-message.txt'
    message.write_text('test(wamn-10yt.77): weaken the purchase-order lock control\n\nKeep this deliberate defect on a local proof branch.\n')
    subprocess.run(['git', '-c', 'core.hooksPath=/dev/null', 'commit', '--no-verify',
                    '-F', str(message)], cwd=lane, check=True)
    assert not git('status', '--porcelain')
    save('source-mutation.json', {
        'baseline_commit': baseline, 'mutant_commit': git('rev-parse', 'HEAD'),
        'matched_occurrences': 1, 'paths': paths,
        'before_sha256': before, 'after_sha256': after,
        'mutation': 'FOR UPDATE to FOR KEY SHARE in the purchase-order statement',
        'corpus_sha256': weld['application_sql_corpus_identity'],
        'schema_state_id': weld['verified_schema_state_id'],
        'normal_materialization': 'write and check passed against base-only PostgreSQL 18',
        'branch_is_local_control_only': True,
        'survival_is_valid': 'line locks and database constraints remain enabled',
    })
finally:
    command = ['docker', 'rm', '-fv', owned['id']]
    result = subprocess.run(command, text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    (evidence / 'materialization/cleanup.log').write_text(result.stdout)
    save('materialization/cleanup-result.json', {'argv': command, 'exit_code': result.returncode})
    result.check_returncode()
