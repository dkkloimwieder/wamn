#!/usr/bin/env python3
"""Record reviewed Stage 2 causes without changing the retained reduction."""
from collections import Counter
import gzip
import hashlib
import json
from pathlib import Path
import re
import shutil
import subprocess

root = Path('/home/kaalin/dev/wamn')
out = root / 'docs/perf/2026.09/consolidation-step2/run-001'
reduced = out / 'reduction-001'
read = lambda path: json.loads(path.read_text())
run = read(out / 'run.json')
source = run['source_head']
raw = read(reduced / 'workspace-results.json')
value = read(reduced / 'classified-failures-draft.json')
lines = (out / 'workspace.log').read_text().splitlines()
before = json.loads(gzip.decompress((out / 'source-before.json.gz').read_bytes()))
after = json.loads(gzip.decompress((out / 'source-after.json.gz').read_bytes()))
assert before == after
assert hashlib.sha256((out / 'workspace.log').read_bytes()).hexdigest() == run['workspace_log_sha256']
assert raw['unresolved'] == []


def write(name, data):
    with (out / name).open('x') as stream:
        json.dump(data, stream, indent=2)
        stream.write('\n')


def source_excerpt(path, start, end):
    data = subprocess.check_output(['git', 'show', source + ':' + path], cwd=root)
    assert hashlib.sha256(data).hexdigest() == before[path]['sha256']
    text = data.decode().splitlines()
    return {'path': path, 'source': source, 'sha256': hashlib.sha256(data).hexdigest(),
            'start_line': start, 'end_line': end,
            'lines': [{'line': number, 'text': text[number - 1]} for number in range(start, end + 1)]}

sources = {
    'dev_config': source_excerpt('services/ctl/src/dev/config.rs', 1758, 1762),
    'client_route': source_excerpt('crates/schema/generator/src/client_route.rs', 287, 295),
    'cranelift': source_excerpt('tests/conformance/tests/cranelift_dev.rs', 337, 345),
    'state_principals': source_excerpt('tests/conformance/tests/state_ownership.rs', 358, 371),
    'state_roots': source_excerpt('tests/conformance/tests/state_ownership.rs', 792, 804),
    'state_claim_roots': source_excerpt('tests/conformance/tests/state_ownership.rs', 2304, 2308),
    'flow_wit': source_excerpt('crates/platform/runtime/tests/flow_http_routing_wit_coherence.rs', 78, 89),
    'deployment_credentials': source_excerpt('tests/system/tests/deploy_platform_inventory.rs', 796, 835),
    'deployment_prerequisites': source_excerpt('tests/system/tests/deploy_platform_inventory.rs', 219, 242),
    'deployment_secret_comparison': source_excerpt('tests/system/tests/deploy_platform_inventory.rs', 1095, 1109),
    'event_env': source_excerpt('deploy/platform/values-host-default.yaml', 166, 206),
}
for failure in value['failures']:
    name, package, target = failure['name'], failure['package'], failure['cargo_target']
    if failure['classification'] == 'requires_current_cause_review':
        failure['required_inputs'] = []
        if name.startswith('dev::config::tests::'):
            key = 'dev_config'
            explanation = 'The test helper still reads ../../packages and the old Receiving directory name. The moved app manifests are under apps/<package ID>; these assertions fail before dependency resolution.'
        elif name.startswith('client_route::tests::'):
            key = 'client_route'
            explanation = 'The shipped attachment fixture still reads ../../../packages and old app directory names. The route assertions stop at the missing file.'
        elif target == '--test cranelift_dev':
            key = 'cranelift'
            explanation = 'The retained scan still opens the removed top-level components directory.'
        elif target == '--test state_ownership':
            if name == 'app_role_and_app_user_id_have_no_production_reader_while_the_claim_escape_is_open':
                key = 'state_claim_roots'
                explanation = 'The additional claim-read scan still opens the removed packages directory, so its retained authority assertion does not run.'
            elif name == 'missing_scan_exclusion_path_is_rejected':
                key = 'state_roots'
                explanation = 'The expected scan roots still require components although the ownership file names apps. Validation stops before checking the deliberately missing exclusion.'
            else:
                key = 'state_principals'
                explanation = 'The principal root check still accepts components and rejects apps/platform/events/materializer. Validation stops before the intended ownership assertion.'
        elif target == '--test flow_http_routing_wit_coherence':
            key = 'flow_wit'
            explanation = 'The WIT copy scan still opens the removed components directory.'
        elif name == 'only_identity_receives_issuer_credentials_and_other_consumers_keep_their_classes':
            key = 'deployment_credentials'
            explanation = 'The static host expectation omits the approved native-C event username and declared org/project/environment Secret keys. The output lists those fields as the mismatch, not missing environment inputs.'
            failure['classification'] = 'deployment_credential_expectation_mismatch'
        elif name == 'every_mounted_secret_is_declared_here_or_named_a_prerequisite':
            key = 'deployment_prerequisites'
            explanation = 'The prior undeclared WMS object-store Secret remains. Native C additionally mounts wamn-event-nats and wamn-materializer-nats, which the retained prerequisite list does not name.'
            failure['classification'] = 'deployment_prerequisite_declaration_mismatch'
        else:
            raise AssertionError((package, target, name))
        if failure['classification'] == 'requires_current_cause_review':
            failure['classification'] = 'stale_app_path_or_scan_roots'
        failure['reviewed_explanation'] = explanation
        failure['source_evidence_key'] = key
        failure['source_evidence'] = f"{sources[key]['path']}:{sources[key]['start_line']}"
        failure['step1_comparison'] = ('changed retained diagnostic' if 'step1_classification' in failure
                                       else 'new failing identity from a previously passing case')
    # Supplement the known retained reducer blind spot without changing its raw file.
    if name == 'router_driver::native_policy::tests::authenticated::native_authenticated_nested_authority_and_lifecycle':
        starts = [i for i, line in enumerate(lines) if line.startswith('test ' + name + ' ...')]
        assert len(starts) == 2
        finish = next(i for i in range(starts[-1] + 1, len(lines)) if re.match(r'^test .+? \.\.\. ', lines[i]))
        block = [{'log_line': i + 1, 'text': lines[i]} for i in range(starts[0], finish)]
        assert any('set WAMN_NATIVE_B_AUTH_PG_URL' in row['text'] for row in block)
        failure['retained_reducer_raw_diagnostics'] = failure['raw_diagnostics']
        failure['raw_diagnostics'] = block
        failure['diagnostic_start_log_line'] = starts[0] + 1
        failure['diagnostic_end_log_line'] = finish
        failure['cause'] = 'WAMN_NATIVE_B_AUTH_PG_URL is absent; the nested authenticated native test exits 101 before live work.'
        failure['required_inputs'] = ['WAMN_NATIVE_B_AUTH_PG_URL']
        failure['reviewed_explanation'] = 'The reducer overwrites the parent diagnostic with its same-named child. Current raw lines show the same missing PostgreSQL input as the earlier runs.'
        failure['source_evidence'] = 'crates/execution/host/src/router_driver/native_policy/tests/authenticated.rs:574'
    if 'source_evidence' not in failure:
        for row in failure['raw_diagnostics']:
            match = re.search(r'panicked at (.+):(\d+):\d+:$', row['text'])
            if match:
                failure['source_evidence'] = match[1] + ':' + match[2]
                break
    assert failure['classification'] != 'requires_current_cause_review'

# Verify skip identity using executable names as well as descriptions, so same
# src/lib.rs descriptions cannot silently cross package owners.
def skip_keys(data):
    targets = {target['running_log_line']: target for target in data['test_targets']}
    return Counter((skip['target_description'],
                    re.sub(r'-[0-9a-f]+$', '', Path(targets[skip['target_running_log_line']]['executable']).name),
                    skip['name']) for skip in data['explicit_self_skips']['entries'])
references = {name: read(root / ('docs/perf/2026.09/consolidation-' + suffix + '/run-001/workspace-results.json'))
              for name, suffix in [('baseline', 'baseline'), ('step1', 'step1')]}
skip_review = {}
for name, reference in references.items():
    old, new = skip_keys(reference), skip_keys(raw)
    skip_review[name] = {'added': list((new - old).elements()), 'removed': list((old - new).elements())}
    assert skip_review[name]['added'] == []
    assert skip_review[name]['removed'] == [('unittests src/lib.rs', 'wamn_runtime',
        'plugins::wamn_jetstream::tests::live_publish_dedupe_bind_fetch_ack')]

value['classification_counts'] = dict(Counter(f['classification'] for f in value['failures']))
assert value['classification_counts'] == {
    'missing_live_or_artifact_input': 79,
    'stale_app_path_or_scan_roots': 23,
    'source_scan_misparses_retained_wit_evidence': 1,
    'unavailable_kubernetes_discovery': 1,
    'deployment_prerequisite_declaration_mismatch': 1,
    'deployment_credential_expectation_mismatch': 1,
}
value.pop('manual_review_required')
value['unresolved_classifications'] = []
value['reviewed_source_excerpts'] = 'reviewed-source-excerpts.json'
value['skip_target_identity_review'] = skip_review
value['interpretation'] = 'The run failed. All 106 failures remain failures; deleted cases are removals. The 84 explicit self-skips did not execute their skipped work. No live inputs were armed. Separate focused runs do not change these results.'
value['comparison_limits'] = [
    'The unchanged reducer uses historical classification labels. Current causes are classified here separately.',
    'The native-B parent/child diagnostic is supplemented from the raw log; original reduced output remains untouched.',
    'Explicit skips are a lower bound. Subtracting them is not an exact count of executed tests.',
    'A static path failure prevents its later authority assertions from running; it is not evidence that those assertions passed.',
]
write('classified-failures.json', value)
write('reviewed-source-excerpts.json', sources)
write('explicit-self-skips.json', value['explicit_self_skips'])
for name in ['workspace-results.json', 'baseline-comparison.json', 'step1-comparison.json', 'classification-inputs.json']:
    assert not (out / name).exists()
    shutil.copyfile(reduced / name, out / name)
write('known-test-removals.json', read(Path(__file__).with_name('known-removals.json')))
write('test-case-delta.json', {
    **read(reduced / 'test-case-delta-draft.json'),
    'summary': {'step1_named_case_occurrences_removed': 49, 'step1_named_case_occurrences_added': 22,
                'net_case_change': -27, 'reported_pass_change': -49, 'failed_case_change': 22},
    'source_change_groups': [
        {'commit': 'a33e5709cd7ee613552aed195e939e5bf6caf82a', 'scope': 'Unused mutable Active selection, notifications, and their cases. Frozen resolution cases remain.'},
        {'commit': 'e02ad16bb5fcc049a304c0132b86f6a50f1442d8', 'scope': 'Custom event delivery/settlement and payload dead-letter assertions removed; native credential, binding and retained publisher cases added or renamed.'},
        {'commits': ['071b1ead', 'd09e9c57', '179968e5', 'c4ef9db4'], 'scope': 'Declared native operator selection, generated launcher retirement, and actual Cargo workspace support add or rename operator/generator cases.'},
    ],
    'interpretation': 'Case-name changes explain count changes, not correctness. Full named occurrence rows remain with their original target and log location.',
})


def cell(text):
    return str(text).replace('|', '\\|').replace('\n', '<br>')


def log_link(start, end=None):
    return f'[log {start}' + (f'–{end}' if end and end != start else '') + f'](workspace.log#L{start})'

header = '''The Stage 2 retained workspace run **failed** on `{source}`: exit **101**, **106 failed tests**, and **42 failed test targets** in **454.938 seconds**. All **26,670 tracked files** outside Beads retained their bytes and modes, and HEAD stayed unchanged during the run. [Capture](run.json), [source stability](source-stability.json), and [unchanged full log](workspace.log).

The same retained command and unarmed environment policy were used for the baseline, Stage 1, and this run. Debug tests used Rust 1.98, two jobs, and the root target. The two schema-regeneration cases stayed explicitly filtered. This result supplies no live-C, deployed-app, authority, or pressure completion claim; separate focused tests keep their own evidence. [Command](command.json), [environment names](environment-names.json), [reducer inputs](classification-inputs.json).

| Measure | Baseline | Stage 1 | Stage 2 |
| --- | ---: | ---: | ---: |
| Test targets | 194 | 191 | 189 |
| Reported test passes, including explicit skips | 2245 | 2214 | 2165 |
| Failed tests | 84 | 84 | 106 |
| Failed test targets | 39 | 39 | 42 |
| Explicit self-skips | 85 | 85 | 84 |
| Ignored tests | 0 | 0 | 0 |
| Explicitly filtered schema cases | 2 | 2 | 2 |
| Passed doctests | 6 | 6 | 6 |
| Failed doctests | 0 | 0 | 0 |

Stage 2 has **24 new failing identities**, **two removed failing cases**, and **one changed retained cause** compared with Stage 1; **81** retained causes match. Against the baseline, **80** retained causes match and **two** changed: the existing app artifact input rename and the expanded mounted-Secret mismatch. No removed failure is counted as a fix. [Baseline comparison](baseline-comparison.json), [Stage 1 comparison](step1-comparison.json).

The 24 new failures are 23 stale app paths or scan roots and one deployment credential expectation. The stale paths stop retained assertions before they can check dependency resolution, route behavior, or authority. The prior mounted-Secret failure now additionally names `wamn-event-nats` and `wamn-materializer-nats`; the older WMS object-store Secret mismatch remains. The exact causes and captured source excerpts are preserved below and in [classified-failures.json](classified-failures.json) and [reviewed-source-excerpts.json](reviewed-source-excerpts.json).

| Current classification | Count |
| --- | ---: |
| Missing live or artifact input | 79 |
| Stale app path or scan roots | 23 |
| Retained WIT source scan misparses declarations | 1 |
| Kubernetes discovery unavailable | 1 |
| Deployment prerequisite declaration mismatch | 1 |
| Deployment credential expectation mismatch | 1 |

All **106 named failures** follow. Package, Cargo target, and case names are exact retained identifiers.

| Package / target | Failed case | Classification | Actual cause | Evidence |
| --- | --- | --- | --- | --- |
'''.format(source=source)
rows = []
for failure in value['failures']:
    cause = failure['cause'] or failure.get('reviewed_explanation', '')
    rows.append('| ' + ' | '.join([
        cell(f"`{failure['package']}` `{failure['cargo_target']}`"),
        cell('`' + failure['name'] + '`'), cell(failure['classification'].replace('_', ' ')),
        cell(cause), log_link(failure['diagnostic_start_log_line'], failure['diagnostic_end_log_line']),
    ]) + ' |')
tail = '''

Two prior failures were removed: `plugins::wamn_jetstream::tests::live_generic_guest_cannot_write_the_provisioned_tap_stream` at `e02ad16b`, and `a_pointer_flip_makes_the_cache_serve_the_new_active_version` in `wiring_doorbell_live` at `a33e5709`. The removed explicit skip is `plugins::wamn_jetstream::tests::live_publish_dedupe_bind_fetch_ack` at `e02ad16b`. Each is a source removal, not a passing test. [Removal records](known-test-removals.json).

Across all case names, Stage 1 to Stage 2 removes 49 occurrences and adds 22: a net reduction of 27. This equals the change from 2298 to 2271 reported test outcomes. The changes include custom event handling deletion, unused mutable Active tests, and operator/workspace test changes. [Named case deltas](test-case-delta.json).

The retained reducer reports no unresolved target, footer, compilation, exit, or duplicate-identity entry. Its native-B child repeats the parent's full case name and hides the parent's diagnostic in the reduced record; manual inspection of log lines 1957–1980 confirms the same missing `WAMN_NATIVE_B_AUTH_PG_URL` input. That full block is retained in the current classification; the original reduction is unchanged. [Raw reduction](reduction-001/workspace-results.json).

All **84 explicit self-skips** follow. The comparison also checked target executable names, so identical `unittests src/lib.rs` descriptions cannot exchange owners. No new skip appeared; the one absent skip is the C removal above. Reported passes include these skipped cases, and silent early returns can remain invisible. Removing the known skips yields 2081 reported test passes plus six doctests, not an exact executed-test count. [Skip records](explicit-self-skips.json).

| Target executable / description | Case | Explicit skip message | Evidence |
| --- | --- | --- | --- |
'''
skip_rows = []
for skip in value['explicit_self_skips']['entries']:
    executable = re.sub(r'-[0-9a-f]+$', '', Path(skip['target_executable']).name)
    skip_rows.append('| ' + ' | '.join([
        cell(f"`{executable}` / `{skip['target_description']}`"),
        cell('`' + skip['name'] + '`'), cell(skip['message']), log_link(skip['diagnostic_log_line']),
    ]) + ' |')
(out / 'report.md').write_text(header + '\n'.join(rows) + tail + '\n'.join(skip_rows) + '\n')
shutil.copyfile(Path(__file__), out / 'finalize-classification.py')
shutil.copyfile(Path(__file__).with_name('reduce.py'), out / 'reduce-current.py')
# The copied preparation program is recorded for provenance, not installed as a tool.
write('classification-summary.json', {
    'source': source, 'exit_code': raw['exit_code'], 'counts': raw['counts'],
    'classification_counts': value['classification_counts'],
    'added_failure_identities': 24, 'removed_failure_identities': 2,
    'changed_retained_causes_vs_step1': 1, 'unchanged_causes_vs_step1': 81,
    'changed_retained_causes_vs_baseline': 2, 'unchanged_causes_vs_baseline': 80,
    'explicit_self_skips': 84, 'added_explicit_self_skips': 0, 'removed_explicit_self_skips': 1,
    'unresolved_classifications': [], 'source_stable': True,
    'tracked_files': len(before), 'log_sha256': run['workspace_log_sha256'],
    'report_sha256': hashlib.sha256((out / 'report.md').read_bytes()).hexdigest(),
})
print(json.dumps({'classification_counts': value['classification_counts'], 'named_failure_rows': len(rows),
                  'named_skip_rows': len(skip_rows), 'source': source, 'report': str(out / 'report.md')}, indent=2))
