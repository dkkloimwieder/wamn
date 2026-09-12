#!/usr/bin/env python3
"""Prepare the Stage 2 result with the unchanged retained reducer APIs."""
import argparse
from collections import Counter
import hashlib
import importlib.util
import json
from pathlib import Path
import re
import sys

sys.dont_write_bytecode = True
root = Path('/home/kaalin/dev/wamn')
month = root / 'docs/perf/2026.09'
guidance_path = Path('/tmp/consolidation-step2-classification-guidance.json')
guidance = json.loads(guidance_path.read_text())
paths = {name: Path(value) for name, value in guidance['paths'].items()}


def read(path):
    return json.loads(path.read_text())


def record(path):
    data = path.read_bytes()
    return {'path': str(path), 'bytes': len(data), 'sha256': hashlib.sha256(data).hexdigest()}


def load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def write(out, name, value):
    with (out / name).open('x') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')


def case_rows(reduced, lines, reducer):
    """List first case-name occurrences per target; nested repeats stay excluded."""
    result = []
    for target in reduced['test_targets']:
        if target['category'] != 'test':
            continue
        seen = set()
        for index in range(target['running_log_line'], target['summary_log_line'] - 1):
            case = reducer.CASE.match(lines[index])
            if case is None or case[1] in seen:
                continue
            seen.add(case[1])
            result.append({'name': case[1], 'description': target['description'],
                'executable': target['executable'], 'log_line': index + 1,
                'target_running_log_line': target['running_log_line']})
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--evidence-dir', type=Path, required=True)
    parser.add_argument('--output-dir', type=Path, required=True)
    args = parser.parse_args()
    evidence = args.evidence_dir.resolve(strict=True)
    # Require the outer capture's finished records, not merely a libtest footer.
    capture = read(evidence / 'run.json')
    stability = read(evidence / 'source-stability.json')
    exit_code = int((evidence / 'exit-code.txt').read_text())
    assert capture['exit_code'] == exit_code
    assert capture['workspace_log_sha256'] == record(evidence / 'workspace.log')['sha256']
    assert stability['head_unchanged'] and stability['bytes_and_modes_unchanged']
    assert capture['source_head'] == stability['head_before'] == stability['head_after']
    assert not args.output_dir.exists(), 'Preserve prior results; choose a fresh output directory.'
    for name in ['retained_runner', 'retained_comparator', 'retained_reducer', 'retained_cause_normalizer',
                 'reducer_legacy_baseline', 'baseline_reduced', 'step1_reduced',
                 'baseline_classified', 'step1_classified']:
        assert record(paths[name])['sha256'] == guidance['input_sha256'][name], name
    comparator = load(paths['retained_comparator'], 'stage2_retained_comparator')
    reducer = comparator.load(paths['retained_reducer'], 'stage2_retained_reducer')
    causes = comparator.load(paths['retained_cause_normalizer'], 'stage2_retained_causes')
    environment = read(evidence / 'environment-names.json')
    assert environment['explicitly_armed_names'] == []
    current = reducer.classify(evidence / 'workspace.log', paths['reducer_legacy_baseline'], exit_code, environment)
    references = {name: read(paths[name + '_reduced']) for name in ['baseline', 'step1']}
    comparisons = {name: comparator.compare(current, reference, reducer, causes)
                   for name, reference in references.items()}
    out = args.output_dir.resolve()
    out.mkdir(parents=True)
    write(out, 'workspace-results.json', current)
    for name, comparison in comparisons.items():
        comparison['source'] = capture['source_head']
        comparison['reference_source'] = guidance['sources'][name]
        comparison['commands_equal'] = read(evidence / 'command.json') == read(paths[name + '_command'])
        comparison['counts'] = current['counts']
        comparison['reference_counts'] = references[name]['counts']
        write(out, name + '-comparison.json', comparison)
    current_lines = [reducer.ANSI.sub('', line) for line in (evidence / 'workspace.log').read_text(errors='replace').splitlines()]
    old_classified = {tuple(entry[key] for key in ['package', 'cargo_target', 'name']): entry
                      for entry in read(paths['step1_classified'])['failures']}
    exact = {tuple(identity) for identity in comparisons['step1']['exact_identity_and_cause_matches']}
    classified = []
    review = []
    for failure in current['failures']:
        identity = reducer.identity(failure)
        prior = old_classified.get(identity)
        cause = causes.cause(failure, reducer)
        entry = {key: failure[key] for key in ['package', 'cargo_target', 'name',
                 'diagnostic_start_log_line', 'diagnostic_end_log_line', 'failure_list_log_line']}
        entry.update(classification=prior['classification'] if identity in exact else 'requires_current_cause_review',
                     cause='\n'.join(cause['causal_lines']), normalized_cause=cause,
                     required_inputs=[name for name in failure['referenced_arming_inputs'] if name != 'PG18'],
                     raw_diagnostics=failure['diagnostic_excerpt'],
                     step1_comparison='exact normalized diagnostic match' if identity in exact else 'review required')
        if prior:
            entry['step1_classification'] = prior['classification']
            entry['step1_cause'] = prior['cause']
        # The retained native-B child repeats its parent's name. Keep its original
        # reducer output, and list every raw occurrence for manual cause review.
        if identity == ('wamn-execution-host', '--lib',
                        'router_driver::native_policy::tests::authenticated::native_authenticated_nested_authority_and_lifecycle'):
            occurrences = [number + 1 for number, line in enumerate(current_lines)
                           if (match := reducer.CASE.match(line)) and match[1] == failure['name']]
            if len(occurrences) > 1:
                entry['same_name_raw_occurrences'] = occurrences
                review.append({'kind': 'nested_case_diagnostic', 'identity': list(identity), 'log_lines': occurrences})
        if identity not in exact:
            review.append({'kind': 'current_failure_cause', 'identity': list(identity)})
        classified.append(entry)
    current_cases = case_rows(current, current_lines, reducer)
    case_deltas = {}
    for name, reference in references.items():
        old_lines = [reducer.ANSI.sub('', line) for line in paths[name + '_log'].read_text(errors='replace').splitlines()]
        old_cases = case_rows(reference, old_lines, reducer)
        # The name list is supplementary. Exact failure keys above remain the
        # package + Cargo target + case identity and preserve every cause.
        old_names = Counter(row['name'] for row in old_cases)
        new_names = Counter(row['name'] for row in current_cases)
        removed = old_names - new_names
        added = new_names - old_names
        case_deltas[name] = {
            'removed_case_occurrences': [row for row in old_cases if row['name'] in removed],
            'added_case_occurrences': [row for row in current_cases if row['name'] in added],
            'removed_name_multiplicities': dict(removed), 'added_name_multiplicities': dict(added),
            'interpretation': 'Occurrence changes require source reconciliation. A removed case is not a pass.',
        }
    known_removals = read(Path(__file__).with_name('known-removals.json'))
    disposition = {}
    for name, comparison in comparisons.items():
        rows = []
        for identity in comparison['absent_reference_failures']:
            known = next((row for row in known_removals['failures'] if row['identity'] == identity), None)
            rows.append({'identity': identity, 'disposition': 'removed test, not a pass' if known else 'requires source reconciliation',
                         'source_change': known})
            if not known:
                review.append({'kind': 'absent_failure', 'reference': name, 'identity': identity})
        disposition[name] = rows
    targets_by_line = {target['running_log_line']: target for target in current['test_targets']}
    skips = []
    for skip in current['explicit_self_skips']['entries']:
        skip = dict(skip)
        target = targets_by_line[skip['target_running_log_line']]
        skip['target_executable'] = target['executable']
        skip['required_inputs'] = [name for name in skip['referenced_arming_inputs'] if name != 'PG18']
        skip['interpretation'] = 'Explicitly skipped work was not executed.'
        skips.append(skip)
    for name, comparison in comparisons.items():
        for kind in ['added_explicit_self_skips', 'absent_explicit_self_skips']:
            for identity in comparison[kind]:
                known = next((row for row in known_removals['skips'] if row['identity'] == identity), None)
                review.append({'kind': kind, 'reference': name, 'identity': identity,
                               'known_source_removal': known})
    write(out, 'classified-failures-draft.json', {
        'source': capture['source_head'], 'exit_code': exit_code, 'source_stable': True,
        'classification_counts': dict(Counter(entry['classification'] for entry in classified)),
        'failures': classified,
        'explicit_self_skips': {**current['explicit_self_skips'], 'entries': skips},
        'removed_failure_dispositions': disposition,
        'unresolved_reducer_entries': current['unresolved'],
        'manual_review_required': review,
        'interpretation': 'Draft classification. Every changed/new/absent case needs current diagnostic or source review. No deleted test is counted as passing.',
    })
    write(out, 'test-case-delta-draft.json', case_deltas)
    write(out, 'classification-inputs.json', [record(paths[name]) for name in
        ['retained_comparator', 'retained_reducer', 'retained_cause_normalizer', 'reducer_legacy_baseline',
         'baseline_reduced', 'step1_reduced', 'baseline_classified', 'step1_classified']]
        + [record(evidence / name) for name in ['run.json', 'exit-code.txt', 'workspace.log', 'source-stability.json',
                                               'environment-names.json', 'command.json']])
    print(json.dumps({'source': capture['source_head'], 'exit_code': exit_code,
                      'counts': current['counts'], 'review_entries': len(review),
                      'unresolved': current['unresolved'], 'output': str(out)}, indent=2))


if __name__ == '__main__':
    main()
