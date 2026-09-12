#!/usr/bin/env python3
"""Classify only the retained B sweep deltas; do not change its raw comparison."""
import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
MONTH = HERE.parents[1]
CURRENT = MONTH / 'native-b-adoption/integrated-workspace-001'
BASELINE = MONTH / 'ctc8-33-nested-authority/integrated-workspace-001'
BASELINE_ROOT = '/home/kaalin/.cache/wamn-lanes/ctc8-33-nested-authority-20260910/'
CURRENT_ROOT = '/home/kaalin/dev/wamn/'
AUTH_NAME = 'router_driver::native_policy::tests::authenticated::native_authenticated_nested_authority_and_lifecycle'
AUTH_INPUT = 'WAMN_NATIVE_B_AUTH_PG_URL'
WIT_NAME = 'version_identity::wamn_wit_packages_stay_at_mvp_version'
INVENTORY_NAMES = {'direct_wasmtime_consumers_inherit_workspace_source_contract',
                   'resolved_wasmtime_type_universe_is_single_and_canonical'}


def read(path):
    return json.loads(path.read_text())


def identity(row):
    return [row['package'], row['cargo_target'], row['name']]


def classify():
    comparison = read(CURRENT / 'workspace-comparison.json')
    before = read(BASELINE / 'workspace-results.json')
    after = read(CURRENT / 'workspace-results.json')
    delta = comparison['comparisons']['nested_authority']
    stable = read(CURRENT / 'source-stability.json')
    environment = read(CURRENT / 'environment-names.json')
    raw = (CURRENT / 'workspace.log').read_text().splitlines()
    old_raw = (BASELINE / 'workspace.log').read_text().splitlines()
    assert comparison['source'] == '6f02d70d6b0a03b90160652df2d9c16c065010e3'
    assert stable['stable_with_declared_exclusions'] is True
    assert (delta['reference_failures'], delta['current_failures']) == (83, 86)
    assert len(delta['exact_identity_and_cause_matches']) == 82
    assert len(delta['changed_causes']) == 1 and len(delta['added_failures']) == 3
    for key in ('absent_reference_failures', 'duplicate_failure_identities', 'reference_unresolved',
                'current_unresolved', 'added_explicit_self_skips', 'absent_explicit_self_skips',
                'added_targets', 'absent_reference_targets'):
        assert not delta[key], key
    skip_keys = lambda data: Counter((row['target_description'], row['name'])
                                    for row in data['explicit_self_skips']['entries'])
    assert skip_keys(before) == skip_keys(after)
    assert sum(skip_keys(after).values()) == 85

    wit = delta['changed_causes'][0]
    assert wit['identity'] == ['wamn-proof-conformance', '--lib', WIT_NAME]
    old_cause, new_cause = wit['before'], wit['after']
    assert sum(line.startswith(BASELINE_ROOT) for line in old_cause['causal_lines']) == 8
    assert sum(line.startswith(CURRENT_ROOT) for line in new_cause['causal_lines']) == 8
    normalized = dict(old_cause, causal_lines=[CURRENT_ROOT + line[len(BASELINE_ROOT):]
                       if line.startswith(BASELINE_ROOT) else line for line in old_cause['causal_lines']])
    assert normalized == new_cause, 'WIT cause changed beyond the exact checkout prefix'

    added = {row['identity'][2]: row for row in delta['added_failures']}
    assert set(added) == INVENTORY_NAMES | {AUTH_NAME}
    auth = added[AUTH_NAME]
    assert auth['identity'] == ['wamn-execution-host', '--lib', AUTH_NAME]
    assert auth['cause'] == {'failure_signatures': [], 'referenced_arming_inputs': [], 'causal_lines': []}
    diagnostic = f"real authenticated native proof: set {AUTH_INPUT} to this proof's fresh disposable PostgreSQL 18 server"
    matches = [index for index, line in enumerate(raw) if line == diagnostic]
    assert len(matches) == 1
    auth_line = matches[0]
    assert f"thread '{AUTH_NAME}'" in raw[auth_line - 1]
    assert '    environment variable not found' in raw[auth_line:auth_line + 5]
    assert AUTH_INPUT not in environment['child_environment_names']
    assert AUTH_INPUT not in environment['explicitly_armed_names']
    introduced = []
    for name in sorted(INVENTORY_NAMES):
        failure = next(row for row in after['failures'] if row['name'] == name)
        assert identity(failure) == ['wamn-proof-conformance', '--test wasmtime_source_identity', name]
        passes = [index + 1 for index, line in enumerate(old_raw) if line == f'test {name} ... ok']
        assert len(passes) == 1
        introduced.append({'identity': identity(failure), 'classification': 'introduced_stale_inventory_failure',
            'status': 'requires_source_fix', 'baseline_pass_log_line': passes[0],
            'current_diagnostic_start_log_line': failure['diagnostic_start_log_line'],
            'current_diagnostic_end_log_line': failure['diagnostic_end_log_line'],
            'cause': added[name]['cause'],
            'interpretation': 'B removed the execution-host direct wasmtime-wasi dependency; the retained direct-consumer inventory still requires it. This failed sweep does not prove a fix.'})
    paths = [CURRENT / name for name in ('workspace-results.json', 'workspace-comparison.json',
             'workspace.log', 'source.txt', 'source-stability.json', 'environment-names.json', 'run.json')]
    paths += [BASELINE / name for name in ('workspace-results.json', 'workspace.log', 'source.txt')]
    paths += [HERE / 'comparator-command.json', Path(__file__)]
    return {'schema': 'wamn-native-b-workspace-classification/v1',
        'baseline': {'source': (BASELINE / 'source.txt').read_text().strip(), 'counts': before['counts'],
                     'combined_reported_counts': before['combined_reported_counts']},
        'current': {'source': comparison['source'], 'counts': after['counts'],
                    'combined_reported_counts': after['combined_reported_counts'],
                    'cargo_exit_code': comparison['exit_code'], 'run': read(CURRENT / 'run.json')},
        'source_stability': stable,
        'exact_identity_and_cause_matches': delta['exact_identity_and_cause_matches'],
        'wit_diagnostic': {'identity': wit['identity'], 'classification': 'existing_failure_checkout_prefix_only',
            'changed_message_count': 8, 'baseline_prefix': BASELINE_ROOT, 'current_prefix': CURRENT_ROOT,
            'all_other_cause_fields_equal': True, 'before': old_cause, 'after': new_cause},
        'unarmed_native_b_auth': {'identity': auth['identity'], 'classification': 'missing_live_fixture',
            'retained_comparator_cause': auth['cause'], 'required_input': AUTH_INPUT,
            'input_absent_from_child_environment': True,
            'raw_diagnostic': [{'log_line': index + 1, 'text': raw[index]}
                               for index in range(auth_line - 1, auth_line + 4)]},
        'introduced_inventory_failures': introduced,
        'explicit_self_skips': {'baseline': 85, 'current': 85, 'identities_and_multiplicities_unchanged': True},
        'targets': {'identities_and_multiplicities_unchanged': True, 'test_targets': 194, 'doctest_targets': 37},
        'comparator_exit_code': 2, 'proof_verdict': 'not supplied',
        'limitations': ['The two introduced inventory failures require source fixes; no later focused result is asserted here.',
                       'Existing failures remain failures. Self-skips and reported passes do not establish executed live proofs.',
                       'The retained comparator and sweep logs are unchanged; the empty B-auth cause is resolved here from exact raw lines.'],
        'inputs': [{'path': str(path), 'bytes': path.stat().st_size,
                    'sha256': hashlib.sha256(path.read_bytes()).hexdigest()} for path in paths]}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    result = classify()
    with args.output.open('x') as output:
        output.write(json.dumps(result, indent=2) + '\n')
