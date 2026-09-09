#!/usr/bin/env python3
"""Compare completed fresh-auth journeys with the retained reducers; no live calls."""
import argparse
import hashlib
import json
import math
from pathlib import Path
import re
import runpy
from statistics import median
import subprocess
import tempfile

METRICS = ('requests_per_second', 'p50_ms', 'p99_ms', 'host_cpu_ms_per_request',
           'host_cpu_cores', 'pg_cpu_cores', 'host_throttled_share', 'sample_window_seconds')
TRACE_PHASES = ('restart-first', 'steady', 'steady-2', 'steady-3', 'steady-4', 'steady-5',
                'human-1', 'human-2', 'human-3', 'human-4', 'human-5')
ALIASES = {'before': 'before-002', 'after': 'after-001'}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def spread(values):
    require(values and all(isinstance(v, (int, float)) and math.isfinite(v) for v in values),
            'missing or nonfinite repeated measurement')
    return dict(n=len(values), min=min(values), median=median(values), max=max(values))


def change(before, after):
    if before is None or after is None:
        return None
    require(math.isfinite(before) and math.isfinite(after), 'nonfinite comparison')
    return dict(baseline=before, candidate=after, delta=after-before,
                percent=None if before == 0 else 100 * (after-before) / before)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('reducers', 'before-journey', 'after-journey', 'output'):
        parser.add_argument('--' + name, required=True, type=Path)
    parser.add_argument('--before-source', default='dfa1c3187fe8cd671688a442b23106046e502cb6')
    parser.add_argument('--after-source', required=True)
    args = parser.parse_args()
    roots = {'before': args.before_journey.resolve(), 'after': args.after_journey.resolve()}
    require(roots['before'] != roots['after'], 'comparison requires two distinct journeys')
    require(all(root.name == 'journey' for root in roots.values()), 'inputs must be journey directories')
    require(not args.output.exists(), 'output directory must be new')
    expected = {'before': args.before_source, 'after': args.after_source}
    require(all(re.fullmatch('[0-9a-f]{40}', value) for value in expected.values()), 'full source commits required')
    inputs = {}

    def remember(path):
        path = path.resolve()
        data = path.read_bytes()
        inputs[str(path)] = hashlib.sha256(data).hexdigest()
        return data

    def read(path):
        return json.loads(remember(path))

    def jq(name, files):
        script = args.reducers / name
        remember(script)
        return json.loads(subprocess.check_output(['jq', '-n', '-f', str(script), *map(str, files)], text=True))

    reducer_path = args.reducers / 'reduce_load.py'
    remember(reducer_path)
    load = runpy.run_path(str(reducer_path))['reduce_runs'](roots['before'].parent, roots['after'].parent)
    for sample in load['samples']:
        require(sample['source'] == expected[sample['run']], 'load sample source differs from requested commit')
        for relative in sample['source_files'].values():
            remember(roots[sample['run']].parent / relative)
    raw_steps, knees, ratio_groups, completion = [], [], [], {}
    with tempfile.TemporaryDirectory(prefix='wamn-performance-aliases-') as temporary:
        stage = Path(temporary)
        summary_aliases, trace_aliases, original_paths = [], [], {}
        for run, journey in roots.items():
            exit_files = [p for p in (journey.parent/'exit-code.txt', journey.parent/'exit') if p.is_file()]
            require(exit_files and all(remember(p).strip() == b'0' for p in exit_files), 'completed exit 0 receipt required')
            cleanup = journey / 'cleanup.receipt'
            require('verdict=pass' in remember(cleanup).decode().split(), 'completed cleanup receipt required')
            completion[run] = dict(exits=list(map(str, exit_files)), cleanup=str(cleanup))
            # Only aliases change: jq reads the original bytes; source metadata is untouched.
            alias_root = stage / ALIASES[run]
            alias_root.mkdir()
            (alias_root / 'journey').symlink_to(journey, target_is_directory=True)
            for credential in ('service', 'human'):
                for repetition in (1, 2, 3):
                    directory = journey / 'throughput' / f'{credential}-{repetition}'
                    summary_path = directory / 'summary.json'
                    report = read(summary_path)
                    index = read(directory / 'index.json')
                    require(report['index'] == index and index['source'] == expected[run], 'summary/index/source mismatch')
                    require(index['duration_seconds'] == 10, 'expected existing ten-second protocol')
                    require([(x['layer'], x['driver'], x['expected_status']) for x in index['layers']] ==
                            [('route', 'oha', 200), ('nodb', 'oha', 404), ('pg', 'pgbench', None)], 'unexpected load drivers')
                    coordinates = [(r['layer'], r['concurrency']) for r in report['results']]
                    require(coordinates == [(layer, c) for layer in ('route', 'nodb', 'pg') for c in (1, 4, 8, 16, 32, 64)],
                            'summary requires the complete ordered layer/concurrency grid')
                    require([v['layer'] for v in report['verdicts']] == ['route', 'nodb', 'pg'], 'missing native knee/peak verdict')
                    summary_aliases.append(alias_root/'journey'/'throughput'/f'{credential}-{repetition}'/'summary.json')
                    for result, step in zip(report['results'], index['steps']):
                        require((result['layer'], result['concurrency']) == (step['layer'], step['concurrency']), 'step identity mismatch')
                        files = {key: str(directory/step[key]) for key in ('result', 'before', 'after', 'host_cpu_before', 'host_cpu_after', 'pg_cpu_before', 'pg_cpu_after')}
                        require(all(Path(p).is_file() for p in files.values()), 'missing raw step evidence')
                        cpu = {}
                        for owner in ('host', 'pg'):
                            edges = {edge: {key: int(value) for key, value in
                                     (line.split() for line in remember(Path(files[f'{owner}_cpu_{edge}'])).decode().splitlines())}
                                     for edge in ('before', 'after')}
                            require(edges['before'].keys() == edges['after'].keys(), 'CPU counter fields changed')
                            delta = {key: value-edges['before'][key] for key, value in edges['after'].items()}
                            require(all(value >= 0 for value in delta.values()), 'CPU counter reset during sample')
                            cpu[owner] = dict(**edges, delta=delta)
                        raw_steps.append(dict(run=run, credential=credential, repetition=repetition, source=expected[run],
                                              summary_file=str(summary_path), source_files=files, result=result, cpu_counters=cpu))
                    for verdict in report['verdicts']:
                        knees.append(dict(run=run, credential=credential, repetition=repetition,
                                          summary_file=str(summary_path), **verdict))
            traces = {}
            for phase in TRACE_PHASES:
                path = journey/f'trace-breakdown-{phase}.json'
                trace = read(path)
                require(trace['phase'] == phase and trace['verdict'] == 'pass', 'incomplete or wrong trace phase')
                traces[phase] = trace
                alias = alias_root/'journey'/path.name
                trace_aliases.append(alias)
                original_paths[str(alias)] = str(path)
            for group, phases in [('service-restart', TRACE_PHASES[:1]), ('service-warm', TRACE_PHASES[1:6]), ('human-warm', TRACE_PHASES[6:])]:
                samples = [dict(file=str(journey/f'trace-breakdown-{phase}.json'), **traces[phase]) for phase in phases]
                ratio_groups.append(dict(run=run, group=group, samples=samples,
                                         overhead_ratio=spread([s['overhead_ratio'] for s in samples])))
            receipt_path = journey/'overhead-ratio-steady.receipt'
            fields = dict(field.split('=', 1) for field in remember(receipt_path).decode().split())
            ratios = [traces[phase]['overhead_ratio'] for phase in TRACE_PHASES[1:6]]
            require(fields['samples'] == '5' and [float(v) for v in fields['ratios'].split(',')] == ratios
                    and float(fields['overhead_ratio']) == median(ratios), 'steady receipt/trace ratios differ')
            completion[run]['recorded_service_ratio_gate'] = dict(file=str(receipt_path), fields=fields)
        groups = jq('summarize.jq', summary_aliases)
        auth = jq('trace-summary.jq', sorted(trace_aliases))
        for group in auth:
            for sample in group['samples']:
                sample['file'] = original_paths[sample['file']]

    comparisons = []
    for before in (g for g in groups if g['phase'] == 'before'):
        key = {field: before[field] for field in ('credential', 'layer', 'concurrency')}
        after = next(g for g in groups if g['phase'] == 'after' and all(g[k] == v for k, v in key.items()))
        comparisons.append(dict(**key, median_changes={metric: change(
            None if before[metric] is None else before[metric]['median'],
            None if after[metric] is None else after[metric]['median']) for metric in METRICS}))
    resource_changes = []
    for before in (g for g in load['summary'] if g['run'] == 'before'):
        key = {field: before[field] for field in ('credential', 'layer', 'concurrency', 'edge')}
        after = next(g for g in load['summary'] if g['run'] == 'after' and all(g[k] == v for k, v in key.items()))
        resource_changes.append(dict(**key, median_changes={metric: change(values['median'], after['statistics'][metric]['median'])
                                                            for metric, values in before['statistics'].items()}))
    knee_comparisons = []
    for credential in ('service', 'human'):
        for layer in ('route', 'nodb', 'pg'):
            pair = {run: [v for v in knees if v['run'] == run and v['credential'] == credential and v['layer'] == layer]
                    for run in roots}
            knee_comparisons.append(dict(credential=credential, layer=layer, **pair,
                median_peak_rps_change=change(*[median([v['peak']['requests_per_second'] for v in pair[run]]) for run in roots])))
    ratio_changes = []
    for group in ('service-restart', 'service-warm', 'human-warm'):
        pair = {run: next(r for r in ratio_groups if r['run'] == run and r['group'] == group) for run in roots}
        ratio_changes.append(dict(group=group, median_change=change(pair['before']['overhead_ratio']['median'],
                                                                    pair['after']['overhead_ratio']['median'])))
    result = dict(schema='wamn-cutover-offline-comparison/v1', sources=expected,
        journey_paths={run: str(path) for run, path in roots.items()}, completion=completion,
        alias_mapping={ALIASES[run]: str(path) for run, path in roots.items()},
        throughput_groups=groups, throughput_median_changes=comparisons, throughput_samples=raw_steps,
        knee_peak_comparisons=knee_comparisons, authentication_groups=auth, trace_ratio_groups=ratio_groups, trace_ratio_median_changes=ratio_changes,
        resource_median_changes=resource_changes,
        recorded_totals={run: {field: sum(s['result'][field] for s in raw_steps if s['run'] == run)
                              for field in ('total_requests', 'errors', 'cut_off')} for run in roots},
        limits=['Descriptive comparison of whole WAMN cutovers, not an isolated upstream runtime effect or an acceptance verdict.',
                'Three sweeps share one host per source; requests and repetitions are not independent deployments.',
                'P50/P99 and knees/peaks are retained native report values. No request pooling or knee from a median curve.',
                'Deadline cutoffs remain separate from completed/failed requests and errors; total_requests excludes cutoffs.',
                'CPU covers the whole before/after sample window including scheduling/background work, not isolated request CPU.',
                'memory.current is sampled cgroup memory, not RSS or peak. Load includes the benchmark and other machine work.',
                'Service-warm gate is the recorded median of five ratios; human and restart ratios have no new gate.',
                'Resource limits, artifacts, chart/native controls, and competing load need owner assessment before attribution.'])
    args.output.mkdir(parents=True)
    for name, value in [('comparison.json', result), ('load-memory.json', load), ('consumed-inputs.json', inputs)]:
        (args.output/name).write_text(json.dumps(value, indent=2, allow_nan=False)+'\n')
    print(json.dumps(dict(output=str(args.output), groups=len(groups), steps=len(raw_steps), resource_samples=len(load['samples']))))


if __name__ == '__main__':
    main()
