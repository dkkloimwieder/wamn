import ast
import copy
import hashlib
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import subprocess

root = Path('/tmp/wamn-cutover-telemetry-live-prepared')
helper = root / 'journey-telemetry-proof'
loader = importlib.machinery.SourceFileLoader('telemetry_proof', str(helper))
spec = importlib.util.spec_from_loader(loader.name, loader)
module = importlib.util.module_from_spec(spec)
loader.exec_module(module)
fixture_path = Path('/home/kaalin/.cache/wamn-lanes/wasmcloud-2-9-20260909/docs/perf/2026.09/1c-scope-split/journey/trace-steady.json')
fixture = json.loads(fixture_path.read_text())
# This retained real trace predates the now-recorded credential-kind field.
# Add only that field to exercise the current contract without a live exporter.
for batch in fixture['batches']:
    for scope in batch['scopeSpans']:
        for span in scope['spans']:
            if span['name'] == 'wamn.component.invoke':
                span['attributes'].append({'key': 'wamn.caller_credential_kind', 'value': {'stringValue': 'pat'}})
args = ('33333333333333333333333333333333', 'receiving-route-auth', 'receiving', 'dev')
checks = []
value = module.trace_receipt(fixture, *args)
assert len(value['invocations']) == 1 and len(value['postgres_effects']) == 1
checks.append('Retained real trace shape with one current credential-kind field passes')

def refuses(label, thunk):
    try:
        thunk()
    except (ValueError, KeyError):
        checks.append(label)
    else:
        raise AssertionError(label)

for omitted in ('wamn.component.invoke', 'wamn.postgres'):
    broken = copy.deepcopy(fixture)
    for batch in broken['batches']:
        for scope in batch['scopeSpans']:
            scope['spans'] = [span for span in scope['spans'] if span['name'] != omitted]
    refuses('Missing ' + omitted + ' is refused', lambda: module.trace_receipt(broken, *args))
refuses('Another trace identifier is refused', lambda: module.trace_receipt(fixture, '4' * 32, *args[1:]))
refuses('Another tenant is refused', lambda: module.trace_receipt(fixture, args[0], 'wrong', *args[2:]))
broken = copy.deepcopy(fixture)
for batch in broken['batches']:
    for scope in batch['scopeSpans']:
        for span in scope['spans']:
            if span['name'] == 'wamn.postgres':
                span['parentSpanId'] = '9999999999999999'
refuses('An effect without an invocation ancestor is refused', lambda: module.trace_receipt(broken, *args))
metrics = '\n'.join([
    'guest_invocation_duration_count{workload_namespace="wamn-receiving-journey",component="flow-http",plugin="wasi-http",http_request_method="POST",operation="wasi:http/incoming-handler#handle"} 2',
    'wamn_postgres_query_duration_ms_count{db_operation="statement.run",wamn_project="receiving"} 1',
    'wamn_jetstream_duration_ms_count{effect_operation="next",wamn_project="receiving"} 1',
])
assert len(module.metric_receipt(metrics, 'wamn-receiving-journey', 'receiving')) == 3
checks.append('All three selected histogram count names and scoped labels pass')
for name in ('guest_invocation_duration_count', 'wamn_postgres_query_duration_ms_count', 'wamn_jetstream_duration_ms_count'):
    broken = '\n'.join(line for line in metrics.splitlines() if not line.startswith(name))
    refuses('Missing ' + name + ' is refused', lambda: module.metric_receipt(broken, 'wamn-receiving-journey', 'receiving'))
refuses('Another metric namespace is refused', lambda: module.metric_receipt(metrics, 'kind-wamn', 'receiving'))
refuses('Another metric project is refused', lambda: module.metric_receipt(metrics, 'wamn-receiving-journey', 'wrong'))
refuses('A non-finite count is refused', lambda: module.metric_receipt(metrics.replace(' 2\n', ' NaN\n'), 'wamn-receiving-journey', 'receiving'))
ast.parse(helper.read_text(), filename=str(helper))
checks.append('Helper Python AST passes')
subprocess.run(['bash', '-n', str(root / 'receiving-cluster-journey-run')], check=True)
checks.append('Prepared runner Bash syntax passes')
receipt = {'verdict': 'pass', 'checks': checks, 'fixture': str(fixture_path),
           'fixture_sha256': hashlib.sha256(fixture_path.read_bytes()).hexdigest(),
           'fixture_adjustment': 'Add current wamn.caller_credential_kind=pat only to existing invocation spans',
           'scope': 'Offline parser controls and syntax only. No Cargo, Docker, Kubernetes, network, or live telemetry ran.'}
(root / 'static-validation.json').write_text(json.dumps(receipt, indent=2) + '\n')
print(json.dumps(receipt, indent=2))
