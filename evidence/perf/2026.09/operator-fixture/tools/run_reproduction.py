#!/usr/bin/env python3
"""Run the current operator fixture beside real concurrent process launches."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--tree', type=Path, required=True)
parser.add_argument('--evidence-dir', type=Path, required=True)
args = parser.parse_args()
tree = args.tree.resolve(strict=True)
evidence = args.evidence_dir.resolve()
evidence.mkdir(parents=True, exist_ok=False)
source = tree / 'services/ctl/src/dev/operator.rs'
text = source.read_text()
start = text.index('    struct Fixture {')
end = text.index('    fn spec(', start)
fixture = text[start:end]
# Keep the actual script method and omit unrelated fixture allocation.
new_start = fixture.index('        fn new()')
script_start = fixture.index('        fn script(', new_start)
fixture = fixture[:new_start] + fixture[script_start:]
build = tree / 'target/operator-fixture-reproduction'
build.mkdir(parents=True, exist_ok=True)
(build / 'fixture.rs').write_text(fixture)
shutil.copy2(Path(__file__).with_name('reproduce.rs'), build / 'main.rs')
result = {'source_sha256': hashlib.sha256(source.read_bytes()).hexdigest(),
          'fixture_sha256': hashlib.sha256(fixture.encode()).hexdigest()}
(evidence / 'fixture.rs').write_text(fixture)
with (evidence / 'compile.log').open('w') as output:
    command = ['rustc', '--edition=2024', str(build / 'main.rs'), '-o', str(build / 'reproduce')]
    result['compile_command'] = command
    result['compile_exit_code'] = subprocess.run(command, cwd=tree, stdout=output, stderr=subprocess.STDOUT).returncode
if result['compile_exit_code']:
    (evidence / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
    raise SystemExit(result['compile_exit_code'])
with tempfile.TemporaryDirectory(prefix='wamn-operator-race-') as scratch:
    command = [str(build / 'reproduce'), scratch]
    result['command'] = command
    before = time.monotonic()
    with (evidence / 'run.log').open('w') as output:
        result['exit_code'] = subprocess.run(command, cwd=tree, stdout=output, stderr=subprocess.STDOUT, timeout=90).returncode
    result['elapsed_seconds'] = round(time.monotonic() - before, 3)
result['cleanup_complete'] = not Path(scratch).exists()
(evidence / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(result))
raise SystemExit(result['exit_code'])
