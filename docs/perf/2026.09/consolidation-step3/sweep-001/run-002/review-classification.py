#!/usr/bin/env python3
"""Complete the source review for the second stage 3 test run."""
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
prior_run = run.parent / 'run-001'
read = lambda path: json.loads(path.read_text())
value = read(run / 'classification-001/classified-failures.json')
original = copy.deepcopy(value)
raw = read(run / 'reduction-001/workspace-results.json')
prior_raw = read(prior_run / 'reduction-001/workspace-results.json')
prior = read(prior_run / 'classification-002/classified-failures.json')
source = value['source']
old_source = prior['source']
before = json.loads(gzip.decompress((run / 'source-before.json.gz').read_bytes()))
assert before == json.loads(gzip.decompress((run / 'source-after.json.gz').read_bytes()))
spec = importlib.util.spec_from_file_location('comparison', root / 'docs/perf/2026.09/receiving-postcommit/tools/compare_integrated_sweep.py')
compare_module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(compare_module)
reducer = compare_module.load(compare_module.REDUCER, 'retained_reducer')
causes = compare_module.load(compare_module.CAUSES, 'retained_causes')
comparison = compare_module.compare(raw, prior_raw, reducer, causes)
comparison.update(source=source, reference_source=old_source,
    commands_equal=read(run / 'command.json') == read(prior_run / 'command.json'))
identity = lambda row: tuple(row[key] for key in ['package', 'cargo_target', 'name'])
old_index = {identity(row): row for row in prior['failures']}
new_index = {identity(row): row for row in value['failures']}
exact = {tuple(row) for row in comparison['exact_identity_and_cause_matches']}
records = read(run / 'classification-001/reviewed-source-excerpts.json')
source_cache = {}

def source_bytes(revision, path):
    key = (revision, path)
    if key not in source_cache:
        source_cache[key] = subprocess.check_output(['git', 'show', revision + ':' + path], cwd=root)
    return source_cache[key]

def excerpt(path, line, radius=10):
    data = source_bytes(source, path)
    assert hashlib.sha256(data).hexdigest() == before[path]['sha256']
    text = data.decode().splitlines()
    assert 1 <= line <= len(text)
    start, end = max(0, line - radius - 1), min(len(text), line + radius)
    key = source[:8] + ':' + path + ':' + str(line)
    records[key] = {'path': path, 'source': source, 'sha256': hashlib.sha256(data).hexdigest(),
        'previous_source': old_source, 'unchanged_from_previous_source': data == source_bytes(old_source, path),
        'start_line': start + 1, 'end_line': end,
        'lines': [{'line': i + 1, 'text': text[i]} for i in range(start, end)]}
    return key

def current_panic_excerpt(row, radius=10):
    text = '\n'.join(item['text'] for item in row['raw_diagnostics'])
    match = re.search(r'panicked at (.+):(\d+):\d+:', text)
    assert match, identity(row)
    return excerpt(match[1], int(match[2]), radius)

for key, row in new_index.items():
    if key in exact:
        old = old_index[key]
        row['classification'] = old['classification']
        row['prior_classification_record'] = '../../run-001/classification-002/classified-failures.json'
        row['prior_source'] = old_source
        row['run1_comparison'] = 'Exact identity and normalized diagnostic match.'
        if old.get('reviewed_explanation'):
            row['reviewed_explanation'] = old['reviewed_explanation']
        if old.get('execution_scope'):
            row['execution_scope'] = old['execution_scope']
        if old.get('source_evidence_keys'):
            row['prior_source_evidence_keys'] = old['source_evidence_keys']
        row['required_inputs'] = old['required_inputs']
    elif key not in old_index:
        assert 'Operation not permitted' in row['cause'], key
        row.update(classification='socket_permission_refusal', execution_scope='local_socket_setup_refused',
            reviewed_explanation='The recorded local socket operation returns EPERM. The intended HTTP or probe exchange cannot run. This capture used the restricted tool sandbox.',
            source_evidence_keys=[current_panic_excerpt(row)])
        if key[2] == 'a_gate_that_dies_before_listening_is_reported_against_its_port':
            path = 'services/ctl/src/dev/environment.rs'
            text = source_bytes(source, path).decode().splitlines()
            found = [i + 1 for i, line in enumerate(text) if 'the management Gate cannot take' in line]
            assert len(found) == 1
            row['source_evidence_keys'].append(excerpt(path, found[0], 12))
            row['reviewed_explanation'] = 'The command fails its port reservation with EPERM. The test then rejects that earlier refusal because it expects the started Gate to die before listening.'
    else:
        assert key == ('wamn-proof-conformance', '--test chart_seam_governance', 'receiving_pat_overlay_renders_a_complete_scoped_host'), key
        assert 'socket: operation not permitted' in row['cause'] and 'Helm render failed' in row['cause']
        row.update(classification='chart_fetch_socket_permission_refusal', execution_scope='chart_fetch_refused_before_render',
            reviewed_explanation='Helm cannot open the DNS socket to fetch the declared chart. Rendering and Kubernetes decoding do not run. The prior capture reached Kubernetes decoding and failed there.',
            source_evidence_keys=[current_panic_excerpt(row, 17)])
    assert row['classification'] != 'requires_current_cause_review'

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

fixed = []
for key in comparison['absent_reference_failures']:
    result = case_result(key[2])
    assert result['status'] == 'ok'
    fixed.append({'identity': key, 'previous_cause': old_index[tuple(key)]['cause'], 'current_result': result,
                  'disposition': 'The same named case executed and passed in this run.'})

added = []
for item in comparison['added_failures']:
    row = new_index[tuple(item['identity'])]
    previous_result = case_result(row['name'], old=True)
    assert previous_result['status'] == 'ok'
    added.append({'identity': item['identity'], 'classification': row['classification'], 'cause': row['cause'],
        'previous_result': previous_result, 'current_log_line': row['diagnostic_start_log_line'],
        'source_evidence_keys': row['source_evidence_keys']})

moves = read(run / 'app-test-moves-prepared.json')
assert moves['source'] == source and moves['previous_source'] == old_source
for entry in moves['source_records']:
    assert hashlib.sha256(source_bytes(entry['source'], entry['path'])).hexdigest() == entry['sha256']
for move in moves['moved_tests']:
    name = move['current_identity'][2]
    move['previous_result'] = case_result(name, old=True, description=move['previous_target_description'])
    move['current_result'] = case_result(name, description=move['current_target_description'])
    assert move['previous_result']['status'] == move['current_result']['status'] == 'ok'
moves['interpretation'] = 'Every listed name remains and reports a passing result under its current app test owner.'

prior_mappings = read(prior_run / 'classification-002/moved-failure-identities.json')
baseline_moves = []
for old in prior_mappings:
    row = new_index[tuple(old['current_identity'])]
    baseline_moves.append({**old, 'current_cause': row['cause'], 'current_log_line': row['diagnostic_start_log_line'],
        'prior_mapping_record': '../../run-001/classification-002/moved-failure-identities.json',
        'disposition': 'The moved case remains a failure because its live input is absent.'})
resolutions = []
for item in value['pending_review']:
    if item['kind'] == 'current_failure_cause':
        row = new_index[tuple(item['identity'])]
        resolutions.append({**item, 'classification': row['classification'],
                            'disposition': row.get('reviewed_explanation') or row.get('run1_comparison')})
    else:
        assert item['kind'] == 'absent_failure', item
        match = next(row for row in baseline_moves if row['previous_identity'] == item['identity'])
        resolutions.append({**item, 'current_identity': match['current_identity'], 'disposition': match['disposition']})
for reference, entries in value['removed_failure_dispositions'].items():
    for entry in entries:
        prior_entry = next(row for row in prior['removed_failure_dispositions'][reference] if row['identity'] == entry['identity'])
        entry.update(copy.deepcopy(prior_entry))
        if entry.get('current_identity'):
            assert tuple(entry['current_identity']) in new_index
            entry['source_change'] = 'baseline-moved-failures.json'

skip_key = lambda row: (row['target_description'], re.sub(r'-[0-9a-f]+$', '', Path(row['target_executable']).name), row['name'])
old_skips = Counter(skip_key(row) for row in prior['explicit_self_skips']['entries'])
new_skips = Counter(skip_key(row) for row in value['explicit_self_skips']['entries'])
comparison['executable_qualified_skips_equal'] = old_skips == new_skips
comparison['added_executable_qualified_skips'] = list((new_skips - old_skips).elements())
comparison['absent_executable_qualified_skips'] = list((old_skips - new_skips).elements())
value.update(classification_counts=dict(Counter(row['classification'] for row in value['failures'])),
    pending_review=[], classification_complete=True, source_review_resolutions=resolutions,
    comparison_to_previous_run='run1-comparison.json', moved_failure_identities='baseline-moved-failures.json')
for old, new in zip(original['failures'], value['failures']):
    assert identity(old) == identity(new) and old['cause'] == new['cause'] and old['raw_diagnostics'] == new['raw_diagnostics']
assert original['explicit_self_skips'] == value['explicit_self_skips']
assert not raw['unresolved'] and not comparison['duplicate_failure_identities']
out = run / 'classification-002'
assert not out.exists()
out.mkdir()
write = lambda name, data: (out / name).write_text(json.dumps(data, indent=2) + '\n')
write('classified-failures.json', value)
write('reviewed-source-excerpts.json', records)
write('run1-comparison.json', comparison)
write('changed-failures.json', {'added_failures': added, 'same_identity_changed_causes': comparison['changed_causes'], 'previous_failures_now_passed': fixed})
write('app-test-moves.json', moves)
write('baseline-moved-failures.json', baseline_moves)
write('explicit-self-skips.json', value['explicit_self_skips'])
write('pending-review.json', {'result_differences': [], 'parser_entries': []})
counts = raw['counts']
run_record = read(run / 'run.json')
summary = {'source': source, 'exit_code': raw['exit_code'], 'elapsed_seconds': run_record['elapsed_seconds'],
    'counts': counts, 'classification_counts': value['classification_counts'], 'classification_complete': True,
    'pending_review_entries': 0, 'unresolved_parser_entries': [], 'source_stable': True, 'tracked_files': len(before),
    'log_sha256': run_record['workspace_log_sha256'], 'previous_exact_failure_matches': len(exact),
    'added_failures': len(added), 'changed_failure_causes': len(comparison['changed_causes']),
    'previous_failures_now_passed': len(fixed), 'moved_app_cases_with_passing_results': len(moves['moved_tests']),
    'baseline_moved_failures': len(baseline_moves)}
cell = lambda text: str(text).replace('|', '\\|').replace('\n', '<br>')
report = f'''# Retained workspace test result

Source `{source}` returned {raw['exit_code']} after {run_record['elapsed_seconds']} seconds. [Full output](../workspace.log).
The capture recorded {len(before):,} tracked files outside Beads with unchanged bytes and modes. HEAD also remained unchanged.

The parser lists {counts['test_failed']} named failures across {counts['test_failed_targets']} targets, {counts['test_reported_passed']:,} reported passes, and {counts['doctest_reported_passed']} passing doctests.
The reported passes include {value['explicit_self_skips']['count']} explicit skips. Those skipped operations did not run.
The skip count remains a lower bound because silent early returns can exist. Two schema-generation cases remain filtered.

This capture ran under the restricted tool sandbox. The integration coordinator confirmed that launch context after completion.
All {len(added)} added failures report denied local socket operations. Each [source excerpt](reviewed-source-excerpts.json) identifies the failed operation or its caller.
The chart case also reports a denied DNS socket while Helm fetches the declared chart. It does not reach the earlier Kubernetes discovery failure.
These remain failed tests. This run does not establish their intended HTTP, TLS, or probe behavior.

Against [run 1](run1-comparison.json), {len(exact)} failures retain the same identity and normalized cause.
The three corrected cases now report passing results. [Every added or changed failure and each corrected case](changed-failures.json) remain named.
All {len(moves['moved_tests'])} [moved app cases](app-test-moves.json) report passing results under their new owners.
The same explicit skip identities, target executables, and multiplicities remain.

The [wave baseline comparison](../reduction-001/baseline-comparison.json) preserves all original identities and causes.
The {len(baseline_moves)} [earlier moved failures](baseline-moved-failures.json) still refuse absent live inputs.
Two earlier failure cases and one earlier explicit skip were removed by recorded source changes. They are not passes.
[Recorded removals](../classification-001/known-test-removals.json), [step 1](../reduction-001/step1-comparison.json), and [step 2](../reduction-001/step2-comparison.json) remain available.

Every failure is classified, with no unresolved parser entry or cause review. This result supplies no application or wave completion claim.

| Classification | Failed tests |
| --- | ---: |
'''
report += '\n'.join(f'| {cell(k.replace("_", " "))} | {v} |' for k, v in value['classification_counts'].items())
report += '\n\n| Package / target | Failed case | Classification | Actual cause | Evidence |\n| --- | --- | --- | --- | --- |\n'
for row in value['failures']:
    report += '| ' + ' | '.join([cell(row['package'] + ' / ' + row['cargo_target']), cell(row['name']),
        cell(row['classification'].replace('_', ' ')), cell(row['cause']), f'[log {row["diagnostic_start_log_line"]}](../workspace.log#L{row["diagnostic_start_log_line"]})']) + ' |\n'
report += '\nEvery explicit skip follows. [Complete skip records](explicit-self-skips.json).\n\n| Target executable / description | Case | Explicit skip message | Evidence |\n| --- | --- | --- | --- |\n'
for row in value['explicit_self_skips']['entries']:
    executable = re.sub(r'-[0-9a-f]+$', '', Path(row['target_executable']).name)
    report += '| ' + ' | '.join([cell(executable + ' / ' + row['target_description']), cell(row['name']), cell(row['message']),
        f'[log {row["diagnostic_log_line"]}](../workspace.log#L{row["diagnostic_log_line"]})']) + ' |\n'
(out / 'report.md').write_text(report)
summary['report_sha256'] = hashlib.sha256((out / 'report.md').read_bytes()).hexdigest()
write('classification-summary.json', summary)
print(json.dumps(summary, indent=2))
