#!/usr/bin/env python3
"""Replay retained operator identity and explicitly synthetic cause receipts."""
import ast
from collections import Counter
import copy
import datetime
import hashlib
import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
EVIDENCE = Path('/home/kaalin/dev/wamn/docs/perf/2026.09/wasmcloud-2-9-cutover/live-receiving-005/journey/operator-recovery')
SOURCE = Path('/home/kaalin/.cache/wamn-lanes/wasmcloud-2-9-20260909/tools/receiving-operator-recovery-run')
proposed = ast.parse((HERE / 'receiving-operator-recovery-run.proposed').read_text())
selected = [node for node in proposed.body if isinstance(node, (ast.Import, ast.ImportFrom)) or
            isinstance(node, ast.FunctionDef) and node.name in ('require', 'operator_state', 'supervised_restart')]
module = ast.Module(body=selected, type_ignores=[])
namespace = {}
exec(compile(module, '<offline selected pure functions>', 'exec'), namespace)
state = namespace['operator_state']
check = namespace['supervised_restart']
before = state(json.loads((EVIDENCE / 'before-state.json').read_text())['operator_pods'])
after = state(json.loads((EVIDENCE / 'scheduler-recovery-state.json').read_text())['operator_pods'])
phases = json.loads((EVIDENCE / 'phases.json').read_text())
since = phases['scheduler-stopped']['started']
new_log = (EVIDENCE / '0355-final-operator-log.stdout').read_text()
try:
    check(before, after, new_log, {'items': []}, since)
except RuntimeError as error:
    retained_refusal = str(error)
else:
    raise AssertionError('005 must not become a pass without missing cause receipts')

synthetic_log = '2026-09-09T16:33:20Z ERROR setup nats connection closed\n'
synthetic_events = {'items': [dict(
    metadata={'name': 'synthetic-only', 'uid': 'synthetic-event'},
    involvedObject={'uid': after['pod_uid']}, reason='Killing',
    reportingComponent='kubelet',
    message='Container runtime-operator failed liveness probe, will be restarted',
    lastTimestamp='2026-09-09T16:34:07Z')]} 
accepted = check(before, after, synthetic_log, synthetic_events, since)
assert accepted['current']['ready'] is False
controls = []
for name, change in [
    ('different Pod', lambda value: value.update(pod_uid='different')),
    ('different image', lambda value: value.update(image_id='different')),
    ('missing restart history', lambda value: value.update(restart_count=4)),
    ('wrong previous process', lambda value: value['termination'].update(containerID='different')),
    ('OOM or failed exit', lambda value: value['termination'].update(exitCode=137, reason='OOMKilled')),
]:
    value = copy.deepcopy(after)
    change(value)
    try:
        check(before, value, synthetic_log, synthetic_events, since)
    except RuntimeError:
        controls.append(name)
    else:
        raise AssertionError(name)
for name, log, events in [
    ('missing terminal closure log', new_log, synthetic_events),
    ('missing kubelet event', synthetic_log, {'items': []}),
    ('stale kubelet event', synthetic_log, {'items': [dict(synthetic_events['items'][0], lastTimestamp='2026-09-09T16:00:00Z')]}),
    ('wrong Pod event', synthetic_log, {'items': [dict(synthetic_events['items'][0], involvedObject={'uid': 'other'})]}),
]:
    try:
        check(before, after, log, events, since)
    except RuntimeError:
        controls.append(name)
    else:
        raise AssertionError(name)
original = ast.parse(SOURCE.read_text())
def function(tree, name):
    return next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == name)
assert ast.dump(function(original, 'classify_sample')) == ast.dump(function(proposed, 'classify_sample'))
assert ast.dump(function(original, 'host_ids')) == ast.dump(function(proposed, 'host_ids'))
assert ast.dump(function(original, 'pod_ids')) == ast.dump(function(proposed, 'pod_ids'))
constants = {node.targets[0].id: node.value.value for node in proposed.body if isinstance(node, ast.Assign)
             and isinstance(node.targets[0], ast.Name) and isinstance(node.value, ast.Constant)}
assert constants['RECOVERY_SECONDS'] == 120 and constants['OUTAGE_SECONDS'] == 150
# Exercise the real final capture block with an already removed original Pod
# and one replacement; the stub returns bytes and never invokes kubectl.
main_node = function(proposed, 'main')
final_body = next(node.finalbody for node in main_node.body if isinstance(node, ast.Try) and node.finalbody)
def call_label(node):
    if isinstance(node, ast.Expr) and isinstance(node.value, ast.Call) and node.value.args:
        arg = node.value.args[0]
        return arg.value if isinstance(arg, ast.Constant) else None
start = next(i for i, node in enumerate(final_body) if call_label(node) == 'final-operator-log')
stop = next(i for i, node in enumerate(final_body) if call_label(node) == 'cleanup.json')
calls = []
def capture_stub(label, arguments, **options):
    calls.append((label, arguments))
    if label == 'final-operator-pods':
        return json.dumps({'items': [{'metadata': {'uid': 'replacement-uid', 'name': 'replacement-pod'}}]}).encode()
    return b''
capture_scope = dict(run=capture_stub, kube=['kubectl'], SYSTEM='wamn-system', json=json,
                     operator_initial={'pod_uid': 'removed-uid', 'pod_name': 'removed-pod'})
exec(compile(ast.Module(body=final_body[start:stop], type_ignores=[]), '<offline stubbed final capture>', 'exec'), capture_scope)
for name in ('removed-pod', 'replacement-pod'):
    assert any('pod/' + name in argv and '--previous' in argv for _, argv in calls)
    assert any('pod/' + name in argv and '--previous' not in argv for _, argv in calls)
for uid in ('removed-uid', 'replacement-uid'):
    assert any('involvedObject.uid=' + uid in argv for _, argv in calls)
samples = json.loads((EVIDENCE / 'route-samples.json').read_text())
stopped = phases['scheduler-stopped']
outage_samples = [s for s in samples if stopped['started'] <= s['timestamp'] <= stopped['ended']]

def receipt(path):
    raw = path.read_bytes()
    return dict(path=str(path), bytes=len(raw), sha256=hashlib.sha256(raw).hexdigest())

report = dict(
    scope='Offline source and retained-evidence inspection only; no live command, build, or worktree edit.',
    source_commit='0388e3bc98d231f1e69538e613ec33689c041653',
    native_source_commit='68ebece9c537f8bb4b5c9999f274ec68d60f35a9',
    receiving005=dict(
        result='fail; not reclassified', previous=before, observed=after,
        outage_seconds=stopped['actual_seconds'], native_guard_host_count=len(stopped['fleet_guard_host_uids']),
        outage_route_classifications=dict(Counter(s['result'] for s in outage_samples)),
        cause='Unproven: previous container log and operator namespace kubelet events were not captured.',
        candidate_replay_refusal=retained_refusal),
    synthetic_controls=dict(scope='Fabricated cause receipts only; not evidence of Receiving005 cause.',
                            supported_transition_with_unready_new_container='accepted', refusal_cases=controls),
    structural_checks=dict(exact_route_classifier_unchanged=True, host_id_and_process_functions_unchanged=True,
                           recovery_ceiling_seconds=120, required_outage_seconds=150, python_ast='pass',
                           removed_and_replacement_pod_final_diagnostics='pass with stubbed commands'),
    retained_inputs=[receipt(EVIDENCE / name) for name in ['before-state.json', 'scheduler-recovery-state.json',
                    'phases.json', '0355-final-operator-log.stdout', 'commands.jsonl', 'route-samples.json']],
    prepared_files=[receipt(HERE / name) for name in ['operator-supervision.patch',
                   'receiving-operator-recovery-run.proposed', 'prepare.py', 'validate_offline.py']])
(HERE / 'offline-validation.json').write_text(json.dumps(report, indent=2) + '\n')
print(json.dumps({'retained_005_refused': retained_refusal, 'synthetic_refusal_controls': len(controls),
                  'structural_checks': report['structural_checks']}, indent=2))
