#!/usr/bin/env python3
"""Compile and run the exact selector tests without the runtime dependency graph."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--tree', type=Path, required=True)
parser.add_argument('--evidence-dir', type=Path, required=True)
args = parser.parse_args()
tree = args.tree.resolve()
out = args.evidence_dir.resolve()
out.mkdir(parents=True, exist_ok=False)
build = tree / 'target/component-absence-proof'
build.mkdir(parents=True, exist_ok=True)
deps = tree / 'target/debug/deps'
records = []
env = os.environ.copy()
env['CARGO'] = shutil.which('cargo')
env['CARGO_MANIFEST_DIR'] = str(tree / 'tests/conformance')
scratch = build / 'scratch'
scratch.mkdir(exist_ok=False)
env['TMPDIR'] = str(scratch)


def run(label, command, expected=0):
    start = time.monotonic()
    with (out / f'{label}.log').open('w') as log:
        result = subprocess.run(command, cwd=tree, env=env,
                                stdout=log, stderr=subprocess.STDOUT)
    records.append({'label': label, 'command': list(map(str, command)),
                    'exit_code': result.returncode,
                    'wall_seconds': round(time.monotonic() - start, 3)})
    (out / 'commands.json').write_text(json.dumps(records, indent=2) + '\n')
    if result.returncode != expected:
        raise RuntimeError(f'{label} exited {result.returncode}, expected {expected}')


def serde_core_fingerprint(library_path):
    crate = library_path.stem.removeprefix('lib')
    fingerprint = deps.parent / '.fingerprint' / crate / f'lib-{crate.rsplit("-", 1)[0]}.json'
    document = json.loads(fingerprint.read_text())
    return next(dependency[-1] for dependency in document['deps'] if dependency[1] == 'serde_core')


def library(name, core=None):
    candidates = [path for path in deps.glob(f'lib{name}-*.rlib')
                  if core is None or serde_core_fingerprint(path) == core]
    if not candidates:
        raise RuntimeError(f'No cached {name} Rust library in {deps}')
    return max(candidates, key=lambda path: path.stat().st_mtime_ns)


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


tool = tree / 'tools/build-components'
original = tool.read_bytes()
source_paths = [tool, tree / 'tests/conformance/tests/profile_selectors.rs',
                tree / 'tests/conformance/src/package_inventory.rs']
before = {str(path.relative_to(tree)): sha(path) for path in source_paths}
passed = False
try:
    serde_json = library('serde_json')
    serde = library('serde', serde_core_fingerprint(serde_json))
    wrapper = build / 'lib.rs'
    wrapper.write_text('#[path = ' + json.dumps(str(source_paths[2]))
                       + ']\npub mod package_inventory;\n')
    common = ['rustc', '--edition=2024', '-L', f'dependency={deps}',
              '--extern', f'serde_json={serde_json}']
    facade = build / 'libwamn_proof_conformance.rlib'
    run('compile-helper', common + ['--crate-name', 'wamn_proof_conformance',
                                   '--crate-type', 'rlib', str(wrapper), '-o', str(facade)])
    binary = build / 'profile_selectors'
    run('compile-tests', common + ['--test', '--extern', f'serde={serde}',
                                   '--extern', f'wamn_proof_conformance={facade}',
                                   str(source_paths[1]), '-o', str(binary)])
    run('baseline', [str(binary), '--include-ignored', '--nocapture'])
    anchor = b'if [[ -n "$WAMN_ABSENT_PACKAGE_COMPONENTS" ]]; then'
    assert original.count(anchor) == 1
    tool.write_bytes(original.replace(anchor, b'if false && [[ -n "$WAMN_ABSENT_PACKAGE_COMPONENTS" ]]; then'))
    try:
        run('mutant', [str(binary), '--include-ignored', '--nocapture', '--exact',
                      'component_build_distinguishes_absent_package_crates_from_inventory_drift'],
            expected=101)
        failure = (out / 'mutant.log').read_text()
        assert 'component_build_distinguishes_absent_package_crates_from_inventory_drift ... FAILED' in failure
        assert 'component profile, canonical inventory, and locked metadata drifted' in failure
        assert 'package component crates are absent' in failure
    finally:
        tool.write_bytes(original)
    run('restored', [str(binary), '--include-ignored', '--nocapture', '--exact',
                     'component_build_distinguishes_absent_package_crates_from_inventory_drift'])
    assert before == {str(path.relative_to(tree)): sha(path) for path in source_paths}
    passed = True
finally:
    assert tool.read_bytes() == original
    shutil.rmtree(scratch)
    (out / 'result.json').write_text(json.dumps({
        'passed': passed,
        'scope': 'Exact profile_selectors.rs test file with the real package_inventory module; not the full conformance crate.',
        'source_head': subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=tree, text=True).strip(),
        'source_sha256': before,
        'source_restored': before == {str(path.relative_to(tree)): sha(path) for path in source_paths},
        'mutation': 'Disable the absent-component refusal before the original inventory drift guard.',
        'cached_libraries': {str(path): sha(path) for path in [serde, serde_json]},
        'commands': records,
    }, indent=2) + '\n')
print(json.dumps({'passed': passed, 'evidence_dir': str(out)}))
