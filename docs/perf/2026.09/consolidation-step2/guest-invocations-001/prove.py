#!/usr/bin/env python3
"""Exercise the Docker and palette commands without compiling guest code."""
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile

TREE = Path('/home/kaalin/.cache/wamn-lanes/consolidation-guest-home-20260911')
OUT = Path(__file__).resolve().parent
BASE = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=TREE, text=True).strip()
PATHS = ['Dockerfile', 'components/no-std/publish.sh']

def source(path, old):
    if old:
        return subprocess.check_output(['git', 'show', f'{BASE}:{path}'], cwd=TREE, text=True)
    return (TREE / path).read_text()

results = []
with tempfile.TemporaryDirectory(prefix='wamn-guest-invocations-') as temporary:
    scratch = Path(temporary)
    fake = scratch / 'bin'
    fake.mkdir()
    (fake / 'cargo').write_text('''#!/usr/bin/env python3
import json, os, sys
with open(os.environ['WAMN_ARGV_LOG'], 'a') as stream:
    stream.write(json.dumps(sys.argv[1:]) + '\\n')
''')
    (fake / 'install').write_text('#!/bin/sh\nexit 0\n')
    for path in fake.iterdir():
        path.chmod(0o755)
    for old in [True, False]:
        arm = 'original' if old else 'fixed'
        for owner, path in [('docker', PATHS[0]), ('palette', PATHS[1])]:
            content = source(path, old)
            log = OUT / f'{arm}-{owner}-argv.jsonl'
            assert not log.exists(), log
            env = dict(os.environ, PATH=str(fake) + ':' + os.environ['PATH'], WAMN_ARGV_LOG=str(log))
            if owner == 'docker':
                stage = content.split('FROM component-toolchain AS component-builder\n', 1)[1].split('\n# ----', 1)[0]
                command = re.sub(r'^RUN\s+|--mount=\S+\s*\\\n\s*', '', stage)
                argv = ['sh', '-eu', '-c', command]
                expected = ['http-route', 'materializer', 'busyloop', 'connection-http-standard', 'sockprobe']
            else:
                root = scratch / arm / 'tree'
                directory = root / 'components/no-std'
                directory.mkdir(parents=True)
                script = directory / 'publish.sh'
                script.write_text(content)
                for component in ['transform', 'http-request', 'label-render']:
                    (directory / component).mkdir()
                    (directory / component / 'declaration.json.in').write_bytes((TREE / 'components/no-std' / component / 'declaration.json.in').read_bytes())
                argv = ['sh', str(script), 'tenant', 'package', '1.0.0', '/package', '/artifacts', '/credentials', 'postgres://fixture', 'postgres://fixture']
                command = content
                expected = ['transform', 'http-request', 'label-render']
            process = subprocess.run(argv, cwd=TREE, env=env, text=True, capture_output=True)
            (OUT / f'{arm}-{owner}.stdout').write_text(process.stdout)
            (OUT / f'{arm}-{owner}.stderr').write_text(process.stderr)
            calls = [json.loads(line) for line in log.read_text().splitlines()]
            builds = [args for args in calls if 'build' in args]
            selections = [[args[i + 1] for i, value in enumerate(args) if value == '-p'] for args in builds]
            all_selected = [package for selection in selections for package in selection]
            passed = process.returncode == 0 and all_selected == expected and all(len(selection) == 1 for selection in selections)
            assert passed is (not old), (arm, owner, selections, process.stderr)
            results.append(dict(arm=arm, owner=owner, source_sha256=hashlib.sha256(content.encode()).hexdigest(), exit_code=process.returncode, cargo_calls=calls, selected_packages=selections, per_guest_rule_passed=passed))
            (OUT / f'{arm}-{owner}-command.sh').write_text(command)
result = dict(base=BASE, tree=str(TREE), passed=True, results=results, proof_limit='Fake Cargo proves command selection only. This test does not compile or compare guest artifacts.')
(OUT / 'result.json').write_text(json.dumps(result, indent=2, sort_keys=True) + '\n')
print(json.dumps({'passed': True, 'original_violations': 2, 'fixed_paths': 2}))
