#!/usr/bin/env python3
"""Execute the generated TUI recipe with fresh disposable proof environments."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import time
import tomllib


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--tree', type=Path, required=True)
    parser.add_argument('--evidence-dir', type=Path, required=True)
    args = parser.parse_args()
    tree = args.tree.resolve(strict=True)
    evidence = args.evidence_dir.resolve()
    if evidence == tree or tree in evidence.parents:
        parser.error('evidence must be outside the source worktree')
    environment = {key: value for key, value in os.environ.items()
                   if not key.startswith(('WAMN_', 'WASH_', 'OTEL_', 'GIT_', 'PG'))
                   and key not in {'DATABASE_URL', 'DB_URL', 'CARGO_TARGET_DIR', 'KUBECONFIG'}}
    environment.update(RUSTUP_TOOLCHAIN='1.98.0', RUSTC_WRAPPER='', CARGO_BUILD_JOBS='2',
                       KUBECONFIG='/dev/null', GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_NOSYSTEM='1',
                       WAMN_IDENTITY_BINARY=str(tree / 'target/debug/wamn-identity'))

    def git(*arguments):
        return subprocess.check_output(['git', *arguments], cwd=tree, env=environment, text=True).strip()

    status = git('status', '--porcelain=v1', '--untracked-files=all', '--', '.',
                 ':(exclude).beads/issues.jsonl', ':(exclude).beads/interactions.jsonl')
    if status:
        parser.error('commit the recipe source before running it')
    source = git('rev-parse', 'HEAD')
    evidence.mkdir(parents=True, exist_ok=False)
    stages = []
    started = time.monotonic()
    result = {'passed': False, 'source': source, 'stages': stages}
    ui = tree / 'packages/receiving/ui'
    if ui.exists():
        parser.error('the recipe requires packages/receiving/ui to be absent')
    scaffold_owned = False
    scaffold_before = {}

    def write(name, value):
        (evidence / name).write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')

    def run(name, command, *, env=environment):
        command = [str(value) for value in command]
        stage = {'name': name, 'argv': command, 'exit_code': None}
        stages.append(stage)
        write('commands.json', stages)
        print('Stage: ' + name, flush=True)
        before = time.monotonic()
        with (evidence / (name + '.log')).open('w') as output:
            completed = subprocess.run(command, cwd=tree, env=env, stdout=output, stderr=subprocess.STDOUT)
        stage.update(exit_code=completed.returncode, elapsed_seconds=round(time.monotonic() - before, 3))
        write('commands.json', stages)
        if completed.returncode:
            raise RuntimeError(f'{name} exited {completed.returncode}; see its retained log')

    def materialize(name):
        run(name, [sys.executable, tree / 'docs/perf/2026.09/generated-tui-parity/tools/materialize.py',
                   '--tree', tree, '--evidence-dir', evidence / name])
        receipt = json.loads((evidence / name / 'result.json').read_text())
        if receipt['changed_paths']:
            raise RuntimeError('normal generation changed checked-in output; inspect the retained hashes')

    def scaffold_snapshot():
        return {str(path.relative_to(ui)): digest(path) for path in sorted(ui.rglob('*')) if path.is_file()}

    try:
        materialize('materialize')
        run('build-cli', ['cargo', 'build', '--locked', '--offline', '-p', 'wamn-ctl', '--bin', 'wamn'])
        run('build-identity', ['cargo', 'build', '--locked', '--offline', '-p', 'wamn-identity', '--bin', 'wamn-identity'])
        run('build-clients', ['cargo', 'build', '--locked', '--offline', '-p', 'wamn-host',
            '-p', 'wamn-scenario-worker', '-p', 'wamn-receiving-tui', '-p', 'wamn-generated-receiving-tui',
            '-p', 'wamn-generated-client-acme-receiving-tui', '-p', 'wamn-generated-wms-tui', '--bins'])
        run('shared-and-receiving-tests', ['cargo', 'test', '--locked', '--offline', '--no-fail-fast',
            '-p', 'wamn-client', '-p', 'wamn-client-tui', '-p', 'wamn-client-terminal',
            '-p', 'wamn-receiving-tui', '--all-targets', '--', '--include-ignored'])
        run('emitter-tests', ['cargo', 'test', '--locked', '--offline', '-p', 'wamn-schema-generator',
            '--test', 'tui_emitter', '--', '--include-ignored'])
        # The absent path and exact captured file set establish ownership of this scratch scaffold.
        scaffold_owned = True
        run('scaffold-create', [tree / 'target/debug/wamn', 'ui', 'scaffold', 'receiving', 'receiving.record_receipt'])
        scaffold_before = scaffold_snapshot()
        profile = tomllib.loads((tree / 'Cargo.toml').read_text())['profile']['dev']
        scaffold_environment = environment | {'CARGO_TARGET_DIR': str(tree / 'target'),
            'CARGO_PROFILE_DEV_DEBUG': str(profile['debug']),
            'CARGO_PROFILE_DEV_SPLIT_DEBUGINFO': str(profile['split-debuginfo'])}
        run('scaffold-tests', ['cargo', 'test', '--offline', '--manifest-path', ui / 'Cargo.toml',
            '--all-targets', '--', '--include-ignored'], env=scaffold_environment)
        materialize('regenerate-with-scaffold')
        after = scaffold_snapshot()
        if any(after.get(path) != value for path, value in scaffold_before.items()):
            raise RuntimeError('regeneration changed developer-owned scaffold source')
        if set(after) - set(scaffold_before) != {'Cargo.lock'}:
            raise RuntimeError('the scaffold created an unexpected file')
        shutil.copytree(ui, evidence / 'scaffold-source')
        write('scaffold-result.json', {'passed': True, 'source_unchanged': True,
                                       'source_sha256': scaffold_before, 'after_sha256': after})
        shutil.rmtree(ui)
        scaffold_owned = False
        run('operator-loop', [sys.executable, tree / 'docs/perf/2026.09/generated-tui-integration/live.py',
            '--tree', tree, '--evidence-dir', evidence / 'operator-loop'])
        run('receiving-composition', [sys.executable, tree / 'docs/perf/2026.09/generated-tui-parity/tools/live_environment.py',
            '--tree', tree, '--evidence-dir', evidence / 'receiving-composition'])
        run('wms-composition', [tree / 'tools/wms-cluster-journey-run', '--apply', '--prove-generated-tui',
            '--evidence-dir', evidence / 'wms-composition'])
        for name in ('operator-loop', 'receiving-composition'):
            if json.loads((evidence / name / 'result.json').read_text()).get('passed') is not True:
                raise RuntimeError(name + ' did not retain a passing receipt')
        for mode in ('success', 'partial'):
            if json.loads((evidence / 'wms-composition' / ('generated-tui-' + mode) / 'result.json').read_text()).get('passed') is not True:
                raise RuntimeError('WMS ' + mode + ' did not retain a passing receipt')
        result['passed'] = True
    except Exception as error:
        result['failure'] = str(error)
    finally:
        if scaffold_owned and ui.is_dir():
            # Leave changed or unknown files for inspection rather than deleting them.
            actual = scaffold_snapshot()
            if scaffold_before and all(actual.get(path) == value for path, value in scaffold_before.items()) \
                    and set(actual) - set(scaffold_before) <= {'Cargo.lock'}:
                shutil.copytree(ui, evidence / 'failed-scaffold-source', dirs_exist_ok=False)
                shutil.rmtree(ui)
            else:
                result['scaffold_preserved_for_inspection'] = True
        result['source_after'] = git('rev-parse', 'HEAD')
        result['status_after'] = git('status', '--porcelain=v1', '--untracked-files=all', '--', '.',
            ':(exclude).beads/issues.jsonl', ':(exclude).beads/interactions.jsonl')
        result['source_unchanged'] = result['source_after'] == source and not result['status_after']
        result['passed'] = result['passed'] and result['source_unchanged']
        result['elapsed_seconds'] = round(time.monotonic() - started, 3)
        write('result.json', result)
        files = sorted(path for path in evidence.rglob('*') if path.is_file() and path.name != 'SHA256SUMS')
        (evidence / 'SHA256SUMS').write_text(''.join(digest(path) + '  ' + str(path.relative_to(evidence)) + '\n' for path in files))
    print(json.dumps({'passed': result['passed'], 'evidence': str(evidence), 'failure': result.get('failure')}), flush=True)
    return 0 if result['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
