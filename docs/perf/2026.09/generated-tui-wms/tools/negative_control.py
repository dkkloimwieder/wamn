#!/usr/bin/env python3
"""Prove the WMS wire test detects an altered read revision, then restore the source."""
import argparse
import hashlib
import json
import os
import re
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--tree', type=Path, required=True)
parser.add_argument('--baseline-dir', type=Path, required=True)
parser.add_argument('--evidence-dir', type=Path, required=True)
args = parser.parse_args()
tree = args.tree.resolve(strict=True)
evidence = args.evidence_dir.resolve()
evidence.mkdir(parents=True, exist_ok=False)
relative = 'crates/client/terminal/examples/wms_move.rs'
source = tree / relative
original = source.read_bytes()
digest = hashlib.sha256(original).hexdigest()
baseline = json.loads((args.baseline_dir / 'result.json').read_text())
baseline_source = json.loads((args.baseline_dir / 'source.json').read_text())
assert baseline['exit_code'] == 0, 'the unmodified gate must pass first'
assert baseline_source['changed_source_sha256'][relative] == digest, 'the tested source changed'
needle = b'let revision = row["row_version"].clone();'
replacement = b'let revision = serde_json::json!(row["row_version"].as_i64().unwrap() + 1);'
assert original.count(needle) == 1, 'mutate only the read-to-command revision binding'
mutated = original.replace(needle, replacement)
command = ['cargo', 'test', '--locked', '--offline', '-p', 'wamn-client-terminal', '--example', 'wms_move',
           'tests::the_bound_move_sends_exact_bytes_and_displays_the_label_key', '--', '--exact', '--include-ignored']
env = os.environ.copy()
env.pop('CARGO_TARGET_DIR', None)
env['RUSTC_WRAPPER'] = ''
result = {'baseline': str(args.baseline_dir), 'original_sha256': digest,
          'mutated_sha256': hashlib.sha256(mutated).hexdigest(), 'command': command}
try:
    source.write_bytes(mutated)
    with (evidence / 'mutant.log').open('w') as log:
        result['mutant_exit_code'] = subprocess.run(command, cwd=tree, env=env, stdout=log,
                                                    stderr=subprocess.STDOUT).returncode
finally:
    # A new write changes mtime, so Cargo must compile the restored implementation.
    source.write_bytes(original)
    result['restored_sha256'] = hashlib.sha256(source.read_bytes()).hexdigest()
    assert result['restored_sha256'] == digest
with (evidence / 'restored.log').open('w') as log:
    result['restored_exit_code'] = subprocess.run(command, cwd=tree, env=env, stdout=log,
                                                 stderr=subprocess.STDOUT).returncode
mutant_log = (evidence / 'mutant.log').read_text()
result['named_test_failed'] = 'test tests::the_bound_move_sends_exact_bytes_and_displays_the_label_key ... FAILED' in mutant_log
result['compiled_mutant'] = 'Finished `test` profile' in mutant_log and 'error: could not compile' not in mutant_log
wire = {}
for label in ['left', 'right']:
    match = re.search(r'^\s*' + label + r': (\[[^\n]+\])$', mutant_log, re.MULTILINE)
    if match:
        wire[label] = json.loads(bytes(json.loads(match.group(1))))
if set(wire) == {'left', 'right'}:
    result['wire_actual'] = wire['left']
    result['wire_expected'] = wire['right']
    actual = wire['left'][0]['value']['expected_row_version']
    expected = wire['right'][0]['value']['expected_row_version']
    wire['left'][0]['value']['expected_row_version'] = expected
    result['changed_only_bound_revision'] = actual == 8 and expected == 7 and wire['left'] == wire['right']
    wire['left'][0]['value']['expected_row_version'] = actual
result['passed'] = (result['mutant_exit_code'] == 101 and result['named_test_failed']
                    and result['compiled_mutant'] and result.get('changed_only_bound_revision', False)
                    and result['restored_exit_code'] == 0)
(evidence / 'result.json').write_text(json.dumps(result, indent=2, sort_keys=True) + '\n')
print(json.dumps(result, sort_keys=True))
raise SystemExit(0 if result['passed'] else 1)
