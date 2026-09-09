#!/usr/bin/env python3
"""Run the selected existing authority proofs on serial, owned PostgreSQL 18 fixtures."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import secrets
import signal
import subprocess
import sys
import time

from protected_write_capture import capture_event_registration

SUMMARY = re.compile(r'^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out;.*$', re.M)
SAFE_LINE = re.compile(r'^(?:running \d+ tests?|test [A-Za-z0-9_:]+ \.\.\. (?:ok|FAILED)|test result: (?:ok|FAILED)\. \d+ passed; \d+ failed; \d+ ignored; \d+ measured; \d+ filtered out; finished in [0-9.]+s)$')
SKIP = re.compile(r'(?im)^(?:test [A-Za-z0-9_:]+ \.\.\. )?(?:skipping\b|skipped\b|skip\s*:|self[- ]skip\b)')


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def write_json(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')
    path.chmod(0o644)


def public_copy(raw, output):
    # Existing tests mint additional credentials. Preserve only Rust test protocol
    # lines, not arbitrary diagnostics whose secret values we cannot enumerate.
    lines = raw.read_text(errors='replace').splitlines()
    output.write_text('\n'.join(line if SAFE_LINE.fullmatch(line) else '[private diagnostic redacted]' for line in lines) + '\n')
    output.chmod(0o644)


def clean_source(repo, source):
    head = subprocess.run(['git', '-C', str(repo), 'rev-parse', 'HEAD'], capture_output=True, text=True, check=True).stdout.strip()
    status = subprocess.run(['git', '-C', str(repo), 'status', '--porcelain=v1', '--untracked-files=normal'], capture_output=True, text=True, check=True).stdout
    if head != source or status:
        raise RuntimeError('Source must remain clean and equal the requested full commit')
    return {'commit': head, 'clean': True}


class Runner:
    def __init__(self, repo, evidence):
        self.repo = repo
        self.evidence = evidence
        self.private = evidence / 'private'
        self.private.mkdir(mode=0o700)
        self.commands = []
        self.environment = {k: v for k, v in os.environ.items() if not k.startswith(('WAMN_', 'PG')) and k != 'DATABASE_URL'}

    def command(self, label, argv, environment=None, timeout=1800):
        number = len(self.commands) + 1
        stem = f'{number:03d}-{label}'
        raw = self.private / (stem + '.log')
        output = self.evidence / (stem + '.redacted.log')
        receipt = {'label': label, 'argv': argv, 'cwd': str(self.repo), 'started_unix_ns': time.time_ns(), 'exit_code': None, 'timeout': False, 'interrupted': False, 'raw_private_log': str(raw.relative_to(self.evidence)), 'redacted_log': output.name}
        self.commands.append(receipt)
        error = None
        child = None
        try:
            with raw.open('xb') as log:
                raw.chmod(0o600)
                child = subprocess.Popen(argv, cwd=self.repo, env=environment or self.environment, stdout=log, stderr=subprocess.STDOUT, start_new_session=True)
                try:
                    receipt['exit_code'] = child.wait(timeout=timeout)
                except (subprocess.TimeoutExpired, KeyboardInterrupt) as exc:
                    receipt['timeout'] = isinstance(exc, subprocess.TimeoutExpired)
                    receipt['interrupted'] = isinstance(exc, KeyboardInterrupt)
                    error = exc
                finally:
                    try:
                        os.killpg(child.pid, 0)
                        group_remains = True
                    except ProcessLookupError:
                        group_remains = False
                    receipt['owned_group_cleanup'] = group_remains
                    if group_remains:
                        # The whole owned process group drains before the fixture is removed.
                        try:
                            os.killpg(child.pid, signal.SIGTERM)
                        except ProcessLookupError:
                            pass
                        deadline = time.monotonic() + 20
                        while time.monotonic() < deadline:
                            child.poll()
                            try:
                                os.killpg(child.pid, 0)
                            except ProcessLookupError:
                                break
                            time.sleep(0.1)
                        else:
                            try:
                                os.killpg(child.pid, signal.SIGKILL)
                            except ProcessLookupError:
                                pass
                        receipt['exit_code'] = child.wait()
        except OSError as exc:
            error = exc
            receipt['launch_error'] = type(exc).__name__
        finally:
            receipt['finished_unix_ns'] = time.time_ns()
            if raw.exists():
                public_copy(raw, output)
                receipt['raw_sha256'] = digest(raw)
                receipt['redacted_sha256'] = digest(output)
            write_json(self.evidence / 'commands.json', self.commands)
        if isinstance(error, KeyboardInterrupt):
            raise error
        return receipt, raw.read_text(errors='replace') if raw.exists() else ''

    def required(self, label, argv, environment=None, timeout=120):
        receipt, text = self.command(label, argv, environment, timeout)
        if receipt['exit_code'] != 0 or receipt['timeout'] or 'launch_error' in receipt:
            raise RuntimeError('Required setup command failed: ' + label)
        return text


def run_case(runner, case, source):
    result = {'id': case['id'], 'group': case['group'], 'expected_tests': case['expected_tests'], 'verdict': 'fail', 'stage': 'source-check', 'arming': {}, 'cleanup': {'owned_id': None, 'attempted': False}}
    case_dir = runner.private / case['id']
    case_dir.mkdir(mode=0o700)
    cid_file = case_dir / 'container-id'
    env_file = case_dir / 'postgres.env'
    owned = None
    cleanup_ok = True
    try:
        result['source'] = clean_source(runner.repo, source)
        env = runner.environment.copy()
        if 'artifact' in case:
            result['stage'] = 'artifact-build'
            build_env = env | {'CARGO_TARGET_DIR': str(runner.repo / 'components/target'), 'RUSTC_WRAPPER': ''}
            runner.required(case['id'] + '-artifact', ['cargo', 'build', '--manifest-path', 'components/Cargo.toml', '--locked', '--offline', '-p', 'sqlx-command', '--target', 'wasm32-wasip2'], build_env, timeout=1800)
            artifact = runner.repo / case['artifact']
            if not artifact.is_file() or artifact.stat().st_size == 0:
                raise RuntimeError('Required SQLx guest artifact is absent or empty')
            env['WAMN_SQLX_TRANSACTION_COMPONENT'] = str(artifact)
            result['artifact'] = {'path': case['artifact'], 'sha256': digest(artifact), 'bytes': artifact.stat().st_size}
        result['stage'] = 'postgres-start'
        password = secrets.token_hex(24)
        env_file.write_text('POSTGRES_PASSWORD=' + password + '\n')
        env_file.chmod(0o600)
        name = 'wamn-cutover-' + case['id'] + '-' + secrets.token_hex(6)
        argv = ['docker', 'run', '--detach', '--cidfile', str(cid_file), '--name', name, '--env-file', str(env_file), '--publish', '127.0.0.1::5432', 'postgres:18']
        if case['mode'] == 'scs-off':
            argv += ['-c', 'standard_conforming_strings=off']
        try:
            runner.required(case['id'] + '-start', argv)
        finally:
            if cid_file.exists():
                candidate = cid_file.read_text().strip()
                if not re.fullmatch('[0-9a-f]{64}', candidate):
                    raise RuntimeError('Owned Docker CID file is invalid; inspect private setup evidence')
                owned = candidate
                result['cleanup']['owned_id'] = owned
        if owned is None:
            raise RuntimeError('Docker returned no owned CID file')
        port_text = runner.required(case['id'] + '-port', ['docker', 'port', owned, '5432/tcp']).strip()
        match = re.fullmatch(r'127\.0\.0\.1:([0-9]+)', port_text)
        if match is None:
            raise RuntimeError('Owned PostgreSQL did not publish one loopback port')
        port = match[1]
        env.update(PGHOST='127.0.0.1', PGPORT=port, PGUSER='postgres', PGPASSWORD=password, PGDATABASE='postgres', PGCONNECT_TIMEOUT='2')
        result['stage'] = 'postgres-readiness'
        deadline = time.monotonic() + 90
        while True:
            ready, version = runner.command(case['id'] + '-ready', ['psql', '--no-psqlrc', '--set', 'ON_ERROR_STOP=1', '--tuples-only', '--no-align', '--command', 'SHOW server_version_num'], env, timeout=5)
            if ready['exit_code'] == 0 and not ready['timeout'] and re.fullmatch(r'18\d{4}\s*', version):
                break
            if time.monotonic() >= deadline:
                raise RuntimeError('Owned PostgreSQL 18 readiness timed out')
            time.sleep(1)
        image = runner.required(case['id'] + '-image', ['docker', 'inspect', '--format', '{{.Image}}', owned]).strip()
        if re.fullmatch(r'sha256:[0-9a-f]{64}', image) is None:
            raise RuntimeError('Owned Docker image ID is invalid')
        result['fixture'] = {'container_id': owned, 'image_id': image, 'server_version_num': int(version.strip()), 'host': '127.0.0.1', 'port': int(port)}
        if case['mode'] == 'two-databases':
            runner.required(case['id'] + '-databases', ['psql', '--no-psqlrc', '--set', 'ON_ERROR_STOP=1', '--command', 'CREATE DATABASE bind_project', '--command', 'CREATE DATABASE bind_control'], env)
        for key, database in case['bindings'].items():
            env[key] = f'postgresql://postgres:{password}@127.0.0.1:{port}/{database}'
            result['arming'][key] = {'set': True, 'database': database, 'owned_container': owned, 'host': '127.0.0.1', 'port': int(port)}
        if 'artifact' in case:
            result['arming']['WAMN_SQLX_TRANSACTION_COMPONENT'] = result['artifact']
        result['fallbacks_unset'] = [key for key in ('WAMN_PG_URL', 'DATABASE_URL') if key not in env]
        if set(result['fallbacks_unset']) != {'WAMN_PG_URL', 'DATABASE_URL'}:
            raise RuntimeError('Unexpected ambient URL fallback')
        result['stage'] = 'test'
        test, raw = runner.command(case['id'] + '-test', case['argv'], env)
        summaries = SUMMARY.findall(raw)
        result['test_command'] = test
        result['test_summaries'] = [{'result': row[0], 'passed': int(row[1]), 'failed': int(row[2]), 'ignored': int(row[3]), 'measured': int(row[4]), 'filtered': int(row[5])} for row in summaries]
        result['explicit_skip_detected'] = SKIP.search(raw) is not None
        result['verdict'] = 'pass' if test['exit_code'] == 0 and not test['timeout'] and len(summaries) == 1 and summaries[0][0] == 'ok' and int(summaries[0][1]) == case['expected_tests'] and int(summaries[0][2]) == 0 and int(summaries[0][3]) == 0 and not result['explicit_skip_detected'] else 'fail'
        if case['id'] == 'authority-denial-matrix':
            capture_event_registration(runner, env, result)
        result['stage'] = 'complete'
    except KeyboardInterrupt:
        result['stage'] += '-interrupted'
        result['interrupted'] = True
    except Exception as exc:
        result['error_kind'] = type(exc).__name__
        # Exception text can contain raw child output; it remains private.
        error_path = case_dir / 'error.txt'
        error_path.write_text(str(exc) + '\n')
        error_path.chmod(0o600)
        result['private_error'] = str(error_path.relative_to(runner.evidence))
    finally:
        if owned is not None:
            result['cleanup']['attempted'] = True
            receipt, _ = runner.command(case['id'] + '-cleanup', ['docker', 'rm', '--force', '--volumes', owned], timeout=60)
            cleanup_ok = receipt['exit_code'] == 0 and not receipt['timeout']
            result['cleanup'].update(exit_code=receipt['exit_code'], timeout=receipt['timeout'], success=cleanup_ok)
            if not cleanup_ok:
                result['verdict'] = 'fail'
        env_file.unlink(missing_ok=True)
        write_json(runner.evidence / (case['id'] + '.json'), result)
    return result, cleanup_ok and result['stage'] == 'complete'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, required=True)
    parser.add_argument('--source', required=True, help='Exact full 40-character commit; source must remain clean')
    parser.add_argument('--evidence-dir', type=Path, required=True, help='New directory outside the source checkout; private/ must not be published')
    args = parser.parse_args()
    repo = args.repo.resolve(strict=True)
    evidence = args.evidence_dir.resolve()
    if not re.fullmatch('[0-9a-f]{40}', args.source):
        parser.error('--source must be a full lowercase commit hash')
    if evidence == repo or repo in evidence.parents:
        parser.error('--evidence-dir must be outside the source checkout')
    os.umask(0o077)
    evidence.mkdir(mode=0o700, parents=True, exist_ok=False)
    manifest = Path(__file__).with_name('selection.json')
    selection = json.loads(manifest.read_text())
    runner = Runner(repo, evidence)
    summary = {'schema': 'wamn-cutover-authority-evidence/v1', 'source': args.source, 'selection_source': selection['selection_source'], 'selection_sha256': digest(manifest), 'wrapper_sha256': digest(Path(__file__)), 'started_unix_ns': time.time_ns(), 'verdict': 'fail', 'limitations': selection['limitations'], 'redaction': 'Public logs preserve only Rust test protocol lines. All other diagnostics remain in private/ with mode 0600; never publish private/.', 'cases': [], 'expected_cases': len(selection['cases']), 'expected_groups': 2, 'expected_tests': 24}
    signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))
    try:
        clean_source(repo, args.source)
        for case in selection['cases']:
            result, proceed = run_case(runner, case, args.source)
            summary['cases'].append({'id': result['id'], 'verdict': result['verdict'], 'stage': result['stage'], 'receipt': case['id'] + '.json'})
            write_json(evidence / 'summary.json', summary)
            print(case['id'] + ': ' + result['verdict'], flush=True)
            if not proceed:
                break
        summary['final_source'] = clean_source(repo, args.source)
        summary['verdict'] = 'pass' if len(summary['cases']) == len(selection['cases']) and all(c['verdict'] == 'pass' for c in summary['cases']) else 'fail'
    except (Exception, KeyboardInterrupt) as exc:
        summary['error_kind'] = type(exc).__name__
        error_path = runner.private / 'wrapper-error.txt'
        error_path.write_text(str(exc) + '\n')
        error_path.chmod(0o600)
        summary['private_error'] = str(error_path.relative_to(evidence))
    finally:
        summary['finished_unix_ns'] = time.time_ns()
        summary['not_run'] = [c['id'] for c in selection['cases'] if c['id'] not in {r['id'] for r in summary['cases']}]
        write_json(evidence / 'summary.json', summary)
    return 0 if summary['verdict'] == 'pass' else 1


if __name__ == '__main__':
    sys.exit(main())
