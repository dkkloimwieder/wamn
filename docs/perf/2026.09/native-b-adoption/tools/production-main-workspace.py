#!/usr/bin/env python3
"""Capture the B main sweep, permitting recorded proof evidence and Beads state.

Small adaptation of docs/perf/2026.09/generated-tui-integration/tools/workspace.py.
The retained cutover runner supplies the identical sweep argv and environment
filter; comparison remains the existing compare-workspace.py's separate job.
"""
import argparse
import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import runpy
import stat
import subprocess
import sys
import tempfile
import time

sys.dont_write_bytecode = True
TREE = Path('/home/kaalin/dev/wamn')
MONTH = TREE / 'docs/perf/2026.09'
EVIDENCE_ROOT = MONTH / 'native-b-adoption'
ADAPTED_FROM = MONTH / 'generated-tui-integration/tools/workspace.py'
RETAINED = MONTH / 'wasmcloud-2-9-cutover/workspace-integration-preparation-001/run.py'


def write(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')


def utc():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def identity(path):
    try:
        info = path.lstat()
    except FileNotFoundError:
        return {'kind': 'missing'}
    digest = hashlib.sha256()
    if stat.S_ISLNK(info.st_mode):
        digest.update(os.fsencode(os.readlink(path)))
        kind = 'symlink'
    elif stat.S_ISREG(info.st_mode):
        with path.open('rb') as source:
            for chunk in iter(lambda: source.read(1024 * 1024), b''):
                digest.update(chunk)
        kind = 'file'
    else:
        raise ValueError(f'Unsupported source input type: {path}')
    return {'kind': kind, 'mode': stat.S_IMODE(info.st_mode),
            'size': info.st_size, 'sha256': digest.hexdigest()}


def status_entries(raw):
    entries = []
    records = iter(raw.split('\0'))
    for record in records:
        if not record:
            continue
        entry = {'status': record[:2], 'path': record[3:]}
        if 'R' in entry['status'] or 'C' in entry['status']:
            entry['original_path'] = next(records)
        entries.append(entry)
    return entries


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--expected-source', required=True)
    parser.add_argument('--evidence-dir', type=Path, required=True)
    args = parser.parse_args()
    if not re.fullmatch('[0-9a-f]{40}', args.expected_source):
        parser.error('--expected-source requires the full integrated commit')
    evidence = args.evidence_dir.resolve()
    if EVIDENCE_ROOT not in evidence.parents or evidence.exists() or args.evidence_dir.is_symlink():
        parser.error('Use a fresh directory under main docs/perf/2026.09/native-b-adoption')
    output_prefix = evidence.relative_to(TREE).as_posix() + '/'
    retained = runpy.run_path(str(RETAINED))
    command = retained['COMMAND']
    ambient = dict(os.environ)
    env = retained['clean_environment'](ambient)
    for name in ('NATS_URL', 'CARGO', 'RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS'):
        env.pop(name, None)
    controlled = {'KUBECONFIG': '/dev/null', 'GIT_CONFIG_GLOBAL': '/dev/null',
                  'GIT_CONFIG_NOSYSTEM': '1', 'RUSTUP_TOOLCHAIN': '1.98.0',
                  'RUSTC_WRAPPER': '', 'CARGO_BUILD_JOBS': '2', 'CARGO_TERM_COLOR': 'never'}
    env.update(controlled)
    git_env = dict(env, GIT_OPTIONAL_LOCKS='0')

    def git(*arguments):
        return os.fsdecode(subprocess.check_output(['git', *arguments], cwd=TREE, env=git_env))

    def excluded(path):
        return path == '.beads' or path.startswith('.beads/') or path.startswith(output_prefix)

    def receipt():
        raw = git('status', '--porcelain=v1', '-z', '--untracked-files=all')
        entries = status_entries(raw)
        paths = set(git('ls-files', '--cached', '--others', '--exclude-standard', '-z').split('\0'))
        return {'head': git('rev-parse', 'HEAD').strip(),
                'branch': git('symbolic-ref', '--quiet', '--short', 'HEAD').strip(),
                'status_porcelain_v1_z': raw, 'status_entries': entries,
                'scoped_status_entries': [row for row in entries if not excluded(row['path'])
                                          or ('original_path' in row and not excluded(row['original_path']))],
                'inputs': {path: identity(TREE / path) for path in sorted(paths) if path and not excluded(path)},
                'capture_tools': {str(path): identity(path) for path in (Path(__file__), ADAPTED_FROM, RETAINED)},
                'scope': 'Tracked and nonignored untracked inputs; .beads and this output directory excluded from hashes and stability. Raw status retains all reported paths. No clean-source claim.'}

    before = receipt()
    if before['head'] != args.expected_source or before['branch'] != 'main':
        parser.error('main must name the supplied full expected commit')
    disallowed = []
    for row in before['status_entries']:
        paths = [row['path']] + ([row['original_path']] if 'original_path' in row else [])
        if all(path == '.beads' or path.startswith('.beads/') for path in paths):
            continue
        if row['status'] == '??' and (row['path'].startswith('docs/perf/')
                                      or row['path'] == 'docs/poc/generated-tui-spec.md'):
            continue
        disallowed.append(row)
    if disallowed:
        parser.error('Disallowed source changes: ' + json.dumps(disallowed))
    evidence.mkdir(parents=True, exist_ok=False)
    write(evidence / 'source-before.json', before)
    write(evidence / 'command.json', command)
    (evidence / 'source.txt').write_text(before['head'] + '\n')
    code, stable = 125, False
    started = time.monotonic()
    run = {'started_utc': utc(), 'exit_code': None, 'source_tree': str(TREE),
           'expected_source': args.expected_source, 'target_policy': 'CARGO_TARGET_DIR removed; main default target'}
    write(evidence / 'run.json', run)
    try:
        with tempfile.TemporaryDirectory(prefix='native-b-main-sweep-', dir='/tmp') as scratch:
            env['TMPDIR'] = scratch
            for name in ('HELM_CACHE_HOME', 'HELM_CONFIG_HOME', 'HELM_DATA_HOME'):
                env[name] = str(Path(scratch) / name.lower())
            write(evidence / 'environment-names.json', {
                'ambient_names': sorted(ambient),
                'removed_ambient_names': sorted(set(ambient) - set(env)),
                'controlled_names': sorted([*controlled, 'TMPDIR', 'HELM_CACHE_HOME', 'HELM_CONFIG_HOME', 'HELM_DATA_HOME']),
                'child_environment_names': sorted(env),
                'explicitly_armed_names': [], 'values_recorded': False,
                'scope': 'No live fixture is armed by this runner; names do not prove test execution.'})
            with (evidence / 'workspace.log').open('wb') as log:
                code = subprocess.run(command, cwd=TREE, env=env, stdout=log, stderr=subprocess.STDOUT).returncode
    except KeyboardInterrupt:
        code = 130
        run['wrapper_error_kind'] = 'KeyboardInterrupt'
    except OSError as error:
        run['wrapper_error_kind'] = type(error).__name__
    finally:
        try:
            after = receipt()
            write(evidence / 'source-after.json', after)
            old, new = before['inputs'], after['inputs']
            stability = {'same_head': before['head'] == after['head'],
                         'same_branch': before['branch'] == after['branch'],
                         'same_scoped_status': before['scoped_status_entries'] == after['scoped_status_entries'],
                         'same_capture_tools': before['capture_tools'] == after['capture_tools'],
                         'added_inputs': sorted(new.keys() - old.keys()),
                         'removed_inputs': sorted(old.keys() - new.keys()),
                         'changed_inputs': sorted(path for path in old.keys() & new.keys() if old[path] != new[path])}
            stable = all(stability[key] for key in ('same_head', 'same_branch', 'same_scoped_status', 'same_capture_tools')) and not any(stability[key] for key in ('added_inputs', 'removed_inputs', 'changed_inputs'))
            stability['stable_with_declared_exclusions'] = stable
            write(evidence / 'source-stability.json', stability)
        except (OSError, ValueError, subprocess.CalledProcessError) as error:
            run['source_after_error_kind'] = type(error).__name__
            write(evidence / 'source-stability.json', {'stable_with_declared_exclusions': False,
                                                      'verification_error_kind': type(error).__name__})
        run.update(finished_utc=utc(), wall_seconds=time.monotonic() - started,
                   exit_code=code, source_stable=stable)
        write(evidence / 'run.json', run)
        (evidence / 'exit-code.txt').write_text(str(code) + '\n')
    return (code if code >= 0 else 128 - code) or (0 if stable else 1)


if __name__ == '__main__':
    raise SystemExit(main())
