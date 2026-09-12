#!/usr/bin/env python3
"""Run one old generated UPDATE against the passing additive-column regression."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import stat
import subprocess
import sys

OLD_REVISION = 'b8c52797'
TEST = 'receiving_data_access::tests::generated_update_ignores_ungranted_additive_columns'
PACKAGES = {'base': 'receiving', 'overlay': 'client_acme_receiving'}
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--tree', type=Path, required=True)
parser.add_argument('--package', choices=PACKAGES, required=True)
parser.add_argument('--positive-evidence-dir', type=Path, required=True)
parser.add_argument('--evidence-dir', type=Path, required=True)
args = parser.parse_args()
tree = args.tree.resolve(strict=True)
positive = args.positive_evidence_dir.resolve(strict=True)
evidence = args.evidence_dir.resolve()
tools = Path(__file__).resolve().parent
root = tools.parent
relative = f'packages/{PACKAGES[args.package]}/generated/sql/purchase_order/update.sql'
path = tree / relative


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def read_json(path):
    return json.loads(path.read_text())


def save(name, value):
    (evidence / name).write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')


def git(*arguments):
    env = {key: value for key, value in os.environ.items() if not key.startswith('GIT_')}
    env.update(GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_NOSYSTEM='1')
    return subprocess.check_output(['git', *arguments], cwd=tree, env=env)


def source_identity():
    # Match the existing capture.py receipt; exclude its evidence-only paths.
    paths = set(git('diff', '--name-only', 'HEAD', '-z').decode().split('\0'))
    paths.update(git('ls-files', '--others', '--exclude-standard', '-z').decode().split('\0'))
    files = {}
    for name in sorted(paths - {''}):
        if not name.startswith(('.beads/', 'docs/perf/', 'docs/poc/')):
            item = tree / name
            files[name] = sha(item.read_bytes()) if item.is_file() else None
    return {'head': git('rev-parse', 'HEAD').decode().strip(), 'changed_source_sha256': files}


require((tree / '.git').is_file(), 'Use the explicit inactive repair worktree')
require(evidence.is_relative_to(root) and evidence != root and not evidence.is_relative_to(tools),
        'Use a new evidence directory below receiving-update-projection')
require(positive.is_relative_to(root) and positive != evidence,
        'Name a separate passing regression receipt below receiving-update-projection')
evidence.mkdir(exist_ok=False)
save('executor-command.json', {'argv': sys.argv, 'harness_sha256': sha(Path(__file__).read_bytes())})
report = {'package': args.package, 'path': relative, 'positive_evidence': str(positive),
          'status': 'incomplete', 'detector': 'not-run', 'restoration': 'not-needed'}
original = metadata = mutant = None
mutated = False
try:
    before = path.lstat()
    require(stat.S_ISREG(before.st_mode), 'The generated UPDATE must be a regular file')
    metadata = {'mode': stat.S_IMODE(before.st_mode), 'atime_ns': before.st_atime_ns,
                'mtime_ns': before.st_mtime_ns}
    original = path.read_bytes()
    metadata['sha256'] = sha(original)
    (evidence / 'original-update.sql').write_bytes(original)
    save('original-metadata.json', metadata)
    require(read_json(positive / 'result.json')['exit_code'] == 0, 'Positive capture did not pass')
    require(read_json(positive / 'regression-result.json') ==
            {'exit_code': 0, 'exact_test_ran': True, 'outcome': 'ok'}, 'Named positive test did not pass')
    cleanup = read_json(positive / 'cleanup.json')
    require(cleanup.get('verdict') == 'pass' and cleanup.get('configuration_absent') is True
            and cleanup.get('listener_stopped') is True, 'Positive cleanup did not pass')
    invocation = read_json(positive / 'regression-command.json')
    require(invocation['cwd'] == str(tree) and TEST in invocation['argv'],
            'Positive receipt names a different worktree or test')
    current = source_identity()
    require(current == read_json(positive / 'source.json'),
            'Current source differs from the named passing positive receipt')
    save('source-before.json', current)
    git_command = ['git', 'show', f'{OLD_REVISION}:{relative}']
    mutant = git(*git_command[1:])
    require(mutant.count(b'RETURNING model.*') == 1 and mutant != original,
            'Old revision must contain the distinct original wildcard UPDATE')
    (evidence / 'mutant-update.sql').write_bytes(mutant)
    save('mutation.json', {'git_command': git_command, 'original_sha256': sha(original),
                           'mutant_sha256': sha(mutant), 'only_source_path': relative})
    require(path.read_bytes() == original and stat.S_IMODE(path.stat().st_mode) == metadata['mode'],
            'Generated UPDATE changed before mutation')
    path.write_bytes(mutant)
    mutated = True
    regression = evidence / 'regression'
    command = [sys.executable, str(tools / 'capture.py'), '--tree', str(tree),
               '--evidence-dir', str(regression), '--', sys.executable,
               str(tools / 'regression_pg18.py'), '--tree', str(tree), '--evidence-dir', str(regression)]
    save('control-command.json', {'argv': command})
    process = subprocess.run(command, cwd=tree)
    report['capture_exit_code'] = process.returncode
    outcome = read_json(regression / 'regression-result.json')
    cleanup = read_json(regression / 'cleanup.json')
    stderr = (regression / 'regression.stderr.log').read_text()
    expected_context = f'execute exact {args.package} UPDATE with an ungranted additive field'
    matched = (process.returncode == 101
               and read_json(regression / 'result.json')['exit_code'] == 101
               and outcome == {'exit_code': 101, 'exact_test_ran': True, 'outcome': 'FAILED'}
               and expected_context in stderr
               and 'permission denied for table purchase_order' in stderr)
    report['expected_positive_path_context'] = expected_context
    report['cleanup'] = cleanup
    if matched and cleanup.get('verdict') == 'pass' and cleanup.get('configuration_absent') is True \
            and cleanup.get('listener_stopped') is True:
        report.update(status='killed', detector='intended-permission-failure')
    elif not outcome.get('exact_test_ran'):
        report.update(status='incomplete', detector='test-did-not-run')
    else:
        report.update(status='failed', detector='unexpected-test-or-cleanup-result')
except Exception as error:
    report.update(status='incomplete', error=str(error))
finally:
    if original is not None:
        try:
            require((evidence / 'original-update.sql').read_bytes() == original, 'Retained backup changed')
            current_bytes = path.read_bytes()
            require(current_bytes == (mutant if mutated else original)
                    and stat.S_IMODE(path.stat().st_mode) == metadata['mode'],
                    'Refusing to overwrite unexpected SQL bytes or mode during restoration')
            if mutated:
                path.write_bytes(original)
                path.chmod(metadata['mode'])
            require(sha(path.read_bytes()) == metadata['sha256'], 'Restored SQL bytes differ')
            os.utime(path, ns=(metadata['atime_ns'], metadata['mtime_ns']))
            restored = path.stat()
            require(stat.S_IMODE(restored.st_mode) == metadata['mode']
                    and restored.st_atime_ns == metadata['atime_ns']
                    and restored.st_mtime_ns == metadata['mtime_ns'], 'Restored SQL metadata differs')
            report['restoration'] = 'exact-bytes-mode-atime-mtime'
        except Exception as error:
            report.update(status='incomplete', restoration='failed', restoration_error=str(error))
    save('control-result.json', report)
print(json.dumps(report, sort_keys=True), flush=True)
raise SystemExit(0 if report['status'] == 'killed' else 1)
