#!/usr/bin/env python3
"""Retain every sweep failure and review only supported setup refusals."""
import argparse
from collections import Counter
import gzip
import hashlib
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys

sys.dont_write_bytecode = True
root = Path(__file__).resolve().parents[6]


def read(path):
    return json.loads(path.read_text())


def write(directory, name, value):
    with (directory / name).open('x') as stream:
        json.dump(value, stream, indent=2)
        stream.write('\n')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--evidence-dir', type=Path, required=True)
    parser.add_argument('--reduction-dir', type=Path, required=True)
    parser.add_argument('--output-dir', type=Path, required=True)
    args = parser.parse_args()
    evidence = args.evidence_dir.resolve(strict=True)
    reduction = args.reduction_dir.resolve(strict=True)
    out = args.output_dir.resolve()
    assert not out.exists(), 'Use a new classification directory.'
    run = read(evidence / 'run.json')
    source = run['source_head']
    raw = read(reduction / 'workspace-results.json')
    value = read(reduction / 'classified-failures-draft.json')
    lines = (evidence / 'workspace.log').read_text(errors='replace').splitlines()
    before = json.loads(gzip.decompress((evidence / 'source-before.json.gz').read_bytes()))
    assert before == json.loads(gzip.decompress((evidence / 'source-after.json.gz').read_bytes()))
    assert hashlib.sha256((evidence / 'workspace.log').read_bytes()).hexdigest() == run['workspace_log_sha256']
    assert source == value['source'] and raw['exit_code'] == run['exit_code']
    source_records = {}

    def source_excerpt(path, needle):
        data = subprocess.check_output(['git', 'show', source + ':' + path], cwd=root)
        assert hashlib.sha256(data).hexdigest() == before[path]['sha256']
        text = data.decode().splitlines()
        matches = [index for index, line in enumerate(text) if needle in line]
        assert len(matches) == 1, (path, needle, matches)
        start, end = max(0, matches[0] - 8), min(len(text), matches[0] + 9)
        key = path + ':' + str(matches[0] + 1)
        source_records[key] = {'path': path, 'source': source,
            'sha256': hashlib.sha256(data).hexdigest(), 'start_line': start + 1,
            'end_line': end, 'lines': [{'line': index + 1, 'text': text[index]}
                                      for index in range(start, end)]}
        return key

    reviewed_nested = set()
    for failure in value['failures']:
        identity = tuple(failure[key] for key in ['package', 'cargo_target', 'name'])
        diagnostic = '\n'.join(row['text'] for row in failure['raw_diagnostics'])
        package, target, name = identity
        explanation = None
        required = []
        path = None
        message = None
        classification = 'missing_live_or_artifact_input'
        if package == 'wamn-receiving-tests' and target == '--lib' and name.startswith('route_authentication_live::cluster::'):
            message = 'WAMN_RECEIVING_EVIDENCE_DIR must name a new absolute directory under repository docs/perf'
            required = ['WAMN_RECEIVING_EVIDENCE_DIR']
            path = 'apps/wamn_receiving/tests/route_authentication_live/cluster.rs'
            explanation = 'The required result directory is absent. The case refuses before its live setup starts.'
        elif package == 'wamn-wms-tests' and target == '--lib' and name.startswith('cluster::'):
            path = 'apps/wamn_wms/tests/cluster.rs'
            if 'the WMS cluster case requires a clean source tree' in diagnostic:
                message = 'the WMS cluster case requires a clean source tree'
                classification = 'setup_refusal'
                explanation = 'The source cleanliness check refuses before live setup. The captured source status remains part of this result.'
            else:
                message = 'set WAMN_WMS_EVIDENCE_DIR to a new directory under the main repository docs/perf'
                required = ['WAMN_WMS_EVIDENCE_DIR']
                explanation = 'The result directory is absent. Read-only preflight can run first, but the application work did not start.'
        elif identity == ('wamn-ctl', '--lib', 'event_advisories::tests::scoped_credentials_confine_management_delivery_and_monitoring'):
            message = 'set WAMN_NATIVE_C_NATS_BIN to the owned nats-server executable'
            required = ['WAMN_NATIVE_C_NATS_BIN']
            path = 'services/ctl/src/event_advisories.rs'
            explanation = 'The explicit native binary input is absent. No broker is started by this case.'
        elif identity == ('wamn-ctl', '--lib', 'event_advisories::tests::retained_broker_advisories_report_missing_source_payloads'):
            message = "set WAMN_NATIVE_C_NATS_URL to this proof's disposable event broker"
            required = ['WAMN_NATIVE_C_NATS_URL']
            path = 'services/ctl/src/event_advisories.rs'
            explanation = 'The explicit broker URL is absent. This case does not connect to the broker.'
        if message and message in diagnostic:
            failure.update(classification=classification, cause=message,
                required_inputs=required, execution_scope='setup_refused_before_live_work',
                reviewed_explanation=explanation, source_evidence=source_excerpt(path, message))
        if identity == ('wamn-execution-host', '--lib', 'router_driver::native_policy::tests::authenticated::native_authenticated_nested_authority_and_lifecycle'):
            starts = [index for index, line in enumerate(lines) if line.startswith('test ' + name + ' ...')]
            if len(starts) > 1:
                finish = next((index for index in range(starts[-1] + 1, len(lines))
                               if re.match(r'^test .+? \.\.\. ', lines[index])), len(lines))
                block = [{'log_line': index + 1, 'text': lines[index]} for index in range(starts[0], finish)]
                failure['retained_reducer_raw_diagnostics'] = failure['raw_diagnostics']
                failure['raw_diagnostics'] = block
                failure['diagnostic_start_log_line'] = starts[0] + 1
                failure['diagnostic_end_log_line'] = finish
                if any('set WAMN_NATIVE_B_AUTH_PG_URL' in row['text'] for row in block):
                    failure.update(classification='missing_live_or_artifact_input',
                        cause='WAMN_NATIVE_B_AUTH_PG_URL is absent. The nested authenticated test exits before live work.',
                        required_inputs=['WAMN_NATIVE_B_AUTH_PG_URL'], execution_scope='setup_refused_before_live_work',
                        reviewed_explanation='Both same-named raw occurrences remain. The parent diagnostic states the absent database input.',
                        source_evidence=source_excerpt('crates/execution/host/src/router_driver/native_policy/tests/authenticated.rs',
                                                      'std::env::var(URL_ENV)'))
                    failure['source_evidence_keys'] = [failure['source_evidence'],
                        source_excerpt('crates/execution/host/src/router_driver/native_policy/tests/authenticated.rs',
                                       'const URL_ENV:')]
                    reviewed_nested.add(identity)
        if 'source_evidence' not in failure:
            for row in failure['raw_diagnostics']:
                match = re.search(r'panicked at (.+):(\d+):\d+:$', row['text'])
                if match:
                    failure['source_evidence'] = match[1] + ':' + match[2]
                    break

    indexed = {tuple(row[key] for key in ['package', 'cargo_target', 'name']): row
               for row in value['failures']}
    pending = []
    for review in value['manual_review_required']:
        identity = tuple(review['identity'])
        if review['kind'] == 'current_failure_cause' and indexed[identity]['classification'] != 'requires_current_cause_review':
            continue
        if review['kind'] == 'nested_case_diagnostic' and identity in reviewed_nested:
            continue
        if review.get('known_source_removal'):
            continue
        pending.append(review)
    value['review_records'] = value.pop('manual_review_required')
    value['pending_review'] = pending
    value['classification_counts'] = dict(Counter(row['classification'] for row in value['failures']))
    value['interpretation'] = 'All listed failures remain failures. Explicit skips did not execute their skipped work. Setup refusals are not application execution.'
    value['reviewed_source_excerpts'] = 'reviewed-source-excerpts.json'
    value['classification_complete'] = not pending and not raw['unresolved']
    out.mkdir(parents=True)
    write(out, 'classified-failures.json', value)
    write(out, 'reviewed-source-excerpts.json', source_records)
    write(out, 'explicit-self-skips.json', value['explicit_self_skips'])
    write(out, 'pending-review.json', {'result_differences': pending, 'parser_entries': raw['unresolved']})
    for filename in ['workspace-results.json', 'baseline-comparison.json', 'step1-comparison.json',
                     'step2-comparison.json', 'classification-inputs.json', 'test-case-delta-draft.json']:
        shutil.copyfile(reduction / filename, out / filename.replace('-draft', ''))
    previous = root / 'docs/perf/2026.09/consolidation-step2/run-003/known-test-removals.json'
    shutil.copyfile(previous, out / 'known-test-removals.json')
    counts = raw['counts']
    log_path = os.path.relpath(evidence / 'workspace.log', out)
    link = lambda number: f'[log {number}]({log_path}#L{number})'
    cell = lambda text: str(text).replace('|', '\\|').replace('\n', '<br>')
    report = f'''# Retained workspace test result

Source: `{source}`. The retained command returned {raw['exit_code']} after {run['elapsed_seconds']} seconds. [Full output]({log_path}).

The retained parser lists {len(value['failures'])} named test failures across {counts['test_failed_targets']} failed test targets. It reports {counts['test_reported_passed']} passing test results and {counts['doctest_reported_passed']} passing doctests. The passing test results include {value['explicit_self_skips']['count']} explicit skips. These skips did not execute their skipped work. Silent early returns can remain, so subtracting these skips does not give an exact execution count.

The same two schema-generation cases remain filtered. No live inputs were armed. Source bytes, modes, and HEAD remained unchanged. Compare [baseline](baseline-comparison.json), [step 1](step1-comparison.json), and [step 2](step2-comparison.json) by exact identity and cause. Removed or renamed cases are not passing cases.

There are {len(pending)} result differences and {len(raw['unresolved'])} parser entries that still need review. [Pending review](pending-review.json) retains every one. Setup refusals remain failed test results and establish no application outcome. This record supplies no native C or wave completion verdict.

| Classification | Failed tests |
| --- | ---: |
'''
    report += '\n'.join(f'| {cell(key.replace("_", " "))} | {count} |' for key, count in value['classification_counts'].items())
    report += '\n\n| Package / target | Failed case | Classification | Actual cause | Evidence |\n| --- | --- | --- | --- | --- |\n'
    for failure in value['failures']:
        report += '| ' + ' | '.join([cell(str(failure['package']) + ' / ' + str(failure['cargo_target'])),
            cell(failure['name']), cell(failure['classification'].replace('_', ' ')),
            cell(failure['cause']), link(failure['diagnostic_start_log_line'])]) + ' |\n'
    report += '\nEvery explicit skip follows. The retained reducer reports a lower bound. [Skip records](explicit-self-skips.json).\n\n| Target executable / description | Case | Explicit skip message | Evidence |\n| --- | --- | --- | --- |\n'
    for skip in value['explicit_self_skips']['entries']:
        executable = re.sub(r'-[0-9a-f]+$', '', Path(skip['target_executable']).name)
        report += '| ' + ' | '.join([cell(executable + ' / ' + skip['target_description']),
            cell(skip['name']), cell(skip['message']), link(skip['diagnostic_log_line'])]) + ' |\n'
    (out / 'report.md').write_text(report)
    summary = {'source': source, 'exit_code': raw['exit_code'], 'counts': counts,
        'classification_counts': value['classification_counts'], 'pending_review_entries': len(pending),
        'unresolved_parser_entries': raw['unresolved'], 'classification_complete': value['classification_complete'],
        'source_stable': True, 'tracked_files': len(before), 'log_sha256': run['workspace_log_sha256'],
        'report_sha256': hashlib.sha256((out / 'report.md').read_bytes()).hexdigest()}
    write(out, 'classification-summary.json', summary)
    print(json.dumps(summary, indent=2))


if __name__ == '__main__':
    main()
