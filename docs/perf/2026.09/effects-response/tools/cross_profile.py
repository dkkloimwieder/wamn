#!/usr/bin/env python3
"""Run the existing cross-profile guest digest test with separate build directories."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--evidence-dir', type=Path, required=True)
parser.add_argument('--app-directory', type=Path, action='append', required=True)
args = parser.parse_args()
tree = Path.cwd().resolve()
evidence = args.evidence_dir.resolve()
evidence.mkdir(parents=True, exist_ok=False)
scratch = tree / 'target' / 'effects-response-profiles-001'
scratch.mkdir(parents=True, exist_ok=False)
commands = []
statuses = []
for profile in ('app', 'proof'):
    env = os.environ.copy()
    env.update(CARGO_TARGET_DIR=str(scratch / profile), RUSTC_WRAPPER='')
    command = ['tools/build-components', 'build-only', profile]
    if profile == 'app':
        command.extend(str(path.resolve(strict=True)) for path in args.app_directory)
    with (evidence / f'{profile}.json').open('w') as plan:
        with (evidence / f'{profile}.log').open('w') as log:
            result = subprocess.run(command, env=env, stdout=plan, stderr=log)
    commands.append({'argv': command, 'CARGO_TARGET_DIR': env['CARGO_TARGET_DIR'],
                     'exit_code': result.returncode})
    statuses.append(result.returncode)
    print(f'{profile}: exit {result.returncode}', flush=True)
status = next((code for code in statuses if code), 0)
if status == 0:
    env = os.environ.copy()
    env.update(WAMN_DIGEST_PROFILE_APP_PLAN=str(evidence / 'app.json'),
               WAMN_DIGEST_PROFILE_PROOF_PLAN=str(evidence / 'proof.json'))
    command = ['cargo', 'test', '-p', 'wamn-proof-conformance', '--test',
               'guest_workspace_closure', '--locked', '--offline',
               'one_commit_built_under_two_profiles_yields_identical_guest_digests',
               '--', '--include-ignored', '--exact', '--nocapture']
    with (evidence / 'assertion.log').open('w') as log:
        result = subprocess.run(command, env=env, stdout=log, stderr=subprocess.STDOUT)
    status = result.returncode
    commands.append({'argv': command, 'WAMN_DIGEST_PROFILE_APP_PLAN': env['WAMN_DIGEST_PROFILE_APP_PLAN'],
                     'WAMN_DIGEST_PROFILE_PROOF_PLAN': env['WAMN_DIGEST_PROFILE_PROOF_PLAN'],
                     'exit_code': status})
    print(f'cross-profile assertion: exit {status}', flush=True)
(evidence / 'commands.json').write_text(json.dumps(commands, indent=2) + '\n')
(evidence / 'result.json').write_text(json.dumps({
    'head': subprocess.check_output(['git', 'rev-parse', 'HEAD']).decode().strip(),
    'harness_sha256': hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
    'exit_code': status, 'build_exit_codes': statuses,
    'limits': 'The existing raw guest digest comparison only; no deployed behavior proof.',
}, indent=2) + '\n')
raise SystemExit(status)
