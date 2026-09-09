#!/usr/bin/env python3
"""Replay the owning probe assertion against the captured Kubernetes object."""
import copy
import hashlib
import json
from pathlib import Path
import subprocess

LANE = Path('/home/kaalin/.cache/wamn-lanes/wasmcloud-2-9-20260909')
ROOT = Path(__file__).resolve().parent.parent
OUT = Path(__file__).resolve().parent
SOURCE = 'tools/receiving-cluster-journey-run'
CAPTURE = ROOT / 'performance-2-9-001/journey/host-measurement-deployment.json'


def assertion(source):
    start = source.index("  jq -e '\n    .spec.replicas == 1")
    end = source.index('\n  measurement_selector=', start)
    return source[start:end].split("  jq -e '\n", 1)[1].rsplit("\n  ' >/dev/null", 1)[0]


before = subprocess.run(['git', 'show', f'HEAD:{SOURCE}'], cwd=LANE,
                        capture_output=True, text=True, check=True).stdout
after = (LANE / SOURCE).read_text()
captured = json.loads(CAPTURE.read_text())
cases = [('original_assertion', assertion(before), captured, 1),
         ('corrected_assertion', assertion(after), captured, 0)]
for probe in ('livenessProbe', 'readinessProbe', 'startupProbe'):
    for field, value in (('path', '/wrong'), ('port', 'wrong'), ('scheme', 'HTTPS')):
        changed = copy.deepcopy(captured)
        changed['spec']['template']['spec']['containers'][0][probe]['httpGet'][field] = value
        cases.append((f'{probe}_{field}_refused', assertion(after), changed, 1))
    changed = copy.deepcopy(captured)
    del changed['spec']['template']['spec']['containers'][0][probe]
    cases.append((f'{probe}_missing_refused', assertion(after), changed, 1))

results = []
for name, expression, value, expected in cases:
    result = subprocess.run(['jq', '-e', expression], input=json.dumps(value),
                            capture_output=True, text=True)
    results.append(dict(name=name, exit_code=result.returncode, expected_exit_code=expected,
                        stdout=result.stdout, stderr=result.stderr))
    assert result.returncode == expected, results[-1]

checks = []
for command in (['bash', '-n', SOURCE], ['git', 'diff', '--check']):
    result = subprocess.run(command, cwd=LANE, capture_output=True, text=True)
    checks.append(dict(command=command, exit_code=result.returncode,
                       stdout=result.stdout, stderr=result.stderr))
    assert result.returncode == 0, checks[-1]

(OUT / 'assertion.jq').write_text(assertion(after) + '\n')
patch = subprocess.run(['git', 'diff', '--', SOURCE], cwd=LANE,
                       capture_output=True, text=True, check=True).stdout
(OUT / 'source.patch').write_text(patch)
receipt = dict(bead='wamn-0h0g.2.7.15', base_source='b8e9881dc8afb94971d6f63bdaa844ea9f29c045',
               captured_deployment=str(CAPTURE.relative_to(ROOT)),
               captured_sha256=hashlib.sha256(CAPTURE.read_bytes()).hexdigest(),
               corrected_source_sha256=hashlib.sha256(after.encode()).hexdigest(),
               cases=results, checks=checks,
               scope='Offline replay of the owning assertion against the actual failed-run deployment.',
               limits='No timed traffic ran. Deployment probes, resources, and benchmark limits remain unchanged.')
(OUT / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')
print(json.dumps(dict(cases=len(results), mismatches=0, checks=len(checks))))
