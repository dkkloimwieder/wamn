#!/usr/bin/env python3
"""One-use capture of the authorized focused app/operator test commands."""
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import tempfile
import time
from datetime import datetime, timezone

root = Path('/home/kaalin/dev/wamn')
evidence = Path(__file__).resolve().parent
expected_head = 'c4ef9db4a9214e44a18aefc0b69d520952b52cb0'
head = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip()
assert head == expected_head
selections = [
    ('generator-tui-emitter', ['-p', 'wamn-schema-generator', '--test', 'tui_emitter']),
    ('generator-materialize', ['-p', 'wamn-schema-generator', '--lib', 'materialize::tests::']),
    ('ctl-native-tui', ['-p', 'wamn-ctl', '--lib', 'dev::native_tui::tests::']),
    ('ctl-ui', ['-p', 'wamn-ctl', '--lib', 'ui::tests::']),
    ('ctl-watch-owner', ['-p', 'wamn-ctl', '--lib', 'dev::watch::tests::package_artifacts_map_to_their_first_semantic_owner']),
]
commands = [(name, ['cargo', 'test', '--locked', '--offline', *selection,
                    '--', '--nocapture', '--test-threads=1']) for name, selection in selections]
env = {key: value for key, value in os.environ.items()
       if not key.startswith(('WAMN_', 'OTEL_', 'GIT_', 'PG'))
       and key not in {'DATABASE_URL', 'CARGO_TARGET_DIR'}}
env.update(KUBECONFIG='/dev/null', GIT_CONFIG_GLOBAL='/dev/null', GIT_CONFIG_NOSYSTEM='1',
           RUSTUP_TOOLCHAIN='1.98.0', RUSTC_WRAPPER='', CARGO_BUILD_JOBS='2')
paths = [os.fsdecode(path) for path in subprocess.check_output(
    ['git', 'ls-files', '-z'], cwd=root).split(b'\0')
    if path and not path.startswith(b'.beads/')]

def write(name, value):
    (evidence / name).write_text(json.dumps(value, indent=2, sort_keys=True) + '\n')

def snapshot():
    result = {}
    for relative in paths:
        path = root / relative
        info = path.lstat()
        data = os.fsencode(os.readlink(path)) if stat.S_ISLNK(info.st_mode) else path.read_bytes()
        result[relative] = {'mode': oct(stat.S_IMODE(info.st_mode)),
                            'kind': 'symlink' if stat.S_ISLNK(info.st_mode) else 'file',
                            'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest()}
    return result

def digest(value):
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(',', ':')).encode()).hexdigest()

before = snapshot()
write('source-before.json', before)
write('commands.json', [{'name': name, 'argv': argv} for name, argv in commands])
write('source.json', {'head': head, 'tracked_files_excluding_beads': len(before),
                      'source_digest': digest(before), 'debug': True, 'jobs': 2,
                      'target_directory': str(root / 'target')})
(evidence / 'status-before.txt').write_bytes(subprocess.check_output(['git', 'status', '--porcelain=v1', '--untracked-files=all'], cwd=root))
write('environment.json', {'removed_prefixes': ['WAMN_', 'OTEL_', 'GIT_', 'PG'],
                            'removed_names': ['DATABASE_URL', 'CARGO_TARGET_DIR'],
                            'fixed': {key: env[key] for key in ['KUBECONFIG', 'GIT_CONFIG_GLOBAL',
                                      'GIT_CONFIG_NOSYSTEM', 'RUSTUP_TOOLCHAIN', 'RUSTC_WRAPPER', 'CARGO_BUILD_JOBS']},
                            'source': 'Same removal and arming policy as generated-tui-integration/tools/workspace.py.'})
results = []
try:
    with tempfile.TemporaryDirectory(prefix='wamn-app-workspace-tests-') as scratch:
        env['TMPDIR'] = scratch
        for key in ('HELM_CACHE_HOME', 'HELM_CONFIG_HOME', 'HELM_DATA_HOME'):
            env[key] = str(Path(scratch) / key.lower())
        for index, (name, argv) in enumerate(commands):
            assert snapshot() == before, 'source changed before ' + name
            write('status.json', {'state': 'running', 'command': name, 'index': index,
                                  'completed': len(results), 'pid': os.getpid()})
            started = time.monotonic()
            started_utc = datetime.now(timezone.utc).isoformat()
            with (evidence / (name + '.stdout.log')).open('xb') as out, (evidence / (name + '.stderr.log')).open('xb') as err:
                process = subprocess.run(argv, cwd=root, env=env, stdout=out, stderr=err)
            command_seconds = time.monotonic() - started
            ended_utc = datetime.now(timezone.utc).isoformat()
            out = (evidence / (name + '.stdout.log')).read_text(errors='replace')
            err = (evidence / (name + '.stderr.log')).read_text(errors='replace')
            summaries = re.findall(r'test result: (?:ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out;', out)
            counts = [dict(zip(['passed', 'failed', 'ignored', 'measured', 'filtered'], map(int, match))) for match in summaries]
            cases = [{'name': match[0], 'status': match[1], 'detail': match[2]}
                     for match in re.findall(r'^test (\S+) \.\.\. (ok|FAILED|ignored)([^\n]*)', out, re.M)]
            compile_failure = process.returncode != 0 and (not summaries or 'could not compile' in err)
            unchanged = snapshot() == before
            record = {'name': name, 'argv': argv, 'cwd': str(root), 'exit_code': process.returncode,
                       'started_utc': started_utc, 'ended_utc': ended_utc,
                       'seconds': round(command_seconds, 3), 'summaries': counts, 'cases': cases,
                       'compile_failure': compile_failure, 'source_unchanged': unchanged,
                       'stdout': name + '.stdout.log', 'stderr': name + '.stderr.log'}
            results.append(record)
            write(name + '-result.json', record)
            write('results.json', {'complete': False, 'results': results})
            assert unchanged, 'source changed during ' + name
            if compile_failure:
                write('status.json', {'state': 'stopped_on_compile_failure', 'command': name,
                                      'completed': len(results), 'not_run': [item[0] for item in commands[index + 1:]]})
                break
            assert len(counts) == 1 and sum(counts[0][key] for key in ['passed', 'failed', 'ignored']) > 0, 'no selected test cases: ' + name
        else:
            write('status.json', {'state': 'complete', 'completed': len(results)})
finally:
    after = snapshot()
    head_after = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip()
    write('source-stability.json', {'head_before': head, 'head_after': head_after, 'head_unchanged': head == head_after, 'before_sha256': digest(before), 'after_sha256': digest(after), 'tracked_files': len(before), 'bytes_and_modes_unchanged': before == after})
    write('source-after.json', after)
    (evidence / 'status-after.txt').write_bytes(subprocess.check_output(['git', 'status', '--porcelain=v1', '--untracked-files=all'], cwd=root))
    complete = len(results) == len(commands) and all(not result['compile_failure'] for result in results)
    write('results.json', {'complete': complete, 'passed': complete and all(result['exit_code'] == 0 for result in results),
                           'source_unchanged': before == after, 'results': results,
                           'not_run': [name for name, _ in commands[len(results):]]})
raise SystemExit(0 if len(results) == len(commands) and all(result['exit_code'] == 0 for result in results) else 1)
