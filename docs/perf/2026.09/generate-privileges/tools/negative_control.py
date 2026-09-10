#!/usr/bin/env python3
"""Prove the privilege refusal test detects a misreported SQL lock value."""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--tree', type=Path, required=True)
parser.add_argument('--baseline-dir', type=Path, required=True)
parser.add_argument('--evidence-dir', type=Path, required=True)
args = parser.parse_args()
tree = args.tree.resolve(strict=True)
evidence = args.evidence_dir.resolve()
evidence.mkdir(parents=True, exist_ok=False)
relative = 'crates/schema/generator/src/generate.rs'
source = tree / relative
original = source.read_bytes()
digest = hashlib.sha256(original).hexdigest()
baseline = json.loads((args.baseline_dir / 'result.json').read_text())
baseline_source = json.loads((args.baseline_dir / 'source.json').read_text())
assert baseline['exit_code'] == 0, 'the unmodified generation tests must pass first'
baseline_hashes = baseline_source['changed_source_sha256']
if relative in baseline_hashes:
    baseline_digest = baseline_hashes[relative]
else:
    baseline_bytes = subprocess.check_output(['git', 'show', baseline_source['head'] + ':' + relative], cwd=tree)
    baseline_digest = hashlib.sha256(baseline_bytes).hexdigest()
assert baseline_digest == digest, 'the tested source changed'
needle = b'"lock": observed.lock,'
replacement = b'"lock": declared.lock,'
assert original.count(needle) == 1, 'mutate only the verified SQL lock diagnostic'
mutated = original.replace(needle, replacement)
test = 'command_privilege_mismatch_reports_for_update_lock_and_declared_value'
command = ['cargo', 'test', '--locked', '--offline', '-p', 'wamn-schema-generator',
           '--test', 'generation', test, '--', '--exact', '--include-ignored']
capture = tree / 'docs/perf/2026.09/effects-response/tools/capture.py'


def run(name):
    return subprocess.run([sys.executable, str(capture), '--tree', str(tree),
                           '--evidence-dir', str(evidence / name), '--', *command],
                          cwd=tree).returncode


result = {'baseline': str(args.baseline_dir), 'original_sha256': digest,
          'mutated_sha256': hashlib.sha256(mutated).hexdigest(),
          'needle': needle.decode(), 'replacement': replacement.decode(), 'test': test}
try:
    source.write_bytes(mutated)
    result['mutant_exit_code'] = run('mutant')
finally:
    assert source.read_bytes() in (original, mutated), 'a concurrent edit must not be overwritten'
    # A new write changes mtime so Cargo recompiles the restored source.
    source.write_bytes(original)
    result['restored_sha256'] = hashlib.sha256(source.read_bytes()).hexdigest()
    assert result['restored_sha256'] == digest
result['restored_exit_code'] = run('restored')
mutant_log = (evidence / 'mutant/command.log').read_text()
restored_log = (evidence / 'restored/command.log').read_text()
result['named_test_failed'] = f'test {test} ... FAILED' in mutant_log
result['named_test_restored'] = f'test {test} ... ok' in restored_log
result['compiled_mutant'] = 'Finished `test` profile' in mutant_log and 'error: could not compile' not in mutant_log
values = {}
for label in ['left', 'right']:
    match = re.search(r'^\s*' + label + r': ("[^\n]+")$', mutant_log, re.MULTILINE)
    if match:
        values[label] = json.loads(match.group(1))
result['asserted_values'] = values
result['changed_only_reported_sql_lock'] = (
    set(values) == {'left', 'right'} and
    values['left'] == values['right'].replace('"lock":true', '"lock":false', 1)
)
result['passed'] = (result['mutant_exit_code'] == 101 and result['named_test_failed']
                    and result['compiled_mutant'] and result['changed_only_reported_sql_lock']
                    and result['restored_exit_code'] == 0 and result['named_test_restored'])
(evidence / 'result.json').write_text(json.dumps(result, indent=2, sort_keys=True) + '\n')
print(json.dumps(result, sort_keys=True))
raise SystemExit(0 if result['passed'] else 1)
