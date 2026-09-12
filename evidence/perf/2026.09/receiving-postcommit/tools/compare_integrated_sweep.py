#!/usr/bin/env python3
"""Compare a finished sweep by exact test identity and retained failure cause."""
import argparse
from collections import Counter
import importlib.util
import json
from pathlib import Path
import sys

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[5]
MONTH = ROOT / 'docs/perf/2026.09'
REDUCER = MONTH / 'wasmcloud-2-9-cutover/workspace-integration-preparation-001/classify.py'
CAUSES = MONTH / 'ctc8-20-pat-service/main-landing-001/compare.py'
CLASSIFIER_BASELINE = MONTH / 'wasmcloud-2-9-cutover/validation-001/workspace-results.json'
REFERENCES = {
    'p3': MONTH / 'p3-http-cutover/integrated-workspace-001/workspace-results.json',
    'pat': MONTH / 'ctc8-20-pat-service/main-landing-001/workspace-results.json',
    'http_reuse': MONTH / 'ctc8-16-http-reuse/integrated-workspace-001/workspace-results.json',
}
LATEST_REFERENCE = 'http_reuse'


def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def write(path, value):
    with path.open('x') as output:
        json.dump(value, output, indent=2)
        output.write('\n')


def compare(current, reference, reducer, causes):
    old, old_duplicates = causes.index(reference['failures'], reducer)
    new, new_duplicates = causes.index(current['failures'], reducer)
    exact, changed = [], []
    for key in sorted(old.keys() & new.keys()):
        before, after = causes.cause(old[key], reducer), causes.cause(new[key], reducer)
        if before == after:
            exact.append(list(key))
        else:
            changed.append({'identity': list(key), 'before': before, 'after': after})
    skip_key = lambda item: (item['target_description'], item['name'])
    old_skips = Counter(map(skip_key, reference['explicit_self_skips']['entries']))
    new_skips = Counter(map(skip_key, current['explicit_self_skips']['entries']))
    return {
        'reference_failures': len(old), 'current_failures': len(new),
        'exact_identity_and_cause_matches': exact, 'changed_causes': changed,
        'added_failures': [{'identity': list(key), 'cause': causes.cause(new[key], reducer)}
                           for key in sorted(new.keys() - old.keys())],
        'absent_reference_failures': [list(key) for key in sorted(old.keys() - new.keys())],
        'duplicate_failure_identities': old_duplicates + new_duplicates,
        'reference_unresolved': reference['unresolved'], 'current_unresolved': current['unresolved'],
        'explicit_self_skip_names_and_target_multiplicities_equal': old_skips == new_skips,
        'added_explicit_self_skips': [list(key) for key in (new_skips - old_skips).elements()],
        'absent_explicit_self_skips': [list(key) for key in (old_skips - new_skips).elements()],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--evidence-dir', type=Path, required=True)
    args = parser.parse_args()
    evidence = args.evidence_dir.resolve(strict=True)
    reducer = load(REDUCER, 'retained_workspace_classifier')
    causes = load(CAUSES, 'retained_workspace_causes')
    current = reducer.classify(evidence / 'workspace.log', CLASSIFIER_BASELINE,
        int((evidence / 'exit-code.txt').read_text()),
        json.loads((evidence / 'environment-names.json').read_text()))
    write(evidence / 'workspace-results.json', current)
    comparisons = {name: compare(current, json.loads(path.read_text()), reducer, causes)
                   for name, path in REFERENCES.items()}
    result = {
        'schema': 'wamn-receiving-postcommit-workspace-comparison/v1',
        'source': (evidence / 'source.txt').read_text().strip(),
        'exit_code': current['exit_code'], 'counts': current['counts'],
        'combined_reported_counts': current['combined_reported_counts'],
        'comparisons': comparisons,
        'latest_reference': LATEST_REFERENCE,
        'latest_reference_source': REFERENCES[LATEST_REFERENCE].with_name('source.txt').read_text().strip(),
        'inputs': [reducer.file_receipt(path) for path in
                   (REDUCER, CAUSES, CLASSIFIER_BASELINE, *REFERENCES.values(),
                    REFERENCES[LATEST_REFERENCE].with_name('source.txt'), Path(__file__))],
        'limitations': [
            'A reported pass that explicitly skips live work is not an executed live proof.',
            'The explicit self-skip count is a lower bound. Silent early returns remain possible.',
            'An absent failure is not a repair claim. Inspect its current target and case.',
            'The comparison retains the original Cargo exit and supplies no acceptance verdict.',
            'The separate PG18 observer and paired cluster receipts keep their own scope.',
        ],
    }
    write(evidence / 'workspace-comparison.json', result)
    print(json.dumps({'exit_code': current['exit_code'], 'counts': current['counts'],
        'comparisons': {name: {key: (len(value) if isinstance(value, list) else value)
                              for key, value in comparison.items()}
                        for name, comparison in comparisons.items()}}, indent=2))
    latest = comparisons[LATEST_REFERENCE]
    return 2 if (latest['changed_causes'] or latest['added_failures'] or latest['absent_reference_failures']
                 or latest['current_unresolved'] or latest['duplicate_failure_identities']) else 0


if __name__ == '__main__':
    raise SystemExit(main())
