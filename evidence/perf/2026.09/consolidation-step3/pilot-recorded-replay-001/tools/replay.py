#!/usr/bin/env python3
"""Replay existing pilot records with the already-built native grader."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import time

REPOSITORY = Path('/home/kaalin/dev/wamn')
ROOT = REPOSITORY / 'docs/perf/2026.09/consolidation-step3/pilot-recorded-replay-001'
BINARY = REPOSITORY / 'target/debug/wamn-gates'
NAMES = [f'{number}-claude-dock-appointments' for number in ['001', '010', '011', '012', '030', '031', '032']]
SOURCES = REPOSITORY / 'docs/experiments/agent-authoring'
REQUIRED = ['task.json', 'run.json', 'checklist.json', 'fixture/steps.json', 'fixture/SCENARIO.md', 'grade/dev.out', 'grade/http.jsonl', 'grade/results.json']

def save(path, value):
    path.write_text(json.dumps(value, indent=2) + '\n')

def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        while block := stream.read(1024 * 1024):
            h.update(block)
    return {'sha256': h.hexdigest(), 'bytes': path.stat().st_size, 'mode': stat.S_IMODE(path.stat().st_mode)}

def git(*args):
    return subprocess.check_output(['git', '-C', str(REPOSITORY), *args], text=True).strip()

def copy_inputs(name, kind):
    source = SOURCES / name
    destination = ROOT / kind / name
    destination.mkdir(parents=True, exist_ok=False)
    inputs, missing = {}, []
    files = set(REQUIRED)
    files.update(str(path.relative_to(source)) for path in (source / 'grade').glob('*.out'))
    for rel in sorted(files):
        path = source / rel
        if not path.is_file():
            missing.append(rel)
            continue
        target = destination / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(path, target)
        inputs[rel] = {'source': str(path.relative_to(REPOSITORY)), **digest(path)}
        assert digest(target) == digest(path)
    return destination, inputs, missing

def run(name, kind, cache, state):
    destination, inputs, missing = copy_inputs(name, kind)
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(('WAMN', 'PG', 'OTEL')) and key != 'DATABASE_URL'}
    env.update(XDG_CACHE_HOME=str(cache), XDG_STATE_HOME=str(state))
    argv = [str(BINARY), 'agent-pilot-grade', '--replay', str(destination)]
    started = time.time()
    with (destination / 'native.stdout').open('wb') as stdout, (destination / 'native.stderr').open('wb') as stderr:
        result = subprocess.run(argv, cwd=REPOSITORY, env=env, stdout=stdout, stderr=stderr, timeout=120)
    elapsed = time.time() - started
    original = json.loads((destination / 'checklist.json').read_bytes())
    result_path = destination / 'checklist-replay.json'
    replay = json.loads(result_path.read_bytes()) if result_path.exists() else None
    original_steps = {row['id']: row for row in original['steps']}
    replay_steps = {row['id']: row for row in replay['steps']} if replay else {}
    step_changes = []
    for identity in sorted(set(original_steps) | set(replay_steps)):
        before, after = original_steps.get(identity), replay_steps.get(identity)
        if not replay:
            break
        if before is None or after is None or before['pass'] != after['pass']:
            step_changes.append({'id': identity, 'original': before, 'replay': after})
    preserved = {rel: digest(destination / rel) == {key: record[key] for key in ['sha256', 'bytes', 'mode']}
                 for rel, record in inputs.items()}
    outcome = original.get('outcome')
    comparable = {key: {'original': original.get(key), 'replay': replay.get(key), 'equal': original.get(key) == replay.get(key)}
                  for key in ['loop', 'paths', 'checks', 'fences', 'teardown']} if replay else {}
    record = {
        'input_kind': kind, 'run': name, 'argv': argv, 'cwd': str(REPOSITORY),
        'environment': {'removed_prefixes': ['WAMN', 'PG', 'OTEL'], 'removed_names': ['DATABASE_URL'],
                        'XDG_CACHE_HOME': str(cache), 'XDG_STATE_HOME': str(state)},
        'exit_code': result.returncode, 'elapsed_seconds': elapsed,
        'original_outcome': outcome, 'replay_outcome': replay.get('outcome') if replay else None,
        'outcome_equal': replay.get('outcome') == outcome if replay else None,
        'original_step_count': len(original_steps), 'replay_step_count': len(replay_steps) if replay else None,
        'changed_step_verdicts': step_changes, 'section_comparison': comparable,
        'original_inputs': inputs, 'missing_tracked_inputs': missing,
        'copied_original_inputs_unchanged': preserved,
        'missing_worktree_and_contracts': not (SOURCES / name / 'worktree').exists(),
        'stderr': (destination / 'native.stderr').read_text(),
    }
    save(destination / 'result.json', record)
    print(json.dumps({key: record[key] for key in ['input_kind', 'run', 'exit_code', 'original_outcome', 'replay_outcome', 'outcome_equal', 'changed_step_verdicts']}), flush=True)
    return record

before = {'head': git('rev-parse', 'HEAD'), 'grader_source_commit': git('log', '-1', '--format=%H', '--', 'tests/integration/src/agent_pilot'),
          'binary': digest(BINARY)}
original_hashes = {str(path.relative_to(REPOSITORY)): digest(path) for name in NAMES for path in (SOURCES / name).rglob('*') if path.is_file()}
save(ROOT / 'source-before.json', before)
tracked = [run(name, 'tracked', ROOT / 'unused-lookups/cache', ROOT / 'unused-lookups/state') for name in NAMES]

# Only these exact historical grading files were authorized for read-only recovery.
recovered = []
recovery = []
for name in NAMES:
    number = name.split('-')[0]
    have_grade = False
    for kind, source_root in [('state', Path.home() / '.local/state/wamn-pilot-grading'), ('cache', Path.home() / '.cache/wamn-pilot/grading')]:
        for filename in ['task.json', 'steps.json']:
            source = source_root / number / filename
            item = {'run': name, 'source': str(source), 'present': source.is_file()}
            if source.is_file():
                target = ROOT / 'recovered-lookups' / kind / ('wamn-pilot-grading' if kind == 'state' else 'wamn-pilot/grading') / number / filename
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy2(source, target)
                item.update(digest(source), copied_to=str(target.relative_to(ROOT)))
                assert digest(source) == digest(target)
                if filename == 'task.json':
                    have_grade |= 'grade' in json.loads(source.read_bytes())
            recovery.append(item)
    if have_grade:
        recovered.append(run(name, 'recovered', ROOT / 'recovered-lookups/cache', ROOT / 'recovered-lookups/state'))
save(ROOT / 'recovery.json', recovery)
after = {'head': git('rev-parse', 'HEAD'), 'binary': digest(BINARY),
         'original_records_unchanged': all(digest(REPOSITORY / path) == record for path, record in original_hashes.items())}
save(ROOT / 'source-after.json', after)
save(ROOT / 'result.json', {'source_before': before, 'source_after': after, 'binary_unchanged': before['binary'] == after['binary'],
                          'tracked_runs': tracked, 'recovered_runs': recovered, 'recovery': recovery,
                          'no_cargo_services_or_agent_launch': True,
                          'limits': ['The original worktrees and their published contracts are absent and were not reconstructed.',
                                     'Historical expected failures remain failures.',
                                     'The tracked and recovered-input runs are separate copies with separate output files.']})
