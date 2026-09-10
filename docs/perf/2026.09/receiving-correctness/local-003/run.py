#!/usr/bin/env python3
"""Compile the explicit reproduction identities and run the pure Receiving tests."""
import datetime
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys

lane = Path('/home/kaalin/.cache/wamn-lanes/receiving-correctness-20260909')
evidence = Path(__file__).resolve().parent
command = ['timeout', '--signal=TERM', '--kill-after=30s', '600s',
           'cargo', 'test', '--locked', '--offline', '-p', 'wamn-proof-integration',
           '--test', 'receiving_command_histories_live', '--', '--nocapture']
inputs = ['tools/receiving-cluster-journey-run', 'Dockerfile', 'Cargo.lock', 'tests/integration/Cargo.toml',
          'tests/integration/tests/receiving_command_histories_live.rs',
          'tests/integration/tests/receiving_history/model.rs',
          'tests/integration/tests/receiving_history/database.rs']

def source():
    return {
        'head': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=lane, text=True).strip(),
        'status': subprocess.check_output(['git', 'status', '--porcelain'], cwd=lane, text=True),
        'sha256': {name: hashlib.sha256((lane / name).read_bytes()).hexdigest() for name in inputs},
    }

def save(name, value):
    (evidence / name).write_text(json.dumps(value, indent=2) + '\n')

environment = os.environ.copy()
environment['RUSTC_WRAPPER'] = ''
environment['KUBECONFIG'] = '/dev/null'
environment['CARGO_TARGET_DIR'] = str(lane / 'target')
save('source-before.json', source())
save('command.json', {'argv': command, 'cwd': str(lane),
                     'target': environment['CARGO_TARGET_DIR'], 'rustc_wrapper': '',
                     'kubeconfig': environment['KUBECONFIG']})
started = datetime.datetime.now(datetime.timezone.utc).isoformat()
with (evidence / 'test.log').open('xb') as log:
    result = subprocess.run(command, cwd=lane, env=environment, stdout=log, stderr=subprocess.STDOUT)
save('source-after.json', source())
save('run.json', {'started': started,
                  'finished': datetime.datetime.now(datetime.timezone.utc).isoformat(),
                  'exit_code': result.returncode})
(evidence / 'exit-code.txt').write_text(str(result.returncode) + '\n')
sys.exit(result.returncode)
