#!/usr/bin/env python3
"""Exercise the telemetry namespace guard without contacting Kubernetes."""
import argparse
import contextlib
import hashlib
import io
import json
from pathlib import Path
import runpy
import subprocess
import sys

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--tree', type=Path, required=True)
parser.add_argument('--evidence-dir', type=Path, required=True)
args = parser.parse_args()
source = args.tree / 'tools/journey-telemetry-proof'
evidence = args.evidence_dir
evidence.mkdir(parents=True, exist_ok=False)
configuration = evidence / 'unused-kubeconfig'
configuration.write_text('No Kubernetes connection is permitted in this fixture.\n')
module = runpy.run_path(str(source), run_name='telemetry_scope_fixture')
cases = [
    ('legacy', 'kind-wamn-receiving-journey', 'wamn-receiving-journey', True),
    ('postcommit', 'kind-wamn-receiving-postcommit', 'wamn-receiving-postcommit', True),
    ('crossed-legacy', 'kind-wamn-receiving-journey', 'wamn-receiving-postcommit', False),
    ('crossed-postcommit', 'kind-wamn-receiving-postcommit', 'wamn-receiving-journey', False),
    ('frozen', 'kind-wamn', 'wamn-system', False),
    ('unnamed', 'kind-other', 'other', False),
]


class ReadIntercepted(Exception):
    pass


results = []
original_run = subprocess.run
original_argv = sys.argv
try:
    for name, context, namespace, admitted in cases:
        calls = []

        def intercept(command, **kwargs):
            calls.append(command)
            raise ReadIntercepted()

        subprocess.run = intercept
        output = evidence / name
        sys.argv = [str(source), '--kubeconfig', str(configuration), '--context', context,
                    '--evidence-dir', str(output), '--source', '1' * 40,
                    '--tenant', 'acme', '--project', 'receiving', '--environment', 'dev',
                    '--namespace', namespace, '--system-namespace', 'wamn-system',
                    '--update-trace', '2' * 32, '--receipt-trace', '3' * 32]
        errors = io.StringIO()
        refused = None
        with contextlib.redirect_stderr(errors):
            try:
                module['main']()
            except ReadIntercepted:
                assert admitted
            except SystemExit as error:
                refused = error.code
        if admitted:
            assert len(calls) == 1 and refused is None
            assert calls[0][:7] == ['kubectl', '--kubeconfig', str(configuration),
                                   '--context', context, '--request-timeout=10s', 'get']
            assert calls[0][7] == '--raw'
        else:
            assert refused == 2 and not calls and not output.exists()
            assert 'only reads a named disposable Receiving journey' in errors.getvalue()
        results.append({'case': name, 'context': context, 'namespace': namespace,
                        'admitted_to_intercepted_read': admitted,
                        'refusal_exit_code': refused, 'external_commands_executed': 0,
                        'verdict': 'pass'})
finally:
    subprocess.run = original_run
    sys.argv = original_argv
(evidence / 'result.json').write_text(json.dumps({
    'scope': 'Argument guard only. The first read is intercepted; no telemetry collection or Kubernetes access occurs.',
    'source_sha256': hashlib.sha256(source.read_bytes()).hexdigest(),
    'cases': results, 'verdict': 'pass'}, indent=2) + '\n')
print('Two named pairs admitted; four forbidden pairs refused; zero external commands.')
