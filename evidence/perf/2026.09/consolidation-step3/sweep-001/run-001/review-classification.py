#!/usr/bin/env python3
"""Record the source review of this completed test run."""
from collections import Counter
import copy
import gzip
import hashlib
import json
from pathlib import Path
import re
import subprocess

run = Path(__file__).resolve().parent
root = run.parents[5]
read = lambda path: json.loads(path.read_text())
value = read(run / 'classification-001/classified-failures.json')
original = copy.deepcopy(value)
source = value['source']
before = json.loads(gzip.decompress((run / 'source-before.json.gz').read_bytes()))
assert before == json.loads(gzip.decompress((run / 'source-after.json.gz').read_bytes()))
prior = read(run.parents[2] / 'consolidation-step2/run-003/classified-failures.json')
comparison = read(run / 'reduction-001/step2-comparison.json')
old_source = comparison['reference_source']
records = read(run / 'classification-001/reviewed-source-excerpts.json')
out = run / 'classification-002'
assert not out.exists()


def excerpt(path, needle, revision=source, radius=9):
    data = subprocess.check_output(['git', 'show', revision + ':' + path], cwd=root)
    digest = hashlib.sha256(data).hexdigest()
    if revision == source:
        assert digest == before[path]['sha256'], path
    lines = data.decode().splitlines()
    matches = [index for index, line in enumerate(lines) if needle in line]
    assert len(matches) == 1, (path, needle, matches)
    start, end = max(0, matches[0] - radius), min(len(lines), matches[0] + radius + 1)
    key = revision[:8] + ':' + path + ':' + str(matches[0] + 1)
    records[key] = {'path': path, 'source': revision, 'sha256': digest,
        'start_line': start + 1, 'end_line': end,
        'lines': [{'line': index + 1, 'text': lines[index]} for index in range(start, end)]}
    return key


def source_path(name, old=False):
    if name.startswith('receiving_data_access::'):
        return 'apps/wamn_receiving/tests/receiving_data_access.rs'
    if name.startswith('wms_runtime_live::'):
        return 'apps/wamn_wms/tests/wms_runtime_live.rs'
    if 'fresh_only::execution_tests::' in name:
        return 'tests/integration/src/route_authentication_live/fresh_only.rs'
    if 'overlay_compatibility::' in name:
        return ('tests/integration/src/route_authentication_live/overlay_compatibility.rs' if old
                else 'apps/client_acme_receiving/tests/overlay_compatibility.rs')
    if 'postcommit::' in name:
        return 'tests/integration/src/route_authentication_live/postcommit.rs' if old else 'apps/wamn_receiving/tests/postcommit.rs'
    if name.endswith('production_receiving_command_histories'):
        return 'apps/wamn_receiving/tests/receiving_command_histories_live.rs'
    if 'startup_burst::' in name:
        return 'tests/integration/tests/startup_burst_live.rs'
    if old:
        return 'tests/integration/src/route_authentication_live.rs'
    return 'apps/wamn_receiving/tests/route_authentication_live/' + name.split('::')[1] + '.rs'


indexed = {tuple(row[key] for key in ['package', 'cargo_target', 'name']): row for row in value['failures']}
journey_keys = [excerpt('test-support/harness/src/journey.rs', 'const JOURNEY_DOCUMENT_ENV:'),
                excerpt('test-support/harness/src/journey.rs', 'pub fn required()')]
module_key = excerpt('apps/wamn_receiving/tests/route_authentication_live.rs', 'mod command_histories;', radius=15)
resolutions = []
for identity, failure in indexed.items():
    if failure['classification'] != 'requires_current_cause_review':
        continue
    package, target, name = identity
    keys = []
    if package in ['wamn-receiving-tests', 'wamn-wms-tests']:
        path = source_path(name)
        keys.append(excerpt(path, 'fn ' + name.split('::')[-1] + '('))
        if 'environment variable not found' in failure['cause'] and 'overlay_compatibility::' in name:
            required = ['WAMN_OVERLAY_OBSERVER_DATABASE_URL']
            keys.append(excerpt(path, 'let url = std::env::var("WAMN_OVERLAY_OBSERVER_DATABASE_URL")'))
            explanation = 'The first database input is absent. The case returns before connecting.'
        else:
            required = failure['required_inputs']
            assert required, identity
            explanation = 'The recorded required input is absent. This failure establishes no live application outcome.'
        if required == ['WAMN_JOURNEY_DOCUMENT']:
            keys.extend(journey_keys)
        if 'product_dev_command_' in name:
            keys.append(excerpt(path, 'const JOURNEY_URL_ENV:'))
        if 'production_receiving_command_histories' in name or 'startup_burst::' in name:
            keys.append(module_key)
        failure.update(classification='missing_live_or_artifact_input', required_inputs=required,
            execution_scope='setup_refused_before_live_work', reviewed_explanation=explanation)
    elif name == 'dev_up_refuses_an_ephemeral_gate_port_and_names_it':
        keys = [excerpt('services/ctl/tests/dev_command.rs', 'fn ' + name + '()', radius=55),
                excerpt('services/ctl/src/dev/up.rs', 'event_provisioning_username: String', radius=23)]
        failure.update(classification='stale_test_inputs_or_expectations', reviewed_explanation=
            'The test command omits seven required event configuration arguments. Clap refuses before the expected ephemeral-port validation runs.')
    elif name == 'resolved_wasmtime_type_universe_is_single_and_canonical':
        keys = [excerpt('tests/conformance/tests/wasmtime_source_identity.rs', 'const DIRECT_CONSUMERS:', radius=30),
                excerpt('apps/wamn_receiving/tests/Cargo.toml', 'wasmtime-wasi-http =', radius=12)]
        failure.update(classification='stale_test_inputs_or_expectations', reviewed_explanation=
            'The actual dependency map includes the extracted Receiving test crate. The expected map still lists only the three earlier consumers.')
    elif name == 'only_identity_receives_issuer_credentials_and_other_consumers_keep_their_classes':
        keys = [excerpt('tests/system/tests/deploy_platform_inventory.rs', 'if project.is_none()', radius=17),
                excerpt('deploy/platform/values-host-default.yaml', '- name: WAMN_EVT_STREAM_REPLICAS', radius=16)]
        failure.update(classification='stale_test_inputs_or_expectations', reviewed_explanation=
            'The default host declares two stream configuration Secret keys. The expected map omits both WAMN_EVT_STREAM_REPLICAS and WAMN_EVT_DUP_WINDOW_SECS.')
    elif name == 'version_identity::wamn_wit_packages_stay_at_mvp_version':
        keys = [excerpt('tests/conformance/src/version_identity.rs', 'fn tracked_wit_files(', radius=24),
                excerpt('tests/conformance/src/version_identity.rs', 'fn wamn_wit_package_violations(', radius=34)]
        for path in ['docs/perf/2026.09/native-c-advisories/materializer-async-001/original-world.wit',
                     'docs/perf/2026.09/native-c-advisories/materializer-async-001/repaired-world.wit']:
            keys.append(excerpt(path, 'package wamn:postgres@0.1.0 {', radius=2))
        failure.update(classification='source_scan_misparses_retained_wit_evidence', reviewed_explanation=
            'The tracked-file scan includes saved WIT displays and treats the opening brace as part of the version. The same failure now names eight more headers in two native C diagnostic captures. These are not changed production WIT versions.')
    else:
        raise AssertionError(identity)
    failure['source_evidence_keys'] = keys
    resolutions.append({'kind': 'current_failure_cause', 'identity': list(identity),
        'classification': failure['classification'], 'explanation': failure['reviewed_explanation'],
        'source_evidence_keys': keys})

mappings = []
for old_identity in comparison['absent_reference_failures']:
    candidates = [(identity, row) for identity, row in indexed.items()
                  if identity[1] == old_identity[1] and identity[2].split('::')[-1] == old_identity[2].split('::')[-1]]
    assert len(candidates) == 1, old_identity
    identity, row = candidates[0]
    old_row = next(row for row in prior['failures'] if [row[key] for key in ['package', 'cargo_target', 'name']] == old_identity)
    keys = [excerpt(source_path(old_identity[2], True), 'fn ' + old_identity[2].split('::')[-1] + '(', old_source),
            excerpt(source_path(identity[2]), 'fn ' + identity[2].split('::')[-1] + '(')]
    mappings.append({'previous_identity': old_identity, 'current_identity': list(identity),
        'previous_cause': old_row['cause'], 'current_cause': row['cause'],
        'previous_source': old_source, 'source_evidence_keys': keys,
        'disposition': 'The case remains a failure under its app test owner. Its required live input remains absent.'})
assert len(mappings) == 17
for item in value['pending_review']:
    if item['kind'] == 'absent_failure':
        match = next(row for row in mappings if row['previous_identity'] == item['identity'])
        resolutions.append({**item, 'current_identity': match['current_identity'],
                            'disposition': match['disposition']})
    else:
        assert item['kind'] == 'current_failure_cause' and tuple(item['identity']) in indexed
        assert indexed[tuple(item['identity'])]['classification'] != 'requires_current_cause_review'
assert len(resolutions) == len(value['pending_review']) == 74

added = []
for item in comparison['added_failures']:
    row = indexed[tuple(item['identity'])]
    moved = next((entry for entry in mappings if entry['current_identity'] == item['identity']), None)
    added.append({'identity': item['identity'], 'classification': row['classification'],
        'cause': row['cause'], 'log_line': row['diagnostic_start_log_line'],
        'previous_identity': moved['previous_identity'] if moved else None,
        'disposition': moved['disposition'] if moved else row.get('reviewed_explanation', 'See the recorded setup refusal.')})
assert len(added) == 39 and sum(row['previous_identity'] is None for row in added) == 22
value.update(classification_counts=dict(Counter(row['classification'] for row in value['failures'])),
             pending_review=[], classification_complete=True, source_review_resolutions=resolutions,
             moved_failure_identities='moved-failure-identities.json')
for rows in value['removed_failure_dispositions'].values():
    for row in rows:
        match = next((entry for entry in mappings if entry['previous_identity'] == row['identity']), None)
        if match:
            row.update(disposition=match['disposition'], current_identity=match['current_identity'],
                       source_change='moved-failure-identities.json')
for row in value['failures']:
    if row.get('step2_comparison') == 'review required':
        row['initial_step2_comparison'] = row['step2_comparison']
        row['step2_comparison'] = 'Resolved by the recorded source review. This case remains a failure.'
assert len(value['failures']) == 104
for old, current in zip(original['failures'], value['failures']):
    assert old['raw_diagnostics'] == current['raw_diagnostics']
    assert old['cause'] == current['cause']
assert original['explicit_self_skips'] == value['explicit_self_skips']
out.mkdir()


def write(name, data):
    (out / name).write_text(json.dumps(data, indent=2) + '\n')


write('classified-failures.json', value)
write('reviewed-source-excerpts.json', records)
write('moved-failure-identities.json', mappings)
write('changed-failures.json', {'added_exact_identities': added,
    'same_identity_changed_cause': comparison['changed_causes'],
    'additional_library_occurrences': [
        {'identity': ['wamn-receiving-tests', '--lib', 'route_authentication_live::command_histories::production_receiving_command_histories'],
         'existing_target': 'wamn-receiving-tests --test receiving_command_histories_live', 'source_evidence': module_key},
        {'identity': ['wamn-receiving-tests', '--lib', 'route_authentication_live::startup_burst::production_http_start_burst_keeps_native_host_progress'],
         'existing_target': 'wamn-proof-integration --test startup_burst_live', 'source_evidence': module_key}]})
write('explicit-self-skips.json', value['explicit_self_skips'])
write('pending-review.json', {'result_differences': [], 'parser_entries': []})
summary = read(run / 'classification-001/classification-summary.json')
summary.update(classification_counts=value['classification_counts'], pending_review_entries=0,
    classification_complete=True, moved_failure_count=17, new_failure_identities_after_moves=22,
    new_ordinary_test_failures=3, new_setup_refusals=19, same_identity_changed_causes=1)
report = (run / 'classification-001/report.md').read_text()
start = report.index('There are 74 result differences')
end = report.index('| Classification |', start)
report = report[:start] + (
    'All 104 failures are classified. No parser entries or cause reviews remain unresolved. '
    'This does not make the failed test run pass. [Source excerpts](reviewed-source-excerpts.json) retain the inspected bytes and their hashes.\n\n'
    'Against step 2, 17 absent identities map to app-owned failures with the same missing inputs. '
    'The 22 other added identities comprise three ordinary test failures, 12 Receiving setup refusals, '
    'five WMS source-cleanliness refusals, and two additional library occurrences of existing live wrappers. '
    '[Every changed identity and cause](changed-failures.json) and [every moved identity](moved-failure-identities.json) remain listed.\n\n'
    'The WIT scan still fails on saved component displays. Its changed diagnostic names two more saved displays. '
    'The earlier deployment prerequisite failure and missing Kubernetes discovery also remain. '
    'This run does not establish native C or wave completion.\n\n') + report[end:]
start = report.index('| Classification |')
end = report.index('Every explicit skip follows.')
cell = lambda text: str(text).replace('|', '\\|').replace('\n', '<br>')
table = '| Classification | Failed tests |\n| --- | ---: |\n'
table += '\n'.join(f'| {cell(key.replace("_", " "))} | {count} |' for key, count in value['classification_counts'].items())
table += '\n\n| Package / target | Failed case | Classification | Actual cause | Evidence |\n| --- | --- | --- | --- | --- |\n'
for failure in value['failures']:
    number = failure['diagnostic_start_log_line']
    table += '| ' + ' | '.join([cell(failure['package'] + ' / ' + failure['cargo_target']),
        cell(failure['name']), cell(failure['classification'].replace('_', ' ')),
        cell(failure['cause']), f'[log {number}](../workspace.log#L{number})']) + ' |\n'
report = report[:start] + table + '\n' + report[end:]
for name in ['baseline-comparison.json', 'step1-comparison.json', 'step2-comparison.json']:
    report = report.replace('](' + name + ')', '](../reduction-001/' + name + ')')
(out / 'report.md').write_text(report)
summary['report_sha256'] = hashlib.sha256((out / 'report.md').read_bytes()).hexdigest()
write('classification-summary.json', summary)
print(json.dumps(summary, indent=2))
