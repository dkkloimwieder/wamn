#!/usr/bin/env python3
"""Record the existing materializer's comparison with all three app outputs."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--tree', type=Path, required=True)
parser.add_argument('--evidence', type=Path, required=True)
args = parser.parse_args()
tree = args.tree.resolve(strict=True)
evidence = args.evidence.resolve()
evidence.mkdir(parents=True, exist_ok=False)
binary = tree / 'target/debug/examples/materialize_package'
commands = []

def generated():
    return {str(path.relative_to(tree)): hashlib.sha256(path.read_bytes()).hexdigest()
            for manifest in sorted((tree / 'apps').glob('*/wamn.json'))
            for path in sorted((manifest.parent / 'generated').rglob('*')) if path.is_file()}

def save(name, value):
    (evidence / name).write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')

def run(name, argv, *, env=None, text=None):
    started = time.monotonic()
    if text is not None:
        (evidence / (name + '.sql')).write_text(text)
    with (evidence / (name + '.stdout')).open('w') as out, (evidence / (name + '.stderr')).open('w') as err:
        result = subprocess.run([str(arg) for arg in argv], cwd=tree, env=env,
                                input=text, text=True, stdout=out, stderr=err, timeout=120)
    commands.append({'name': name, 'argv': [str(arg) for arg in argv],
                     'exit_code': result.returncode, 'seconds': time.monotonic() - started,
                     'stdin': name + '.sql' if text is not None else None})
    save('commands.json', commands)
    if result.returncode:
        raise RuntimeError(f'{name} exited {result.returncode}')
    return (evidence / (name + '.stdout')).read_text()

before = generated()
save('generated-before.json', before)
source = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=tree, text=True).strip()
result = {'passed': False, 'source_head': source,
          'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
          'generator': 'wamn-schema-generator/0.1.0', 'toolchain': 'rust-1.98.0',
          'generated_files': len(before), 'packages': {}}
started = time.monotonic()
try:
    psql = ['psql', '-X', '-v', 'ON_ERROR_STOP=1', '-qAt']
    version = int(run('version', psql, text='SHOW server_version_num;\n').strip())
    assert 180000 <= version < 190000
    result['server_version_num'] = version
    run('schemas', psql, text='CREATE SCHEMA receiving;\nCREATE SCHEMA wms;\n')
    environment = dict(os.environ)
    environment['WAMN_SCHEMA_INTROSPECTION_PG_URL'] = (
        f"postgresql://{os.environ['PGUSER']}:{os.environ['PGPASSWORD']}@"
        f"{os.environ['PGHOST']}:{os.environ['PGPORT']}/{os.environ.get('PGDATABASE', 'postgres')}")
    for package in ('wamn_receiving', 'client_acme_receiving', 'wamn_wms'):
        root = tree / 'apps' / package
        migrations = sorted((root / 'migrations').glob('*.sql'))
        for migration in migrations:
            run(package + '-' + migration.stem, psql + ['-f', migration])
        run(package + '-check', [binary, 'check', root], env=environment)
        result['packages'][package] = {'passed': True,
            'migrations': {str(path.relative_to(tree)): hashlib.sha256(path.read_bytes()).hexdigest()
                           for path in migrations}}
    result['passed'] = True
except Exception as error:
    result['failure'] = str(error)
finally:
    after = generated()
    save('generated-after.json', after)
    result['generated_unchanged'] = before == after
    result['seconds'] = time.monotonic() - started
    result['passed'] = result['passed'] and result['generated_unchanged']
    save('result.json', result)
print(json.dumps(result, sort_keys=True))
raise SystemExit(0 if result['passed'] else 1)
