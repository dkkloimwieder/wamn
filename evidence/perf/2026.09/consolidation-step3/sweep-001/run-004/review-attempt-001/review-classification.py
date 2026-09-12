#!/usr/bin/env python3
"""Review the completed fourth workspace run against its recorded predecessors."""
from collections import Counter
import copy
import gzip
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import sys

sys.dont_write_bytecode = True
run = Path(__file__).resolve().parent
root = run.parents[5]
prior_run = run.parent / 'run-003'
read = lambda path: json.loads(path.read_text())
value = read(run / 'classification-001/classified-failures.json')
original = copy.deepcopy(value)
raw = read(run / 'reduction-001/workspace-results.json')
prior_raw = read(prior_run / 'reduction-001/workspace-results.json')
prior = read(prior_run / 'classification-002/classified-failures.json')
source, old_source = value['source'], prior['source']
before = json.loads(gzip.decompress((run / 'source-before.json.gz').read_bytes()))
assert before == json.loads(gzip.decompress((run / 'source-after.json.gz').read_bytes()))
run_record = read(run / 'run.json')
stability = read(run / 'source-stability.json')
assert source == run_record['source_head'] == stability['head_before'] == stability['head_after']
assert raw['exit_code'] == run_record['exit_code'] == int((run / 'exit-code.txt').read_text())
assert hashlib.sha256((run / 'workspace.log').read_bytes()).hexdigest() == run_record['workspace_log_sha256']
assert hashlib.sha256(json.dumps(before, sort_keys=True, separators=(',', ':')).encode()).hexdigest() == stability['before_sha256'] == stability['after_sha256']
for entry in read(run / 'mapping-archives.json'):
    packed = (run / entry['archive']).read_bytes()
    unpacked = gzip.decompress(packed)
    assert len(packed) == entry['archive_bytes'] and hashlib.sha256(packed).hexdigest() == entry['archive_sha256']
    assert len(unpacked) == entry['original_bytes'] and hashlib.sha256(unpacked).hexdigest() == entry['original_sha256']
spec = importlib.util.spec_from_file_location('comparison', root / 'docs/perf/2026.09/receiving-postcommit/tools/compare_integrated_sweep.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)
reducer = module.load(module.REDUCER, 'retained_reducer')
causes = module.load(module.CAUSES, 'retained_causes')
comparison = module.compare(raw, prior_raw, reducer, causes)
comparison.update(source=source, reference_source=old_source,
    commands_equal=read(run / 'command.json') == read(prior_run / 'command.json'))
assert comparison['commands_equal']
assert not comparison['added_failures'] and not comparison['changed_causes'] and not comparison['absent_reference_failures']
identity = lambda row: tuple(row[key] for key in ['package', 'cargo_target', 'name'])
old_index = {identity(row): row for row in prior['failures']}
new_index = {identity(row): row for row in value['failures']}
exact = {tuple(row) for row in comparison['exact_identity_and_cause_matches']}
assert exact == set(old_index) == set(new_index)
source_cache = {}
source_checks = {}

def source_bytes(revision, path):
    key = (revision, path)
    if key not in source_cache:
        source_cache[key] = subprocess.check_output(['git', 'show', revision + ':' + path], cwd=root)
    return source_cache[key]

def check_source(path):
    previous = source_bytes(old_source, path)
    current = source_bytes(source, path)
    digest = hashlib.sha256(current).hexdigest()
    assert current == previous and digest == before[path]['sha256'], path
    source_checks[path] = {'path': path, 'previous_source': old_source, 'source': source,
        'bytes': len(current), 'sha256': digest, 'unchanged': True, 'captured_map_matches': True}

for key, row in new_index.items():
    old = old_index[key]
    row['classification'] = old['classification']
    row['prior_classification_record'] = '../../run-003/classification-002/classified-failures.json'
    row['prior_source'] = old_source
    row['run3_comparison'] = 'Exact identity and normalized diagnostic match.'
    for field in ['reviewed_explanation', 'execution_scope', 'required_inputs']:
        if field in old:
            row[field] = copy.deepcopy(old[field])
    source_keys = [x for x in [row.get('source_evidence'), old.get('source_evidence')] if x]
    source_keys += old.get('prior_source_evidence_keys', []) + old.get('source_evidence_keys', [])
    checked = set()
    for item in source_keys:
        match = re.fullmatch(r'(?:[0-9a-f]{8,40}:)?(.+):(\d+)', item)
        if match:
            path = match[1]
            check_source(path)
            checked.add(path)
    row['verified_source_paths'] = sorted(checked)
    if old.get('prior_source_evidence_keys'):
        row['prior_source_evidence_keys'] = old['prior_source_evidence_keys']

log_lines = {False: [reducer.ANSI.sub('', line) for line in (run / 'workspace.log').read_text().splitlines()],
             True: [reducer.ANSI.sub('', line) for line in (prior_run / 'workspace.log').read_text().splitlines()]}

def case_result(name, old=False, description=None):
    result = prior_raw if old else raw
    lines = log_lines[old]
    found = []
    for target in result['test_targets']:
        if target['category'] != 'test' or (description and target['description'] != description):
            continue
        start, end = target['running_log_line'], target['summary_log_line'] - 1
        for i in range(start, end):
            match = reducer.CASE.match(lines[i])
            if not match or match[1] != name:
                continue
            next_case = next((j for j in range(i + 1, end) if reducer.CASE.match(lines[j])), end)
            suffix = lines[i].split(' ... ', 1)[1]
            status = suffix if suffix in ['ok', 'FAILED', 'ignored'] else None
            completion = i + 1 if status else None
            if not status:
                ends = [(j + 1, lines[j]) for j in range(i + 1, next_case) if lines[j] in ['ok', 'FAILED', 'ignored']]
                assert len(ends) == 1, (name, ends)
                completion, status = ends[0]
            found.append({'description': target['description'], 'executable': target['executable'],
                'case_log_line': i + 1, 'completion_log_line': completion, 'status': status})
    assert len(found) == 1, (name, description, found)
    return found[0]

moves = read(prior_run / 'classification-002/app-test-moves.json')
assert moves['source'] == old_source
moves['previous_mapping_source'] = old_source
moves['source'] = source
moves['prior_mapping_record'] = '../../run-003/classification-002/app-test-moves.json'
for entry in list(moves['source_records']):
    data = source_bytes(entry['source'], entry['path'])
    assert hashlib.sha256(data).hexdigest() == entry['sha256']
    if entry['source'] == old_source:
        check_source(entry['path'])
        moves['source_records'].append({**entry, 'source': source})
for move in moves['moved_tests']:
    name = move['current_identity'][2]
    move['previous_run_result'] = case_result(name, old=True, description=move['current_target_description'])
    move['current_result'] = case_result(name, description=move['current_target_description'])
    assert move['previous_run_result']['status'] == move['current_result']['status'] == 'ok'
moves['interpretation'] = 'Each listed case reports a passing result under its current app test owner in both compared runs.'

baseline_moves = read(prior_run / 'classification-002/baseline-moved-failures.json')
for move in baseline_moves:
    row = new_index[tuple(move['current_identity'])]
    assert row['classification'] == 'missing_live_or_artifact_input'
    move.update(current_cause=row['cause'], current_log_line=row['diagnostic_start_log_line'],
        prior_mapping_record='../../run-003/classification-002/baseline-moved-failures.json')
    for key in move['source_evidence_keys']:
        match = re.fullmatch(r'(?:[0-9a-f]{8,40}:)?(.+):(\d+)', key)
        assert match, key
        check_source(match[1])

resolutions = []
for item in value['pending_review']:
    if item['kind'] == 'current_failure_cause':
        row = new_index[tuple(item['identity'])]
        resolutions.append({**item, 'classification': row['classification'], 'disposition': row['run3_comparison']})
    else:
        assert item['kind'] == 'absent_failure', item
        match = next(row for row in baseline_moves if row['previous_identity'] == item['identity'])
        resolutions.append({**item, 'current_identity': match['current_identity'], 'disposition': match['disposition']})
for reference, entries in value['removed_failure_dispositions'].items():
    for entry in entries:
        previous = next(row for row in prior['removed_failure_dispositions'][reference] if row['identity'] == entry['identity'])
        entry.update(copy.deepcopy(previous))
        if entry.get('current_identity'):
            assert tuple(entry['current_identity']) in new_index
            entry['source_change'] = 'baseline-moved-failures.json'

skip_key = lambda row: (row['target_description'], re.sub(r'-[0-9a-f]+$', '', Path(row['target_executable']).name), row['name'])
old_skips = Counter(skip_key(row) for row in prior['explicit_self_skips']['entries'])
new_skips = Counter(skip_key(row) for row in value['explicit_self_skips']['entries'])
comparison['executable_qualified_skips_equal'] = old_skips == new_skips
comparison['added_executable_qualified_skips'] = list((new_skips - old_skips).elements())
comparison['absent_executable_qualified_skips'] = list((old_skips - new_skips).elements())
assert old_skips == new_skips
value.update(classification_counts=dict(Counter(row['classification'] for row in value['failures'])),
    pending_review=[], classification_complete=True, source_review_resolutions=resolutions,
    comparison_to_previous_run='run3-comparison.json', moved_failure_identities='baseline-moved-failures.json',
    source_hash_comparison='source-comparison.json')
for old, new in zip(original['failures'], value['failures']):
    assert identity(old) == identity(new) and old['cause'] == new['cause'] and old['raw_diagnostics'] == new['raw_diagnostics']
assert original['explicit_self_skips'] == value['explicit_self_skips']
assert not raw['unresolved'] and not comparison['duplicate_failure_identities']
out = run / 'classification-002'
assert not out.exists()
out.mkdir()
write = lambda name, data: (out / name).write_text(json.dumps(data, indent=2) + '\n')
write('classified-failures.json', value)
write('reviewed-source-excerpts.json', read(run / 'classification-001/reviewed-source-excerpts.json'))
write('run3-comparison.json', comparison)
write('changed-failures.json', {'added_failures': comparison['added_failures'],
    'same_identity_changed_causes': comparison['changed_causes'], 'absent_reference_failures': comparison['absent_reference_failures']})
write('app-test-moves.json', moves)
write('baseline-moved-failures.json', baseline_moves)
write('explicit-self-skips.json', value['explicit_self_skips'])
write('pending-review.json', {'result_differences': [], 'parser_entries': []})
write('source-comparison.json', {'source': source, 'previous_source': old_source,
    'verified_source_files': list(source_checks.values()), 'changed_paths_since_run3': subprocess.check_output(
        ['git', 'diff', '--name-only', old_source, source], cwd=root, text=True).splitlines()})
counts = raw['counts']
summary = {'source': source, 'exit_code': raw['exit_code'], 'elapsed_seconds': run_record['elapsed_seconds'],
    'counts': counts, 'classification_counts': value['classification_counts'], 'classification_complete': True,
    'pending_review_entries': 0, 'unresolved_parser_entries': [], 'source_stable': True, 'tracked_files': len(before),
    'log_sha256': run_record['workspace_log_sha256'], 'previous_exact_failure_matches': len(exact),
    'added_failures': len(comparison['added_failures']), 'changed_failure_causes': len(comparison['changed_causes']),
    'absent_reference_failures': len(comparison['absent_reference_failures']),
    'moved_app_cases_with_passing_results': len(moves['moved_tests']), 'baseline_moved_failures': len(baseline_moves),
    'verified_source_files': len(source_checks), 'executable_qualified_skips_equal': old_skips == new_skips}
cell = lambda text: str(text).replace('|', '\\|').replace('\n', '<br>')
report = f'''Source `{source}` returned {raw['exit_code']} after {run_record['elapsed_seconds']} seconds. [Full output](../workspace.log).
The capture recorded {len(before):,} tracked files outside Beads with unchanged bytes and modes. HEAD also remained unchanged.

The parser lists {counts['test_failed']} named failures across {counts['test_failed_targets']} targets. It reports {counts['test_reported_passed']:,} passes and {counts['doctest_reported_passed']} passing doctests.
The reported passes include {value['explicit_self_skips']['count']} explicit skips. Those skipped operations did not run.
The skip count remains a lower bound because silent early returns can exist. The command filtered {counts['test_filtered_out']} schema-generation cases.

Against [run 3](run3-comparison.json), all {len(exact)} failures retain the same identity and normalized cause.
No failure identity was added or removed. No cause changed, and the exact retained commands match.
The same skip names, target executables, and multiplicities remain.
All {len(moves['moved_tests'])} [moved app cases](app-test-moves.json) report passing results under their current owners.
[Source comparison](source-comparison.json) verifies the reused files against both captured Git sources and the new source map.

The [starting baseline comparison](../reduction-001/baseline-comparison.json) retains every original failure and cause.
The {len(baseline_moves)} [earlier moved failures](baseline-moved-failures.json) still refuse absent live inputs.
Two earlier failed cases and one earlier explicit skip were removed by recorded source changes. Those removals are not passes.
[Recorded removals](../classification-001/known-test-removals.json), [step 1](../reduction-001/step1-comparison.json), and [step 2](../reduction-001/step2-comparison.json) remain available.

Every failure is classified, with no unresolved parser entry or cause review. This failed run does not establish completion of stage 3 or application tests.

| Classification | Failed tests |
| --- | ---: |
'''
report += '\n'.join(f'| {cell(k.replace("_", " "))} | {v} |' for k, v in value['classification_counts'].items())
report += '\n\n| Package / target | Failed case | Classification | Actual cause | Evidence |\n| --- | --- | --- | --- | --- |\n'
for row in value['failures']:
    report += '| ' + ' | '.join([cell(row['package'] + ' / ' + row['cargo_target']), cell(row['name']),
        cell(row['classification'].replace('_', ' ')), cell(row['cause']),
        f'[log {row["diagnostic_start_log_line"]}](../workspace.log#L{row["diagnostic_start_log_line"]})']) + ' |\n'
report += '\nEvery explicit skip follows. [Complete skip records](explicit-self-skips.json).\n\n| Target executable / description | Case | Explicit skip message | Evidence |\n| --- | --- | --- | --- |\n'
for row in value['explicit_self_skips']['entries']:
    executable = re.sub(r'-[0-9a-f]+$', '', Path(row['target_executable']).name)
    report += '| ' + ' | '.join([cell(executable + ' / ' + row['target_description']), cell(row['name']), cell(row['message']),
        f'[log {row["diagnostic_log_line"]}](../workspace.log#L{row["diagnostic_log_line"]})']) + ' |\n'
(out / 'report.md').write_text(report)
summary['report_sha256'] = hashlib.sha256((out / 'report.md').read_bytes()).hexdigest()
write('classification-summary.json', summary)
print(json.dumps(summary, indent=2))
