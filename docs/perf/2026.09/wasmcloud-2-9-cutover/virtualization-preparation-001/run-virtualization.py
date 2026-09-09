#!/usr/bin/env python3
"""Run both existing virtualization proofs, clean owned Compose resources, then rebuild m1."""
import argparse
import json
import os
from pathlib import Path
import re
import secrets
import signal
import socket
import sys
import time

from proof_support import Runner, SKIP, SUMMARY, clean_source, digest, write_json


def reserve_ports():
    sockets = [socket.socket(), socket.socket()]
    try:
        for stream in sockets:
            stream.bind(('127.0.0.1', 0))
        return sockets, [stream.getsockname()[1] for stream in sockets]
    except BaseException:
        for stream in sockets:
            stream.close()
        raise


def artifacts(repo):
    paths = ['components/target/wasm32-wasip2/release/std_virtualization_probe.wasm', 'components/target/virtualized/std-empty-environment/std_virtualization_probe.wasm', 'components/target/virtualized/std-empty-environment/receiving.wasm', 'components/target/wasm32-wasip2/release/http_route.wasm']
    return {name: {'bytes': (repo/name).stat().st_size, 'sha256': digest(repo/name)} for name in paths if (repo/name).is_file()}


def exact_test(runner, selected, env, source, index, result):
    clean_source(runner.repo, source)
    result.update({'name': selected['argv'][selected['argv'].index('--lib') + 1], 'expected_tests': 1, 'invoked': True, 'passed': False, 'classification': 'invoked_failed', 'arming': {key: {'set': True, 'value': '<private>' if key.endswith('_PG_URL') else value} for key, value in env.items() if key.startswith('WAMN_STD_VIRTUALIZATION_')}})
    receipt, output = runner.command('test-' + str(index), selected['argv'], env, timeout=1800)
    result['command'] = receipt
    rows = SUMMARY.findall(output)
    result['summaries'] = [{'result': row[0], 'passed': int(row[1]), 'failed': int(row[2]), 'ignored': int(row[3]), 'measured': int(row[4]), 'filtered': int(row[5])} for row in rows]
    result['explicit_skip_detected'] = SKIP.search(output) is not None
    result['passed'] = receipt['exit_code'] == 0 and not receipt['timeout'] and len(rows) == 1 and rows[0][0] == 'ok' and int(rows[0][1]) == 1 and int(rows[0][2]) == 0 and int(rows[0][3]) == 0 and not result['explicit_skip_detected']
    if result['passed']:
        result['classification'] = 'invoked_passed'
    elif result['explicit_skip_detected'] or any(row['ignored'] for row in result['summaries']):
        result['classification'] = 'invoked_but_ignored_or_self_skipped'
    elif not rows or sum(row['passed'] + row['failed'] for row in result['summaries']) == 0:
        result['classification'] = 'invoked_without_executed_test'
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repo', type=Path, required=True)
    parser.add_argument('--source', required=True, help='Full clean source commit')
    parser.add_argument('--evidence-dir', type=Path, required=True, help='New directory outside the source checkout')
    args = parser.parse_args()
    repo = args.repo.resolve(strict=True)
    evidence = args.evidence_dir.resolve()
    if not re.fullmatch('[0-9a-f]{40}', args.source):
        parser.error('--source must be the full lowercase commit hash')
    if evidence == repo or repo in evidence.parents:
        parser.error('--evidence-dir must be outside the source checkout')
    os.umask(0o077)
    evidence.mkdir(mode=0o700, parents=True, exist_ok=False)
    selection_file = Path(__file__).with_name('selection.json')
    selection = json.loads(selection_file.read_text())
    runner = Runner(repo, evidence)
    # Do not inherit another Compose owner's profiles, project, or env-file inputs.
    env = {key: value for key, value in runner.environment.items() if not key.startswith('COMPOSE_')}
    env['RUSTC_WRAPPER'] = ''
    env.pop('CARGO_TARGET_DIR', None)  # Use each workspace's normal target directory.
    project = 'wamn-cutover-std-' + secrets.token_hex(8)
    compose = ['docker', 'compose', '-p', project, '-f', 'test-support/infrastructure/std-virtualization.compose.yaml']
    project_filter = 'label=com.docker.compose.project=' + project
    owned = False
    sockets = []
    proof_attempted = False
    summary = {'schema': 'wamn-cutover-virtualization-evidence/v1', 'source': args.source, 'selection_sha256': digest(selection_file), 'wrapper_sha256': digest(Path(__file__)), 'support_sha256': digest(Path(__file__).with_name('proof_support.py')), 'started_unix_ns': time.time_ns(), 'verdict': 'fail', 'stage': 'source-check', 'acceptance': selection['acceptance'], 'profile_risk': selection['profile_risk'], 'tests': [{'name': test['argv'][test['argv'].index('--lib') + 1], 'invoked': False, 'passed': False, 'classification': 'not_invoked'} for test in selection['tests']], 'compose': {'project': project, 'cleanup_attempted': False, 'cleanup_passed': False}, 'm1_restore': {'required': False, 'attempted': False, 'passed': False}, 'redaction': 'Private raw logs and errors have mode 0600. Public copies preserve Rust test protocol lines only; do not publish private/.'}
    signal.signal(signal.SIGTERM, lambda *_: (_ for _ in ()).throw(KeyboardInterrupt()))
    try:
        summary['source_preflight'] = clean_source(repo, args.source)
        summary['artifacts_before'] = artifacts(repo)
        summary['stage'] = 'build-proof'
        proof_attempted = True
        summary['m1_restore']['required'] = True
        runner.required('build-proof', ['tools/build-components', 'proof'], env, timeout=1800)
        virtual_dir = repo/'components/target/virtualized/std-empty-environment'
        virtual_dir.mkdir(parents=True, exist_ok=True)
        runner.required('virtualize-probe', ['cargo', 'run', '-p', 'wamn-component-virtualizer', '--locked', '--offline', '--', '--input', 'components/target/wasm32-wasip2/release/std_virtualization_probe.wasm', '--output', str(virtual_dir/'std_virtualization_probe.wasm')], env, timeout=1800)
        summary['proof_artifacts'] = artifacts(repo)
        if len(summary['proof_artifacts']) != 4 or any(row['bytes'] == 0 for row in summary['proof_artifacts'].values()):
            raise RuntimeError('A required probe, Receiving, or HTTP shell artifact is missing or empty')
        env.update(WAMN_STD_VIRTUALIZATION_COMPONENT_WASM=str(virtual_dir/'std_virtualization_probe.wasm'), WAMN_STD_VIRTUALIZATION_RECEIVING_DIRECTORY=str(virtual_dir))
        summary['stage'] = 'artifact-test'
        summary['tests'][0] = exact_test(runner, selection['tests'][0], env, args.source, 1, summary['tests'][0])
        write_json(evidence/'summary.json', summary)
        if not summary['tests'][0]['passed']:
            raise RuntimeError('Exact artifact test did not pass; live test remains uninvoked')
        summary['stage'] = 'compose-preflight'
        for kind, argv in [('containers', ['docker', 'ps', '--all', '--quiet', '--filter', project_filter]), ('networks', ['docker', 'network', 'ls', '--quiet', '--filter', project_filter]), ('volumes', ['docker', 'volume', 'ls', '--quiet', '--filter', project_filter])]:
            existing = runner.required('compose-preflight-' + kind, argv, env).strip()
            if existing:
                raise RuntimeError('Generated Compose project already has resources; refusing ownership')
        sockets, ports = reserve_ports()
        if len(set(ports)) != 2:
            raise RuntimeError('The two reserved ports must differ')
        env.update(WAMN_STD_VIRT_PG_PORT=str(ports[0]), WAMN_STD_VIRT_REGISTRY_PORT=str(ports[1]))
        summary['compose']['ports'] = {'postgres': ports[0], 'registry': ports[1]}
        for stream in sockets:
            stream.close()
        sockets = []
        # Compose must bind these ports itself. A racing bind fails; no existing service is reused.
        owned = True
        summary['stage'] = 'compose-start'
        runner.required('compose-up', compose + ['up', '--detach', '--wait', '--wait-timeout', '60', 'postgres', 'registry'], env, timeout=120)
        containers = runner.required('owned-containers', ['docker', 'ps', '--all', '--quiet', '--no-trunc', '--filter', project_filter], env).split()
        if len(containers) != 2 or any(not re.fullmatch('[0-9a-f]{64}', cid) for cid in containers):
            raise RuntimeError('Compose must own exactly two valid container IDs')
        services = {}
        for index, cid in enumerate(containers):
            labels = json.loads(runner.required('owned-labels-' + str(index), ['docker', 'inspect', '--format', '{{json .Config.Labels}}', cid], env))
            service = labels.get('com.docker.compose.service')
            if labels.get('com.docker.compose.project') != project or service not in {'postgres', 'registry'} or service in services:
                raise RuntimeError('Unexpected Compose project or service ownership')
            services[service] = cid
        summary['compose']['owned_containers'] = services
        env.update(PGHOST='127.0.0.1', PGPORT=str(ports[0]), PGUSER='postgres', PGPASSWORD='probe', PGDATABASE='postgres', PGCONNECT_TIMEOUT='2')
        summary['stage'] = 'postgres-readiness'
        deadline = time.monotonic() + 90
        while True:
            ready, version = runner.command('postgres-ready', ['psql', '--no-psqlrc', '--set', 'ON_ERROR_STOP=1', '--tuples-only', '--no-align', '--command', 'SHOW server_version_num'], env, timeout=5)
            if ready['exit_code'] == 0 and not ready['timeout'] and re.fullmatch(r'18\d{4}\s*', version):
                break
            if time.monotonic() >= deadline:
                raise RuntimeError('Owned PostgreSQL 18 did not become ready through its published port')
            time.sleep(1)
        summary['compose']['server_version_num'] = int(version.strip())
        env.update(WAMN_STD_VIRTUALIZATION_SENTINEL='must-not-cross', WAMN_STD_VIRTUALIZATION_PG_URL=f'postgresql://postgres:probe@127.0.0.1:{ports[0]}/postgres', WAMN_STD_VIRTUALIZATION_ARTIFACT_BASE=f'127.0.0.1:{ports[1]}/wamn/std-proof', WAMN_STD_VIRTUALIZATION_FLOW_HTTP_WASM=str(repo/'components/target/wasm32-wasip2/release/http_route.wasm'))
        summary['stage'] = 'live-test'
        summary['tests'][1] = exact_test(runner, selection['tests'][1], env, args.source, 2, summary['tests'][1])
        summary['stage'] = 'proof-complete'
    except (Exception, KeyboardInterrupt) as exc:
        summary['error_kind'] = type(exc).__name__
        error_path = runner.private/'error.txt'
        error_path.write_text(str(exc) + '\n')
        error_path.chmod(0o600)
        summary['private_error'] = str(error_path.relative_to(evidence))
    finally:
        for stream in sockets:
            stream.close()
        if owned:
            try:
                summary['compose']['cleanup_attempted'] = True
                receipt, _ = runner.command('compose-down', compose + ['down', '--volumes'], env, timeout=120)
                summary['compose']['cleanup_command'] = receipt
                summary['compose']['cleanup_passed'] = receipt['exit_code'] == 0 and not receipt['timeout']
                for kind, argv in [('containers', ['docker', 'ps', '--all', '--quiet', '--filter', project_filter]), ('networks', ['docker', 'network', 'ls', '--quiet', '--filter', project_filter]), ('volumes', ['docker', 'volume', 'ls', '--quiet', '--filter', project_filter])]:
                    check, remaining = runner.command('remaining-' + kind, argv, env, timeout=30)
                    empty = check['exit_code'] == 0 and not check['timeout'] and not remaining.strip()
                    summary['compose']['remaining_' + kind + '_empty'] = empty
                    summary['compose']['cleanup_passed'] &= empty
            except (Exception, KeyboardInterrupt) as exc:
                summary['compose']['cleanup_error_kind'] = type(exc).__name__
                summary['compose']['cleanup_passed'] = False
        else:
            summary['compose']['cleanup_not_needed'] = True
        if proof_attempted:
            # Restore the production profile even after a test/setup failure; this is
            # a rebuild, not a promise that m1 and proof have equal digests.
            try:
                clean_source(repo, args.source)
                summary['m1_restore']['attempted'] = True
                restore_env = {key: value for key, value in env.items() if not key.startswith(('WAMN_', 'PG'))}
                receipt, _ = runner.command('restore-m1', ['tools/build-components', 'm1'], restore_env, timeout=1800)
                summary['m1_restore']['command'] = receipt
                summary['m1_restore']['passed'] = receipt['exit_code'] == 0 and not receipt['timeout']
                summary['m1_restore']['required'] = not summary['m1_restore']['passed']
                summary['artifacts_after_m1'] = artifacts(repo)
            except (Exception, KeyboardInterrupt) as exc:
                summary['m1_restore']['error_kind'] = type(exc).__name__
        try:
            summary['final_source'] = clean_source(repo, args.source)
        except Exception as exc:
            summary['source_error_kind'] = type(exc).__name__
        summary['proof_passed'] = all(test['passed'] for test in summary['tests'])
        summary['verdict'] = 'pass' if summary['proof_passed'] and summary['compose']['cleanup_passed'] and summary['m1_restore']['passed'] and 'final_source' in summary else 'fail'
        summary['finished_unix_ns'] = time.time_ns()
        write_json(evidence/'summary.json', summary)
    print('virtualization: ' + summary['verdict'])
    return 0 if summary['verdict'] == 'pass' else 1


if __name__ == '__main__':
    sys.exit(main())
