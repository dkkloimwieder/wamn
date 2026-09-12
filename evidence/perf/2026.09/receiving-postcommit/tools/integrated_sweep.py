#!/usr/bin/env python3
"""Capture one final workspace sweep from an isolated integrated checkout."""
import argparse
import datetime
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[5]
MONTH = ROOT / 'docs/perf/2026.09'
RETAINED = MONTH / 'wasmcloud-2-9-cutover/workspace-integration-preparation-001/run.py'
COMMAND = [
    'cargo', 'test', '--workspace', '--no-fail-fast', '--locked', '--offline', '--',
    '--include-ignored', '--nocapture', '--test-threads=1',
    '--skip', 'regenerate_checked_in_journey_schema',
    '--skip', 'regenerate_checked_in_dev_config_schema',
]


def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def write(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source-tree', type=Path, required=True)
    parser.add_argument('--expected-main', required=True)
    parser.add_argument('--evidence-dir', type=Path, required=True)
    parser.add_argument('--apply', action='store_true', help='Run only after source integration and the lane boundary.')
    args = parser.parse_args()
    tree = args.source_tree.resolve(strict=True)
    evidence = args.evidence_dir.resolve()
    lane_root = Path('/home/kaalin/.cache/wamn-lanes').resolve()
    if lane_root not in tree.parents or tree == ROOT:
        parser.error('Use a dedicated verification worktree under /home/kaalin/.cache/wamn-lanes.')
    if not (tree / 'tests/integration/src/route_authentication_live/postcommit.rs').is_file():
        parser.error('The integrated checkout lacks the postcommit proof anchor.')
    if not re.fullmatch('[0-9a-f]{40}', args.expected_main):
        parser.error('--expected-main requires the full integrated commit.')
    if evidence.exists() or evidence == tree or tree in evidence.parents:
        parser.error('Use a fresh evidence directory outside the source worktree.')
    evidence.relative_to(MONTH / 'receiving-postcommit')
    retained = load(RETAINED, 'retained_workspace_runner')
    before = retained.source_receipt(tree)
    git_env = retained.clean_environment(os.environ)
    git_env.update(GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_NOSYSTEM='1')
    main_head = subprocess.check_output(['git', 'rev-parse', 'refs/heads/main'], cwd=tree, env=git_env, text=True).strip()
    if before['status'] or before['head'] != args.expected_main or main_head != args.expected_main:
        parser.error('The clean verification checkout and local main must name the expected integrated commit.')
    plan = {'source': before['head'], 'source_tree': str(tree), 'command': COMMAND,
            'evidence_dir': str(evidence), 'explicitly_armed_names': [],
            'live_scope': 'Retain missing live fixtures as refusals or explicit self-skips. The separate PG18 observer receipt supplies its own evidence.'}
    if not args.apply:
        print(json.dumps(plan, indent=2))
        return 0
    evidence.mkdir(parents=True)
    write(evidence / 'plan.json', plan)
    write(evidence / 'source-before.json', before)
    write(evidence / 'command.json', COMMAND)
    (evidence / 'source.txt').write_text(before['head'] + '\n')
    write(evidence / 'tools.json', [retained_file(path) for path in (Path(__file__), RETAINED)])
    env = retained.clean_environment(os.environ)
    for key in ('NATS_URL', 'CARGO', 'CARGO_ENCODED_RUSTFLAGS', 'RUSTFLAGS'):
        env.pop(key, None)
    scratch_owner = tempfile.TemporaryDirectory(prefix='receiving-postcommit-final-sweep-', dir=lane_root)
    scratch = Path(scratch_owner.name)
    try:
        env.update(KUBECONFIG='/dev/null', GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_NOSYSTEM='1',
                   RUSTUP_TOOLCHAIN='1.98.0', RUSTC_WRAPPER='', CARGO_BUILD_JOBS='2',
                   CARGO_TERM_COLOR='never', TMPDIR=str(scratch))
        for key in ('HELM_CACHE_HOME', 'HELM_CONFIG_HOME', 'HELM_DATA_HOME'):
            env[key] = str(scratch / key.lower())
        write(evidence / 'environment-names.json', {
            'ambient_names': sorted(os.environ),
            'removed_ambient_names': sorted(set(os.environ) - set(env)),
            'child_environment_names': sorted(env),
            'explicitly_armed_names': [], 'values_recorded': False,
            'scope': 'No live fixture is armed by this sweep. Separately executed live receipts remain separate.',
        })
        started = time.monotonic()
        code = 125
        run = {'started_utc': datetime.datetime.now(datetime.timezone.utc).isoformat()}
        try:
            with (evidence / 'workspace.log').open('wb') as log:
                code = subprocess.run(COMMAND, cwd=tree, env=env, stdout=log, stderr=subprocess.STDOUT).returncode
        finally:
            after = retained.source_receipt(tree)
            write(evidence / 'source-after.json', after)
            stable = before == after
            write(evidence / 'source-stability.json', {'same_commit_and_clean_tree': stable})
            run.update(exit_code=code, elapsed_seconds=time.monotonic() - started,
                       source_stable=stable, log_sha256=hashlib.sha256((evidence / 'workspace.log').read_bytes()).hexdigest())
            write(evidence / 'run.json', run)
            (evidence / 'exit-code.txt').write_text(str(code) + '\n')
        return (code if code >= 0 else 128 - code) or (0 if stable else 1)
    finally:
        cleanup = {'scratch': str(scratch)}
        try:
            scratch_owner.cleanup()
        except OSError as error:
            cleanup['error'] = str(error)
            raise
        finally:
            cleanup['removed'] = not scratch.exists()
            cleanup['verdict'] = 'pass' if cleanup['removed'] else 'fail'
            write(evidence / 'scratch-cleanup.json', cleanup)


def retained_file(path):
    return {'path': str(path.resolve()), 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}


if __name__ == '__main__':
    raise SystemExit(main())
