#!/usr/bin/env python3
"""Reuse the comparator on D's retained result without running a workspace sweep."""
import importlib.util
import json
from pathlib import Path
import sys

sys.dont_write_bytecode = True
evidence = Path(__file__).resolve().parent
script = evidence.parent / 'tools/compare_integrated_sweep.py'
compile(script.read_bytes(), str(script), 'exec')
spec = importlib.util.spec_from_file_location('postcommit_sweep_comparison', script)
comparison = importlib.util.module_from_spec(spec)
spec.loader.exec_module(comparison)
reducer = comparison.load(comparison.REDUCER, 'retained_workspace_classifier')
causes = comparison.load(comparison.CAUSES, 'retained_workspace_causes')
reference = comparison.REFERENCES[comparison.LATEST_REFERENCE]
current = json.loads(reference.read_text())
source = reference.with_name('source.txt').read_text().strip()
assert source == 'dce31af910ba7b586e3537ab2b1cfea784ea2066'
results = {name: comparison.compare(current, json.loads(path.read_text()), reducer, causes)
           for name, path in comparison.REFERENCES.items()}
latest = results[comparison.LATEST_REFERENCE]
exit_code = 2 if (latest['changed_causes'] or latest['added_failures']
                 or latest['absent_reference_failures'] or latest['current_unresolved']
                 or latest['duplicate_failure_identities']) else 0
assert exit_code == 0
assert len(latest['exact_identity_and_cause_matches']) == current['counts']['test_failed'] == 81
assert latest['explicit_self_skip_names_and_target_multiplicities_equal']
assert current['explicit_self_skips']['count'] == 85
result = {
    'scope': 'Syntax check and offline reuse of compare() on the retained HTTP reuse result. No tests, builds, databases, or live jobs.',
    'latest_reference': comparison.LATEST_REFERENCE, 'source': source,
    'latest_comparison_exit_code': exit_code, 'counts': current['counts'],
    'combined_reported_counts': current['combined_reported_counts'],
    'comparisons': {name: {key: len(value) if isinstance(value, list) else value
                           for key, value in compared.items()}
                    for name, compared in results.items()},
    'inputs': [reducer.file_receipt(path) for path in
               (reference, reference.with_name('source.txt'), comparison.REDUCER, comparison.CAUSES,
                *comparison.REFERENCES.values(), script, Path(__file__))],
}
with (evidence / 'result.json').open('x') as output:
    json.dump(result, output, indent=2)
    output.write('\n')
print(json.dumps({'latest_comparison_exit_code': exit_code,
                  'failure_identity_and_cause_matches': len(latest['exact_identity_and_cause_matches']),
                  'explicit_self_skips': current['explicit_self_skips']['count']}))
