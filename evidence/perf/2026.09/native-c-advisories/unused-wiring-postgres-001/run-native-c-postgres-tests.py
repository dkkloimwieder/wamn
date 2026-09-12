#!/usr/bin/env python3
"""Run existing native C tests against separate, owned PostgreSQL containers."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import subprocess
import tempfile
import time
import uuid

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--root', type=Path, required=True)
parser.add_argument('--source', required=True, help='Integrated commit used for the test builds')
parser.add_argument('--out', type=Path, required=True)
parser.add_argument('--runtime-binary', type=Path, required=True)
parser.add_argument('--surface-binary', type=Path, required=True)
parser.add_argument('--matrix-binary', type=Path, required=True)
args = parser.parse_args()
root = args.root.resolve()
out = args.out.resolve()


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def source():
    head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip()
    dirty = subprocess.check_output(
        ['git', 'status', '--porcelain=v1', '--untracked-files=no', '--', '.', ':(exclude).beads'], cwd=root, text=True)
    return {'head': head, 'tracked_status': dirty}


before = source()
assert before == {'head': args.source, 'tracked_status': ''}, 'source must match the clean build commit'
specs = [
    ('released-candidate', args.runtime_binary.resolve(), 'WAMN_EXEC_PLATFORM_PG_URL',
     ['executor_platform_surface_live', '--ignored', '--exact', '--nocapture', '--test-threads=1']),
    ('family-surfaces', args.surface_binary.resolve(), 'WAMN_FAMILY_SURFACE_PG_URL',
     ['--nocapture', '--test-threads=1']),
    ('family-matrix', args.matrix_binary.resolve(), 'WAMN_DENIAL_MATRIX_PG_URL',
     ['--include-ignored', '--nocapture', '--test-threads=1']),
]
for _, binary, _, _ in specs:
    assert binary.is_file() and os.access(binary, os.X_OK), f'test binary is not executable: {binary}'
out.mkdir(parents=True, exist_ok=False)
(out / 'source-before.json').write_text(json.dumps(before, indent=2) + '\n')
(out / 'inputs.json').write_text(json.dumps({
    'source': args.source, 'runner_sha256': digest(Path(__file__)),
    'binaries': {label: {'path': str(binary), 'sha256': digest(binary)}
                 for label, binary, _, _ in specs},
}, indent=2) + '\n')
results = []
for label, binary, variable, test_args in specs:
    print(f'Starting {label}', flush=True)
    stage = out / label
    stage.mkdir()
    password = secrets.token_urlsafe(24)
    commands = []
    fixture_passwords = [password]
    for relative in ['crates/platform/runtime/tests/executor_platform_surface_live.rs',
                     'crates/control/provision/tests/family_surface_grants.rs',
                     'crates/control/provision/tests/family_denial_matrix.rs']:
        fixture_passwords.extend(re.findall(
            r'^const (?:GENERATION_PASSWORD|GENERATION_PW|PROBE_PASSWORD): &str = "([^"]+)";',
            (root / relative).read_text(), re.MULTILINE))
    def redact(value):
        for secret in fixture_passwords:
            value = value.replace(secret, '<fixture-password>')
        return value
    container = None
    result = {'name': label, 'test_exit_code': None, 'cleanup': False}

    def run(name, argv, *, env=None, timeout=60, allow_failure=False):
        safe_args = [redact(str(value)) for value in argv]
        commands.append(safe_args)
        (stage / 'commands.json').write_text(json.dumps(commands, indent=2) + '\n')
        start = time.monotonic()
        timed_out = False
        try:
            completed = subprocess.run(argv, cwd=root, env=env, text=True,
                                       capture_output=True, timeout=timeout)
        except subprocess.TimeoutExpired as error:
            timed_out = True
            def decoded(value):
                return value.decode(errors='replace') if isinstance(value, bytes) else (value or '')
            completed = subprocess.CompletedProcess(argv, 124, decoded(error.stdout), decoded(error.stderr))
        for suffix, value in [('stdout', completed.stdout), ('stderr', completed.stderr)]:
            (stage / f'{name}.{suffix}.log').write_text(redact(value))
        (stage / f'{name}.json').write_text(json.dumps({
            'exit_code': completed.returncode, 'elapsed_seconds': time.monotonic() - start, 'timed_out': timed_out,
        }, indent=2) + '\n')
        if not allow_failure:
            completed.check_returncode()
        return completed

    try:
        run('image', ['docker', 'image', 'inspect', 'postgres:18', '--format', '{{.Id}}'])
        with tempfile.TemporaryDirectory(prefix='wamn-native-c-pg-') as private:
            env_file = Path(private) / 'postgres.env'
            env_file.write_text(f'POSTGRES_PASSWORD={password}\nPOSTGRES_DB=postgres\n')
            env_file.chmod(0o600)
            name = f'wamn-native-c-{label}-{uuid.uuid4().hex[:12]}'
            container = run('create', [
                'docker', 'create', '--pull=never', '--name', name,
                '--label', 'wamn.test.owner=native-c-unused-wiring',
                '--env-file', str(env_file), '--publish', '127.0.0.1::5432', 'postgres:18',
            ]).stdout.strip()
            assert re.fullmatch(r'[0-9a-f]{64}', container), 'Docker did not return a container id'
            result['container_id'] = container
            run('start', ['docker', 'start', container])
            port = run('port', ['docker', 'port', container, '5432/tcp']).stdout.strip()
            assert re.fullmatch(r'127\.0\.0\.1:\d+', port), 'PostgreSQL must bind only to loopback'
            for attempt in range(60):
                ready = run(f'ready-{attempt}', ['docker', 'exec', container, 'pg_isready',
                                               '-h', '127.0.0.1', '-U', 'postgres', '-d', 'postgres'], allow_failure=True)
                if ready.returncode == 0:
                    break
                time.sleep(1)
            else:
                raise RuntimeError('owned PostgreSQL did not become ready')
            url = f'postgresql://postgres:{password}@{port}/postgres'
            env = {key: value for key, value in os.environ.items() if not key.startswith('PG')}
            env[variable] = url
            (stage / 'test-environment.json').write_text(json.dumps({
                variable: f'postgresql://postgres:<fixture-password>@{port}/postgres',
                'inherited_PG_variables': 'removed',
            }, indent=2) + '\n')
            run('version', ['psql', '-X', url, '-Atqc', 'SELECT version();'], env=env)
            tested = run('test', [str(binary), *test_args], env=env, timeout=300, allow_failure=True)
            result['test_exit_code'] = tested.returncode
            result['test_summary'] = next((line for line in tested.stdout.splitlines()
                                           if line.startswith('test result:')), None)
            if tested.returncode == 0:
                assert result['test_summary'] and '0 failed' in result['test_summary'], 'missing test result'
                assert 'skipping ' not in (tested.stdout + tested.stderr), 'a live test silently skipped'
    except Exception as error:
        result['error'] = redact(str(error))
    finally:
        if container:
            try:
                run('postgres-log', ['docker', 'logs', container], allow_failure=True)
            finally:
                removed = run('remove', ['docker', 'rm', '--force', '--volumes', container], allow_failure=True)
                remaining = run('absence', ['docker', 'ps', '--all', '--filter', f'id={container}',
                                            '--format', '{{.ID}}'], allow_failure=True)
                result['cleanup'] = removed.returncode == 0 and remaining.returncode == 0 and not remaining.stdout.strip()
        (stage / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
        results.append(result)
        print(json.dumps(result), flush=True)

after = source()
(out / 'source-after.json').write_text(json.dumps(after, indent=2) + '\n')
passed = before == after and all(item['test_exit_code'] == 0 and item['cleanup'] and 'error' not in item for item in results)
(out / 'result.json').write_text(json.dumps({
    'bead': 'wamn-0ct2.7', 'source_unchanged': before == after, 'tests': results, 'passed': passed,
}, indent=2) + '\n')
raise SystemExit(0 if passed else 1)
