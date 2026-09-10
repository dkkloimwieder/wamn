#!/usr/bin/env python3
"""Run the selector assertion against the original batching and restored tool."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--tree', type=Path, required=True)
parser.add_argument('--evidence', type=Path, required=True)
args = parser.parse_args()
tree = args.tree.resolve(strict=True)
evidence = args.evidence.resolve(strict=True)
assert json.loads((evidence / 'selectors-001/result.json').read_text())['exit_code'] == 0
for profile in ('m1', 'proof'):
    assert json.loads((evidence / (profile + '-build-001/result.json')).read_text())['exit_code'] == 0
out = evidence / 'mutation-001'
out.mkdir(exist_ok=False)
tool = tree / 'tools/build-components'
saved = tool.read_bytes()
original_stat = tool.stat()
base = json.loads((evidence / 'preparation-001/source.json').read_text())['base']
mutant = subprocess.check_output(['git', 'show', base + ':tools/build-components'], cwd=tree)
(out / 'batched-build-components').write_bytes(mutant)
(out / 'fixed-build-components').write_bytes(saved)
command = ['cargo', 'test', '--locked', '--offline', '-p', 'wamn-proof-conformance',
           '--test', 'profile_selectors', 'selector_tools_execute_exact_fake_cargo_argv',
           '--', '--include-ignored', '--exact', '--nocapture']
env = dict(os.environ, RUSTC_WRAPPER='', RUSTUP_TOOLCHAIN='1.98.0', CARGO_BUILD_JOBS='2')
env.pop('CARGO_TARGET_DIR', None)
def run(name):
    with (out / (name + '.stdout')).open('w') as output, (out / (name + '.stderr')).open('w') as errors:
        return subprocess.run(command, cwd=tree, env=env, stdout=output, stderr=errors).returncode
try:
    tool.write_bytes(mutant)
    mutated_status = run('mutated')
finally:
    tool.write_bytes(saved)
    os.utime(tool, ns=(original_stat.st_atime_ns, original_stat.st_mtime_ns))
restored_status = run('restored')
mutated_output = (out / 'mutated.stdout').read_text()
named_failure = 'selector_tools_execute_exact_fake_cargo_argv ... FAILED' in mutated_output
result = {'passed': mutated_status == 101 and named_failure and restored_status == 0,
          'mutated_exit_code': mutated_status, 'restored_exit_code': restored_status,
          'named_failure': named_failure, 'command': command, 'base': base,
          'restored_sha256': hashlib.sha256(tool.read_bytes()).hexdigest(),
          'source_restored': tool.read_bytes() == saved and tool.stat().st_mtime_ns == original_stat.st_mtime_ns}
result['passed'] = result['passed'] and result['source_restored']
(out / 'result.json').write_text(json.dumps(result, indent=2) + '\n')
print(json.dumps(result))
raise SystemExit(0 if result['passed'] else 1)
